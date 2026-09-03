# Fossil

Fossil is a distributed version control system written in C beginning in 2007 by D. Richard Hipp, the architect of SQLite, to manage SQLite itself. Its lineage is explicit in Fossil's own history document: content addressing and SQLite storage from Monotone, integrated wiki/ticket ambitions from Hipp's own CVSTrac (begun 2002). A self-hosting prototype was devised 2007-07-16; SQLite's documentation split onto it 2007-11-12 and SQLite's source followed 2009-08-11. It is not a historical artifact: 2.28 shipped 2026-03-11 and trunk tip is 2.29 (checkin d04ad10377, 2026-08-28). Fossil bundles version control with wiki, tickets, forum, chat, alerts, RBAC and a web UI in one stand-alone executable. For Lattice it is the load-bearing existence proof: an append-only, rebase-free VCS has carried a flagship project in production for nineteen years.

## 1. What did it get right?

### 1.1 Two tables of truth, and the boundary drawn in the source

Fossil's entire syncable global state is two SQL tables. From `src/schema.c` (artifact `602fca4d…`), the DDL and, twenty lines below it, an explicit divider:

```sql
CREATE TABLE blob(
  rid INTEGER PRIMARY KEY, rcvid INTEGER,
  size INTEGER,                   -- Size of content. -1 for a phantom.
  uuid TEXT UNIQUE NOT NULL, content BLOB,
  CHECK( length(uuid)>=40 AND rid>0 ));
CREATE TABLE delta(
  rid INTEGER PRIMARY KEY, srcid INTEGER NOT NULL REFERENCES blob);
CREATE INDEX delta_i1 ON delta(srcid);
-- ----------------------------------------------------------------
-- The BLOB and DELTA tables above hold the "global state" of a Fossil
-- project; the stuff that is normally exchanged during "sync".  The
-- "local state" of a repository is contained in the remaining tables.
-- ----------------------------------------------------------------
```

Everything else — timeline rows, filename indexes, parent/child edges, tag resolution, ticket state — is derived. `tech_overview.wiki` states the invariant: the metadata *"contains no new information — only information that has been extracted from the canonical artifacts and saved in a more useful form,"* and *"the entire metadata corpus can be recomputed from the canonical artifacts. That is what the `fossil rebuild` command does."* That is Lattice's derived-semantic-layer decision, proven across nineteen years of schema churn — with the transferable detail being that the boundary is a physical divider inside the schema source, not a paragraph in a design document. Lattice should place the equivalent divider in the store's schema module and gate it in CI.

### 1.2 Immutability is affordable, and the numbers show where it stops being affordable

From `stats.wiki` (last updated 2018-06-04), all six rows:

| Project | Artifacts | Check-ins | Span | Uncompressed | Repo | Ratio | Clone BW |
|---|---|---|---|---|---|---|---|
| SQLite | 77,492 | 20,686 | 18.02 yr | 5.6 GB | 70.0 MB | 80:1 | 51.1 MB |
| TCL | 161,991 | 23,146 | 20.19 yr | 8.0 GB | 222.0 MB | 36:1 | 150.5 MB |
| Fossil | 39,148 | 11,266 | 10.87 yr | 3.8 GB | 42.0 MB | 90:1 | 27.4 MB |
| SLT | 2,384 | 169 | 9.51 yr | 2.1 GB | 145.9 MB | 14:1 | 143.4 MB |
| TH3 | 12,406 | 3,718 | 9.69 yr | 544 MB | 18.0 MB | 30:1 | 14.7 MB |
| SQLite Docs | 8,752 | 2,783 | 10.56 yr | 349.9 MB | 16.3 MB | 21:1 | 13.57 MB |

`tech_overview.wiki` adds an independent SQLite figure dated 2020-02-08: over 7.1 GB of artifacts in under 97 MB (~74:1), median compressed blob 156 bytes against median uncompressed 45,312 bytes. `selfcheck.wiki` gives the storage shape: content files *"are stored in the repository as a tree. The leaves of the tree are stored as zlib-compressed BLOBs. Interior nodes are deltas from their descendants."*

SLT (SQL Logic Test) is the row Lattice should stare at, and Fossil diagnoses it itself: SLT *"consists of many large (megabyte-sized) SQL scripts that have one or maybe two edits each."* Clone bandwidth of 143.4 MB against a 145.9 MB repository is effectively zero saving. Note precisely what Fossil does and does not dedup: two byte-identical files collapse to one blob, because addressing is by content hash. What Fossil lacks is *sub-file* dedup — deltas are selected along a file's version chain, so a megabyte script edited twice stores near-megabyte units. Content-defined chunking is not a marginal gain over this; it attacks exactly the failure SLT exhibits, and SLT is the closest published measurement of the asset-heavy workload Lattice targets.

### 1.3 Verify-before-commit, and deliberate hash diversity

Before every transaction commit Fossil *"re-extracts the original content of all files that were written, recomputes the hash, and verifies that the recomputed hash still matches."* Independently, each manifest carries an R card (per `fileformat.wiki`, MD5 of all files in the check-in except the manifest itself) and a Z card (MD5 of all prior lines of the manifest), and the delta format carries a 32-bit checksum of its reconstruction target. The second hash is deliberate: *"these added checks use a different hash algorithm (MD5) in order to avoid common-mode failures in the hash algorithm implementation."* Fossil accepts a *"substantial performance cost"* because *"reliability is more important than raw speed."* Lattice's store is single-hash (BLAKE3); Fossil's argument is that a hash-implementation bug is a common-mode failure that content addressing cannot detect, because detector and detected share code.

### 1.4 Correction by accretion — and it is explicitly a read-path override

`fossil amend` replaces nothing. Per `fossil-v-git.wiki`, it works *"by adding a correction record to the repository that affects how later Fossil operations present the corrected data. The old information is still there in the repository, it is just overridden from the amendment point forward."* Mechanically that record is a control artifact of T cards (`fileformat.wiki`): `+` applies a tag to one artifact, `-` removes it, `*` propagates to descendants until a more recent tag of the same name. The `comment`, `user` and `date` tags override *"for display purposes"* only.

This is worth naming exactly: Fossil already ships a narrow, non-destructive presentation layer over an immutable record — a lens, in Lattice's vocabulary, restricted to per-checkin metadata. The documented idiom for disowning a bad check-in is `fossil amend abcd1234 --branch BOGUS --hide`; for a check-in with good descendants, `fossil merge --backout`. Deleting a wiki page or forum post appends an empty version. Lattice's "review status updatable by later attestation without rewriting checkpoints" is this mechanism, already load-bearing since 2007.

### 1.5 Shunning, in mechanism

The shun record is three columns, and its position in `schema.c` (line 155) is *below* the global-state divider (line ~94):

```sql
CREATE TABLE shun(
  uuid TEXT PRIMARY KEY,-- UUID of artifact to be shunned. Canonical form
  mtime DATE,           -- When added.  seconds since 1970
  scom TEXT             -- Optional text explaining why the shun occurred
) WITHOUT ROWID;
```

Shunning is two-phase: adding a hash inhibits push and pull immediately, but *"all shunned artifacts (but not the shunning list itself) are removed from the repository whenever the repository is reconstructed using the 'rebuild' command."* The removal itself, `shun_artifacts()` in `src/shun.c`, is the part Lattice should copy:

```c
db_multi_exec(
   "CREATE TEMP TABLE toshun(rid INTEGER PRIMARY KEY);"
   "INSERT INTO toshun SELECT rid FROM blob, shun WHERE blob.uuid=shun.uuid;");
db_prepare(&q, "SELECT rid FROM delta WHERE srcid IN toshun");
while( db_step(&q)==SQLITE_ROW ){
  int srcid = db_column_int(&q, 0);
  content_undelta(srcid);          /* materialize each dependent FIRST */
}
db_finalize(&q);
db_multi_exec(
   "DELETE FROM delta WHERE rid IN toshun;"
   "DELETE FROM blob  WHERE rid IN toshun;" /* ... */ );
```

Before deleting, Fossil walks `delta_i1` to find every artifact delta-encoded *against* the victim and fully materializes each one, so redaction never damages content that merely shared storage with it. Hipp asserts the invariant on the forum, hedged — *"(IIRC) the shun command makes sure the artifact being shunned is not used as the source of a delta"* — but the source above confirms it unhedged.

Propagation is out of band: shun rows ride the *configuration* channel, not the artifact channel, as `/shun $MTIME $UUID scom $VALUE` (`src/configure.c` line 374), merged last-writer-wins on mtime. Pushing a shun list requires Admin on the receiver.

### 1.6 Sync negotiation without a wants/haves handshake

A phantom is an artifact whose hash is known but whose content is not — `blob.size` negative (`-1` per the schema comment). Fossil creates one on receiving an `igot` card for an unheld artifact, or a file card referencing an absent delta source. Each round trip a repo sends `gimme` for its phantoms and `igot` for what it holds; cluster artifacts (one or more M cards of hashes plus a Z-card MD5), generated by the server *"if the number of entries in the unclustered table on the server is greater than 100,"* collapse advertisement cost. The clone-bandwidth column in §1.2 measures that this works.

## 2. Why didn't it win (or why is it niche)?

Fossil is not dying — it ships releases and hosts SQLite and TCL — but it is capped, largely on purpose, and the causes are overwhelmingly social.

**The stance is the ceiling.** Core contributor Stephan Beal, on the Fossil forum (2020-10-15): *"Becoming popular, widespread, or supplanting git has never been a stated fossil project goal."* `fossil-v-git.wiki` frames the whole comparison as cathedral versus bazaar and states that *"the SQLite project doesn't accept outside contributions from previously-unknown developers, but the Linux kernel does."* Fossil deliberately has no low-friction drive-by path; its answers are `bundle` and `patch`, which the docs concede *"require higher engagement than firing off a PR."* Autosync-by-default, synced branch names and no rebase all serve *"the specific designed-in goal of promoting SQLite's cathedral development model."* A tool optimised for a team who know each other by name will not be adopted by teams who do not.

**Network effects did the rest.** Stack Overflow's 2022 developer survey put Git at 93%; Stack Overflow's own write-up of the remainder names Subversion and Mercurial and does not mention Fossil at all. Warren Young, on the Fossil forum (2019-07-21): Git *"effectively has a global monopoly on DVCSes, and I don't see how you replace such a thing."*

**Bundling grew the surface rather than shrinking it, and made the bridge lossy.** Counting Fossil 2.29's live help index on 2026-09-03: **120 commands, 116 settings, 226 web pages**, plus 157 unsupported/testing commands. For calibration, Git 2.50.1 reports 170 commands via `git --list-cmds=main` (144 of them builtins) — so Fossil's porcelain is smaller than Git's but the same order of magnitude, and its *settings* surface is its own. Fossil began from a four-noun model — artifact, check-in, manifest, repository — and did not stay there.

Worse, the bundled objects are exactly what Git cannot represent, which forces interop one-way. `fossil git export` works; `fossil git import MIRROR`'s built-in help still reads literally **"TBD..."** at 2.29 on 2026-08-28. `mirrortogithub.md` states *"The mirroring is one-way. If you check in changes on GitHub, those changes will not be reabsorbed by Fossil,"* that *"there are technical problems that make a two-way mirror all but impossible,"* and therefore *"you cannot accept pull requests on GitHub."* That mirror shipped in version 2.9 (2019), twelve years in, and Fossil now runs an hourly cron job pushing to GitHub. The differentiator became the reason the escape hatch opens only one way.

**One genuinely technical cause:** the version-chain delta model degrades badly on large, rarely-edited binaries (SLT: 14:1, no clone saving). Monorepo and asset-heavy workloads were never served.

## 3. What will Lattice do differently, concretely?

### 3.1 The tombstone is global state; Fossil's shun record is not

Fossil's shun row sits below the global/local divider. It is not an artifact, has no hash, is unsigned, names no actor, and its `scom` reason is unschematised free text — so a peer cannot distinguish "lawfully redacted" from "missing because the sender is malicious or the disk rotted." Fossil concedes the deeper point in `fossil-v-git.wiki` (not, as often cited, in `shunning.wiki`): *"if shunning and purging were removed from Fossil, you could still remove artifacts from the repository with SQL DELETE statements; the repository database file is, after all, directly modifiable, being writable by your user. Where the Fossil philosophy really takes hold is in making it difficult to violate the integrity of the hash tree."* Immutability there is friction, not cryptography.

**Commitment (ADR-REDACT-01, `Tombstone` struct):** the tombstone is a first-class op-log entry in *global* (syncable) state with fields: redacted content hash, pre-image chunk-tree root, redactor Ed25519 public key, timestamp, reason code drawn from a closed enum, and an Ed25519 signature over all of the above. The Merkle parent's child slot is replaced by `H(tombstone)` rather than emptied, so the enclosing directory-tree root still verifies.

**Gate G-VERIFY-OFFLINE, blocking on `ltx verify`:** a peer that has *never* held the plaintext must verify a redacted checkpoint's Merkle root and enumerate who authorised each removal, with no network access. Test fixture: clone into a fresh store, assert `ltx verify --offline` exits 0 and `ltx trace --redactions` names the redactor key for every tombstone. Fossil cannot do this — Beal, on the forum, describes what Fossil does instead: shunning *"breaks it when it removes records which are referenced by other records. e.g., shunning a file semantically breaks any commit which refers to it and any commits which derive from those commits."* That is the gap, stated by a Fossil maintainer.

### 3.2 Steal `content_undelta` — then notice that CDC makes it far harder

`shun.c`'s undelta-before-delete pass is the right invariant and Lattice must implement it at chunk level: before deleting a redacted chunk, re-materialize every non-redacted chunk-tree that references it.

But Fossil's fan-out is tiny. `delta(rid, srcid)` is essentially one version chain per path, so a blob's dependents number in the ones. Lattice pushes **all** content through content-defined chunking with global dedup, so one chunk may be referenced by an unbounded number of chunk-trees across unrelated files.

**Constraint on ADR-STORE-01 (store-backend benchmark):** the backend **must** maintain a persistent `chunk_hash → referencing_chunk_tree` reverse index, and the redaction path **must** issue a pre-flight fan-out query against it. redb/RocksDB give this cheaply as a second keyspace; custom append-only packs do not, and "scan the packs" is an O(repository) answer to what users will read as an O(file) operation. The benchmark must therefore measure reverse-index maintenance cost on write, not only read throughput — otherwise the benchmark will pick the backend that cannot ship the feature.

### 3.3 Provenance in the signed artifact — with an explicit erasure carve-out

Fossil has provenance-lite: `rcvfrom(rcvid, uid, mtime, nonce, ipaddr)` records who pushed each artifact, when, and from which IP. But `rcvfrom` is at `schema.c` line 104, *below* the divider — it does not sync. `blockchain.md` confirms the consequence: *"Commit source info isn't transmitted from the remote server on clone or pull: the size of the rcvfrom table after initial clone is 1, containing only the remote server's IP address."* Fossil's provenance dies at the clone boundary.

**Commitment (ADR-PROV-01):** Lattice provenance is a signed field of the checkpoint artifact in global state; only the query index is derived.

**Gate G-PROV-REBUILD:** clone to a peer that has never seen the repo, delete the provenance index, run `ltx internals reindex`, and require `ltx trace --agent --unreviewed --touching src/auth/` to return byte-identical output to the origin. Fossil's `rebuild` invariant applied to the agent layer, as a mechanical test rather than an aspiration.

**But ADR-PROV-01 and ADR-REDACT-01 collide, and the ADRs must say so.** Fossil can honour an identity-erasure request cheaply *because* provenance is local: `fossil scrub --verily` removes *"concealed email addresses, IP addresses of correspondents, and similar privacy-sensitive fields"* without touching the DAG, and the docs are explicit that *"in the DAG, commits by 'bertina' will continue to be visible unchanged."* Moving actor identity into the signed global artifact removes that cheap path: under GDPR, erasing an actor's identity would require a tombstone against every checkpoint they authored. ADR-PROV-01 must therefore split provenance into a signed **actor-key** field (global, never erasable, pseudonymous) and a **key→human-identity binding** held in local/side state that `ltx scrub` can erase — otherwise "provenance is global and signed" and "GDPR erasure is affordable" cannot both hold.

### 3.4 Do not repeat `auto-shun`

`src/db.c` line 4681 reads `SETTING: auto-shun boolean default=on`, documented as *"If enabled, automatically pull the shunning list from a server to which the client autosyncs."* `tech_overview.wiki` adds: *"The shun table is also copied during a clone."* Meanwhile `shunning.wiki` argues non-propagation is *"a security feature"* and concludes: *"By refusing to propagate the shunning list, Fossil ensures that no remote user will ever be able to remove information from your personal repositories without your permission."* That absolute claim and a default-on setting cannot both be true. On the forum, Andy Bradford describes the shipped behaviour: the server *"will also publish the shunned artifacts list to clients and they will remove it from their repositories automatically."* The docs describe the *upward* (push, Admin-gated) direction and generalise it to all directions; the default governs the *downward* one.

**Commitment (ADR-REDACT-01, sync path):** a tombstone arriving from a remote is **quarantined**. It suppresses *serving* the content and marks it redacted-pending, but does not destroy local bytes until an actor holding a key in the repo's local `trusted_redactors` set countersigns. Receipt and acceptance are two distinct op-log operation kinds (`TombstoneReceived`, `TombstoneAccepted`), so `ltx undo` reverses the local acceptance while the received tombstone stays on the record. No setting may make acceptance automatic; this is a design constraint, not a default.

### 3.5 Append-only storage does not buy you undo

This is the finding that most challenges the spec's framing. Fossil has had immutable storage for nineteen years and its undo is weaker than Git's reflog. From `fossil help undo`: *"A single level of undo/redo is supported. The undo/redo stack is cleared by the commit and check-out commands."* It covers exactly eight commands — `update`, `merge`, `revert`, `stash pop`, `stash apply`, `stash drop`, `stash goto`, `clean` — and `fossil clean` *"only saves state for files less than 10MiB in size."* The stack lives in the checkout database. Fossil's undo is a working-directory file-copy stack, not an operation log. (Its separate `purge` command, which moves artifacts to a recoverable "graveyard", carries the warning that it *"is a work-in-progress and may yet contain bugs"* and is described in `fossil-v-git.wiki` as *"experimental"*.)

Lattice's Merkle-linked op-log is genuinely novel against this baseline. But the reason Fossil never got there is the wall Lattice also faces: undo must cover operations mutating state *outside* the content-addressed store.

**Commitment (op-log schema) + Gate G-UNDO-TOTAL:** every op-log entry for a mutation of non-content repository state — settings, remote config, lens definitions, key material, review attestations — must record a pre-image hash of the affected state. The gate is a property test: for every operation kind in the `Operation` enum, apply it to a fixture repo, run `ltx undo`, and assert the full repository state hash returns to its pre-operation value. Any operation kind without an inverse fails the build. Without this, `ltx undo` acquires Fossil's whitelist problem under a new name, and "undo undoes everything" holds only over the content-addressed subset.

### 3.6 The Git bridge and the lens cannot both be lossless

Fossil is the strongest available evidence against a stated Lattice target. A simpler model, designed by an unusually capable engineer over nineteen years, produced a one-way mirror and a documented verdict that two-way is *"all but impossible."* The correspondence between the two histories had to be materialised in an out-of-band `.mirror_state` directory: *"Do not put those files under Git management. Do not edit or delete them."* The mapping is not derivable from content.

Lattice asserts three things that cannot all hold: (a) `ltx sync` speaks to Git remotes so plain-Git teammates notice nothing; (b) Lattice→Git published history is lens-defined; (c) no force-push concept exists. A lens is a non-injective projection, and lenses are *versioned*. When a pinned lens version changes, the generated Git commit graph changes shape, and updating the remote to match is a non-fast-forward update — a force-push, executed against a remote plain-Git teammates are pulling from. Denying the concept does not remove the operation; it removes the warning.

**Commitment (ADR-BRIDGE-01):** store the lens-output↔checkpoint correspondence durably *inside* the repository as op-log entries (Fossil's `.mirror_state`, done properly and versioned), and **pin one lens version per Git remote** in a `LensPin{remote, lens_id, lens_version}` record. Changing a pinned lens is a first-class, signed, logged operation kind (`LensRepin`) whose CLI output declares "this rewrites published Git history for remote X", with the same loudness the spec demands of redaction. `ltx sync` must refuse to push a shape change under an unchanged pin.

### Residual risk for Lattice

- **Redaction is not cheap under global dedup.** Cost scales with reference fan-out, not with the redacted file's size. A boilerplate chunk — a licence header, a common import block — could make one redaction re-materialize gigabytes. Fossil avoided this only by not deduplicating below file granularity; no prior system in this survey has solved it.
- **Local write access defeats every immutability guarantee.** Fossil says so outright. `.lattice/` is writable by its owner. Signed tombstones prove *a redaction happened*; nothing proves *a deletion did not*. Lattice must state its guarantee as "no checkpointed data is silently lost across an honest sync," not "no data loss."
- **Tombstone propagation is a denial-of-service surface.** Automatic effect hands a compromised remote the power to destroy content on every peer. Quarantine (§3.4) mitigates it but inserts a manual countersignature into exactly the GDPR/secret-leak workflow where speed is the point. That tension is unresolved, not solved.
- **Attestation ordering is a trust hole Fossil already has.** `fileformat.wiki`: *"When two or more tags with the same name are applied to the same artifact, the tag with the latest (most recent) date is used."* The date is the artifact's own self-asserted D card. If Lattice orders review attestations by their claimed timestamp, an actor downgrades or upgrades a review status by claiming a later date. Attestations must be ordered by op-log position (Merkle-linked, tamper-evident), never by wall-clock field.
- **Structural verification is not semantic survival.** `H(tombstone)` keeps the Merkle root verifiable, but Beal's point stands: removing content still *"semantically breaks any commit which refers to it and any commits which derive from those commits."* Lattice needs a defined degraded-read behaviour for a checkpoint containing a tombstone — what `ltx diff` and structural merge do — or it ships a verifiable tree that no command can usefully read.
- **The seven-noun budget will erode without an enforced gate.** Fossil began with four nouns and ships 120 commands and 116 settings. `ltx internals` is structurally the same escape hatch as Fossil's 157 testing commands — and Fossil kept that split and still grew its porcelain. Propose gate G-NOUN-BUDGET: a CI check that fails if any string outside an allowlist of seven nouns appears in user-facing help text. A noun budget defended by intention is not defended.
- **Roadmap ordering.** The spec ships the Git bridge *before* lenses. If the bridge fixes published-history shape in v0, lenses inherit that as a compatibility constraint, and "lens-defined export" is retrofitted onto a shape chosen before the lens system existed.
- **Divergence has no guaranteed convergence.** `blockchain.md`: Fossil is *"an AP-mode system, which means there can be no guaranteed consensus on the content of the ledger at any given time,"* and *"you cannot guarantee that the command `fossil info tip` gives the same result everywhere."* Lattice's no-force-push P2P sync inherits this exactly: a signed retraction is itself just another artifact peers may or may not have pulled. "Converges by merge or a logged signed retraction" is an eventual-consistency claim, not a guarantee, and the docs must say so.

## Sources

- Fossil, "Fossil Versus Git" — cathedral/bazaar; "previously-unknown developers"; bundle/patch friction; the SQL-DELETE concession; amend as presentation override; purge called experimental — https://fossil-scm.org/home/doc/trunk/www/fossil-v-git.wiki [primary]
- Fossil, "Deleting Content From Fossil" (shunning) — two-phase removal via rebuild; "security feature"; "no remote user will ever be able to remove information…"; `amend --branch BOGUS --hide`; `merge --backout`; scrub carve-out — https://fossil-scm.org/home/doc/trunk/www/shunning.wiki [primary]
- Fossil, "Fossil File Format" — R card, Z card, T card `+`/`-`/`*` semantics, latest-date tag resolution, cluster M cards — https://fossil-scm.org/home/doc/trunk/www/fileformat.wiki [primary]
- Fossil, "Fossil Technical Overview" — derived-metadata invariant; 2020-02-08 SQLite figures (7.1 GB → 97 MB, 74:1, medians 156 B / 45,312 B); "The shun table is also copied during a clone" — https://fossil-scm.org/home/doc/trunk/www/tech_overview.wiki [primary]
- Fossil, "Repository Integrity Self-Checks" — re-extract-and-verify; MD5 common-mode rationale; "reliability is more important than raw speed"; delta-tree leaf/interior structure — https://fossil-scm.org/home/doc/trunk/www/selfcheck.wiki [primary]
- Fossil, "Performance Statistics" (2018-06-04, all six rows incl. SLT) — https://fossil-scm.org/home/doc/trunk/www/stats.wiki [primary]
- Fossil, "The Fossil Sync Protocol" — igot/gimme, phantoms, cluster generation above 100 unclustered entries — https://fossil-scm.org/home/doc/trunk/www/sync.wiki [primary]
- Fossil, "Is Fossil A Blockchain?" — AP-mode and `fossil info tip`; optional PGP signing; rcvfrom not transmitted on clone — https://fossil-scm.org/home/doc/trunk/www/blockchain.md [primary]
- Fossil, "The History And Purpose Of Fossil" — 2007-07-16 / 2007-11-12 / 2009-08-11; CVSTrac 2002; Monotone lineage — https://fossil-scm.org/home/doc/tip/www/history.md [primary]
- Fossil, "Mirroring A Fossil Repository On GitHub" — one-way; "all but impossible"; no PRs; `.mirror_state` warnings; 2.9; hourly cron — https://fossil-scm.org/home/doc/trunk/www/mirrortogithub.md [primary]
- Fossil, "Change Log" — 2.28 released 2026-03-11; 2.27 on 2025-09-30; 2.29 pending — https://fossil-scm.org/home/doc/trunk/www/changes.wiki [primary]
- Fossil source `src/schema.c`, artifact `602fca4d0af4cba3186a9bb01eb4ff160802f9ff501b2a018dce36f6692399e5` — blob/delta DDL and divider (l.94–98), rcvfrom (l.104), shun (l.155) — https://fossil-scm.org/home/raw/602fca4d0af4cba3186a9bb01eb4ff160802f9ff501b2a018dce36f6692399e5 [primary]
- Fossil source `src/shun.c`, artifact `29c9cf679b993b96ef7d087743a6d83eb9eb57289e716270097d4e733f85e69b` — `shun_artifacts()`, undelta-then-delete — https://fossil-scm.org/home/raw/29c9cf679b993b96ef7d087743a6d83eb9eb57289e716270097d4e733f85e69b [primary]
- Fossil source `src/configure.c` line 374 — `/shun $MTIME $UUID scom $VALUE` config wire format; `CONFIGSET_SHUN` — https://fossil-scm.org/home/doc/trunk/src/configure.c [primary]
- Fossil source `src/db.c` line 4681 — `SETTING: auto-shun boolean default=on`, "automatically pull the shunning list from a server to which the client autosyncs" — https://fossil-scm.org/home/doc/trunk/src/db.c [primary]
- Fossil command help at 2.29 (d04ad10377, 2026-08-28): `undo` (single level, 8 commands, 10MiB clean limit), `git` (import = "TBD..."), `purge` (graveyard/obliterate, "work-in-progress"), `scrub` ("--verily", irreversible), `amend` — https://fossil-scm.org/home/help?cmd=undo [primary]
- Fossil help index, counted 2026-09-03: 120 commands / 226 web pages / 116 settings / 157 testing — https://fossil-scm.org/home/help [primary]
- Fossil User Forum, "Shunning tag edits and the timeline" (2019-09-27→10-01) — Beal (#10) on shunning breaking referring commits; Bradford (#23.1) on client-side auto-removal — https://www.fossil-scm.org/forum/forumpost/c1ff250092 [primary]
- Fossil User Forum, "…unresolved deltas — the clone is probably incomplete and unusable" (2021-12-17/18) — Hipp (#25) on manual `DELETE` and the hedged shun/delta-source invariant — https://fossil-scm.org/forum/forumpost/f4cc31863179f843 [primary]
- Fossil User Forum, "Is Git irreplaceable?" — Beal (2020-10-15) on non-goals; Young (2019-07-21) on Git's monopoly — https://fossil-scm.org/forum/forumpost/2f63cce407d3d49c [primary]
- Stack Overflow Blog, "Beyond Git: the other version control systems developers use" (2023-01-09) — Git at 93% in the 2022 survey; Fossil not named — https://stackoverflow.blog/2023/01/09/beyond-git-the-other-version-control-systems-developers-use/ [secondary]
- Git 2.50.1 (Apple Git-155), measured locally 2026-09-03: `git --list-cmds=main` → 170; `--list-cmds=builtins` → 144 [primary, own measurement]
