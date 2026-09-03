# Lattice — revised specification

**Gate G0.8 · the original brief with the G0.2 resolutions merged.**

This is the authoritative specification. It records every place the original
brief was changed, why, and the challenge that caused it. Sections not listed
here are **unchanged** and the original text governs.

Nothing below weakens a gate. Three changes make a measurement **stricter** than
the original text; the rest correct a model, close a gap, or resolve an internal
contradiction. §0.4 forbids re-scoping a HARD gate downward, and none of these
does.

---

## §4.2 — The seven concepts (CONSTRAINT)

*Cause: Challenge 3. The original §4.2 caps user-facing nouns at seven, but
§4.4's own CLI table and §5.4's working model introduce `changeset` and
`conflict`, which would have made HARD gate G2.3 fail against the spec itself.*

**REVISED.** The seven nouns stand exactly as enumerated. Two terms that appeared
to be an eighth and ninth are folded in:

- **A changeset is not a new noun.** It is a **change** that has not been
  checkpointed yet. §4.2's "change" is already "a logical unit of work with a
  stable ID," which is exactly what a changeset is. Therefore `ltx assign`
  assigns hunks or entities to a *change*, and `ltx save <change>` checkpoints
  one. The partial-save capability the brief demands (§2, §5.4) is preserved
  intact; only the vocabulary collapses. A user learns one concept that behaves
  the same before and after it becomes durable.
- **A conflict is a state, not an object, in user-facing language.** The
  first-class conflict object specified in §5.4 is unchanged and is exactly
  right. But the surface says a *change* or a *checkpoint* **has conflicts**, the
  way a file has a size. The object is reachable at `ltx internals conflict …`
  for tooling.
- **Provenance, actor and attestation** are adjectival metadata on a checkpoint,
  never standalone nouns on a normal path. `ltx trace` answers "who wrote this,
  and was it reviewed"; its output labels are attributes.

**Consequence for ADR-11:** the naming ADR now covers *line*, *lens*, and the
core verbs. `changeset` is struck from its scope.

---

## §5.1 — `ltx verify` is two operations

*Cause: Challenge 4. §5.1 says verify "walks the full structure"; §5.6 makes
partial clone the v1 default. Both cannot hold, and the silent failure mode —
verify reporting success over the subset it happens to hold — undermines the
"no data loss" guarantee precisely where users would go to check it.*

**REVISED.**

- **`ltx verify`** (default; works on any clone) verifies the complete Merkle
  **spine** — the op-log hash chain, every checkpoint, every directory tree,
  every chunk-tree node — plus the content of every chunk present locally. It
  always reports coverage: *"verified 100% of history structure; content verified
  for 12,418 of 96,331 chunks (12.9%); 83,913 chunks not present locally."*
- **`ltx verify --complete`** fetches what is missing and verifies everything.
  This is the form G1.1's once-per-stage full verification uses, and the only
  form permitted to print an unqualified "verified".

The `--json` result carries `structure_verified`, `chunks_verified`,
`chunks_absent`, and `complete: bool`, so a caller cannot mistake one for the
other without ignoring a field named `complete`.

---

## §4.3 — Undo names its remote residue

*Cause: Challenge 8. §4.3 promises every state-changing command is undoable,
including `sync`. Locally that is true and G1.3 measures it honestly. But a
user's model — "undo puts things back" — is wrong for sync in the one case where
being wrong is most expensive: the content has already left the machine.*

**REVISED.** Undo of a sync remains fully undoable locally. Additionally, it
**must state its own boundary**: `ltx undo` of an operation with remote effects
prints, and includes in its JSON result, what was *not* undone — the peer that
received the data, the checkpoints it holds, and the commands that address remote
state (`ltx retract`, and `ltx redact` when content must be destroyed rather than
hidden).

Enforced mechanically: G2.4's machine-checked requirement (every error path
carries a recovery action, a causal category, and the §4.2 concept involved) is
extended to the undo result of any operation with remote effects. The `--json`
schema gains `remote_effects_not_undone`, present and empty for purely local
undos. A sync-undo that fails to name its residue fails a HARD gate.

---

## §1 and §5.5 — "audit-grade" is restated as verifiable attribution

*Cause: Challenge 6. §1 names "audit-grade provenance" as a target use; §5.5's
own threat model states that a malicious local actor can claim to be human. Both
are defensible; together they invite a team to adopt Lattice for a compliance
obligation it cannot discharge.*

**REVISED.** The user-facing claim is **"verifiable attribution and
tamper-evident review history."** "Audit-grade" is not used unqualified. Three
commitments, all already gated:

1. §5.5's metadata-honesty requirement becomes a hard display rule: a signed fact
   and a self-reported claim never render with equal visual weight, and the JSON
   contract types them as distinct fields (`verified` vs `asserted`) so no
   downstream tool can flatten them by accident.
2. `ltx trace` always carries per-record signature status; an unsigned record is
   labelled `unverified`, never left blank.
3. The threat model ships as user-facing documentation, and the G6.2 security kit
   is instructed to attack the gap between what the UI implies and what the
   signatures prove.

G5.2 already measures the mechanism — "forged actor claims: 100% detected **or
correctly surfaced as unverified**" — and this revision lives in that second
clause.

---

## §5.6 / ADR-5 — Redaction is advisory-with-explicit-acceptance, and authorised

*Cause: Challenge 7. G5.7 measures that redaction works; nothing measured what
happens when it is used as a weapon. Tombstones are designed to propagate and to
survive verification, which makes malicious redaction worse than the force-push
it replaces — a force-push is loud, local and recoverable from any peer that has
not fetched.*

**REVISED.**

1. **A tombstone arriving over sync is never silently applied.** It is
   quarantined: recorded in the op-log, surfaced by `ltx status`, and applied
   only on explicit local acceptance or a pre-configured policy naming
   authorised redactor keys.
2. **Redaction requires a committed, signed authorisation policy** listing the
   keys permitted to redact. A tombstone signed by a key outside the policy
   verifies as a *signature* but is rejected as an *authorisation*, and says so.
3. **G5.7's harness gains an adversarial population** — spurious tombstones from
   unauthorised keys, tombstone stripping, replayed tombstones from older op-log
   segments, and mass-redaction floods. The measured value counts failures across
   both honest and adversarial populations. **This is strictly stricter** than
   the original harness (§0.3).

**Stated limitation:** none of this defends against an *authorised* redactor
acting maliciously. That is governance, and §8 forbids inventing a
threshold-authorisation scheme. Lattice's answer is the permanent, unforgeable
audit trail. Documented in the threat model and handed to G6.2.

---

## §7 / ADR-4 — The daemon is an accelerator, never a requirement

*Cause: Challenge 5. ADR-4 asked "what degrades without it?" and no gate
answered. As written, Lattice could pass every latency gate with a resident
daemon and be unusable without one, entirely green.*

**REVISED.** Three commitments, specified in `docs/adr/adr-4-daemon.md`:

1. No command may be **unavailable** without the daemon.
2. Any command more than **5× slower** daemonless must say so in its own output,
   naming the daemon as the remedy — machine-checked by G2.4's extended check.
3. Every timing harness (G1.5, G1.6, G1.7, G4.6, G5.4) emits **both**
   `p95_daemon_resident` and `p95_daemonless`. **Pass/fail remains governed by
   the original target applied to the daemon-resident figure** — re-scoping a
   gate upward on our own authority is as forbidden as downward — but the
   daemonless number appears in every scorecard.

---

## §6 / G0.3 — The merge oracle reports three baselines

*Cause: Challenge 2, now empirically confirmed. For a clean auto-merge the tree
recorded in the merge commit **is** the line merge's output, because that is
literally how the human produced it. Measured: **93.77%** of 14,990 replayed real
merges reproduce byte-identically. A "line-based silent mis-merge rate" over that
population is therefore identically zero for a tautological reason, and G4.3's
"structural rate ≤ line baseline" would have collapsed to "exactly zero" —
strictly stronger than the 0.1% absolute beside it, and unsatisfiable.*

**REVISED.** `corpus/manifests/g0-3-oracle.md` (hash-pinned) defines three
reported baselines:

1. **Naive** — mis-merges among clean-and-matching cases. **Measured: 0.00%**,
   zero by construction, reported with that reasoning attached so the number is
   never mistaken for evidence that line merge is safe.
2. **Divergence** — clean replays whose tree differs from the recorded tree,
   after normalization and after excluding evil merges. **Measured: 0.1492%**
   (CI95 0.0976–0.2279, n = 14,078). This is the honest comparator and is what
   G4.3's "≤ line baseline" clause is evaluated against.
3. **Resolved-conflict** — merges where replay conflicts and the human resolution
   is genuine ground truth. **Measured line conflict rate: 5.6605%** (CI95
   5.3017–6.0420, n = 14,981). This is G4.4's denominator and the correct
   comparator for G4.3-b below.

Because baseline (2) is 0.1492%, **G4.3's 0.1% absolute target is the binding
constraint** — which is what makes the gate satisfiable at all.

---

## §6 / G4.3 — A second measurement, over the population that matters

*Cause: Challenge 1. G4.3's denominator admits a case only when line merge was
clean AND matched the human — the definition of an easy merge, where structural
merge has almost nothing to do. Meanwhile the dangerous population — conflicts
structural merge confidently auto-resolves, which G4.4 rewards Lattice for
growing — is excluded by the same clause.*

**REVISED.** G4.3 is measured twice; both are reported and both must be
satisfied:

- **G4.3-a** — exactly as originally specified. Unchanged, so the brief's
  criterion is honoured verbatim.
- **G4.3-b** — of the cases where structural merge auto-resolves a conflict line
  merge reported (the G4.4 numerator), the fraction semantically non-equivalent
  to the human result under the same normalization. Reported beside G4.4's rate
  in every scorecard.

G4.3-b carries **no numeric target**, because inventing one would substitute our
judgment for the stakeholder's. **Open question referred to the stakeholder**
(`STAKEHOLDER/001`): G4.3-b should probably become the primary safety gate with
G4.3-a retained as a regression check. Until that is decided, G4.4's mandated
resolution-vs-mis-merge curve carries the judgment.

---

## Unchanged, and worth saying so

Everything else stands. In particular: the CONSTRAINTs of §2 (preserve what Git
got right), §4.1's ten-minute scenario, §4.3's remaining UX invariants, §5.2's
bytes-are-truth correction, §5.6's Git-bridge-ships-first ordering, §8's
non-goals and the Rust/Apache-2.0 constraints, and every gate target in §6 not
named above. The Gauntlet protocol of §0 is untouched and governs this document
as it governs everything else.
