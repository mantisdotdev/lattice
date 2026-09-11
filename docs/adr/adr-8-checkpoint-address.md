# ADR-8 — A checkpoint's address is its identity

**Status:** Accepted · **Answers:** ADR-6's measured finding · **Amends:** ADR-3 (Decision, "Metadata: redb")
**Gates:** G1.4 (concurrency, HARD), G1.5/G1.6/G1.7 (latency) — all four were blocked by the defect below and this unblocks them; **none of the four is claimed here, and none has been run**

## Context

ADR-6 recorded an obligation and then discharged it: measure whether serialised
commands fit G1.4's real shape. They do not, and the reason turned out to be
none of the things the question was asked about.

The largest row of `bench/results/raw/adr6-scaling.json`:

```json
{
  "files": 10000,
  "first_save_s": 0.23,
  "incremental_save_s": 4.184,
  "status_s": 13.121
}
```

Saving ten thousand files for the first time costs 0.23 s. Saving again, having
changed exactly one of them, costs eighteen times that. `status` costs
fifty-seven times it — and `status` writes nothing, walks no tree and does not
so much as look at the working files. Whatever dominates is therefore not the
tree walk, not hashing, and not the repository lock. It is on the *read* path,
and it grows with the size of the store rather than the size of the work.

It is this, in `Repo::checkpoint`:

> a checkpoint is content-addressed like everything else, but its own address is
> over its body rather than its serialised form, so the lookup is by scanning
> the addresses we know ... a checkpoint index is a later refinement.

`Checkpoint::body_id` hashes `(tree, message, parent, at_unix_ms)`. The blob is
stored under `ChunkId::of(serialised Checkpoint)` — a different hash of a
different byte string. Nothing maps one to the other, so finding a checkpoint by
its id means reading, decompressing and attempting to deserialise **every chunk
in the store**. `status` does it twice.

The structure is a *deliberate* one, and the reason it exists is sound: folding
`oplog_seq` into the identity would force the blob to be rewritten once the
sequence is known, and trusting a blob's own `id` field would let any file whose
bytes deserialise as a checkpoint impersonate one. Both stay true here. What is
wrong is only that the two hashes were allowed to differ.

### The divergence this actually is

Calling it "a later refinement was owed" would be too kind, and the source note
quoted above says it in those terms. ADR-3 decided where checkpoints live, and
it did not put them here:

> **Metadata: redb.** The op-log, references, checkpoint graph, changesets, lens
> definitions and the provenance index live in a single embedded transactional
> store.

Its consequences even name the shape that follows: "a checkpoint in redb may
reference chunks in a pack." A checkpoint row keyed by id in a transactional
store is a direct lookup and always was. The implementation instead made the
checkpoint a chunk — and once it was a chunk whose address was not its id, a
scan was the only way left to find it.

So the seconds this document measures are the cost of a divergence from an
accepted decision, not of a refinement postponed. Recording that is the point of
having ADRs at all, and §2 has to answer for it: it ratifies the divergence
rather than reverting it, and owes a reason.

## Decision

### 1. Store the body, at the body's own address

A checkpoint blob becomes exactly the bytes `body_id` already hashes:
`serde_json::to_vec(&(tree, message, parent, at_unix_ms))`, written at
`ChunkId::of(those bytes)`. Identity and address stop being two things that must
be kept in agreement, and become one thing.

Then `checkpoint(id)` is `store.read(ChunkId::from_hex(id))` — one binary search
per pack and one decompression — and `checkpoints()` walks the op-log's `Save`
entries and reads each blob directly. Neither scans.

**No checkpoint id changes.** The blob is serialised as the same tuple, in the
same order, by the same serialiser, so every existing id still hashes from the
same bytes. `Operation::Save { checkpoint }`, every line's `tip`, and every
entry hash in the Merkle chain are untouched. This ADR moves bytes in the store
and nothing else.

### 2. Keeping checkpoints in packs, against ADR-3, and why

Two other fixes were available and both are rejected here.

**Put checkpoints in redb, as ADR-3 said.** This is the strongest of the three
on atomicity: a checkpoint row and the `Save` entry that references it would
land in one transaction, and the ordering invariant between the two stores would
have one less thing to carry. It is rejected for one forward-looking reason and
one present one. Partial clone (§5.6, G5.8) transfers packs, and a checkpoint
that is a chunk rides along in the transfer unit the design already has, where a
redb row would need a mechanism of its own — that argument is about a feature
nothing implements yet, and is marked as such. The present reason is that every
checkpoint in every existing repository is already a chunk, so this is the only
one of the three that is a data move rather than an addressing change, and it is
the one whose failure mode is losing content rather than losing speed.

**Add a second redb table beside `SAVED`**, mapping checkpoint id → chunk
address, written in the same transaction as the `Save`. Smaller to write, and no
migration for new saves. It loses because it *adds* a thing to keep true: the
blob would still carry an `id` field to be checked against what it hashes to,
`is_authentic` would still exist, and the index would be a third place recording
a fact the other two already imply.

§1 instead deletes both. The blob carries no `id` — its address is where it
lives — and `Store::read` already re-hashes every chunk it returns and refuses a
mismatch. Authenticity stops being a check a caller must remember to make and
becomes a property of having read the thing at all. A blob that lies about what
it is cannot be written, rather than being written and then caught.

The honest summary is that ADR-3's answer was better on atomicity and this one
is better on everything else, and that the deciding argument is that it is
reachable from where the repositories actually are.

### 3. Format 4 → 5, and it is a migration of content, not of entries

A format-4 repository has its checkpoint blobs at the old address, so the direct
read misses and the checkpoints appear to be gone. They are not, and the
migration says so: for each `Save` in the op-log, find the blob by the old scan
and write the body at its address.

Three properties make this the safe shape of migration:

- **It is additive.** Packs are append-only, so the old blob is never touched.
  An interrupted migration has written some bodies and not others, and rerunning
  it writes the rest; writing one twice is a no-op because the address is the
  content.
- **The version moves last**, in one transaction, exactly as ADR-7 §Consequences
  required after an interrupted line-state migration was found to be able to
  brick a repository. A crash before it leaves a format-4 repository that
  migrates again.
- **It needs no fallback path.** Once the version is 5 every checkpoint is
  readable by address, so `checkpoint` has one implementation rather than a fast
  path and a scan behind it. A fallback would be a second mechanism kept alive
  for a case that, after migration, cannot occur — and an untested one, because
  nothing would reach it.

`MIN_READABLE_FORMAT` stays 3. This break requires no repository to be
recreated, which is what ADR-17 §9's per-entry format tag was introduced to
guarantee, and this is the second migration to rely on it.

### 4. Damage met during the migration must not lock the user out

This is the first thing that ever makes `Repo::open` read content, and that is
a new way to fail. A chunk that does not read — a torn write, a bit flip, an
address its bytes no longer hash to — would fail the open, and every command
goes through the open. Including `verify`, which is the command a user runs to
find out what is wrong. A repository that cannot be opened cannot be diagnosed.

So an unreadable chunk is skipped and counted rather than propagated. What is
done with the count is the part that matters: if any checkpoint is still
unmigrated **and** some chunk could not be read, the version stays at 4. The two
cannot be told apart — identifying an unreadable chunk is precisely what reading
it would have done — so the missing checkpoint may be the damaged chunk, and
declaring the migration finished would leave a checkpoint permanently unreadable
that a refetch could have brought back. Holding the version back is what makes a
repaired store finish the migration on a later open.

Content that could come back is not a thing to trade for a faster open.

### 5. The orphaned blobs are inert, and `verify` must keep saying so

The old serialised-struct blobs remain in their packs after migration. They
still hash to their own addresses, so `verify` is right about them and stays
quiet. Nothing references them: `checkpoints()` now enumerates from the op-log,
so a blob no `Save` names is not a checkpoint and never was.

They are garbage in the precise sense — unreferenced content awaiting a
collector that does not exist yet — and they are the reason this ADR does not
claim to reduce store size. It reduces work, not bytes.

## Measured

Both arms are the same probe, on the same machine, in the same session: one
binary built at `0c70ca8` (this branch, before the change) and one after.
`bench/results/raw/adr6-scaling.json` was produced by an earlier version of the
probe, in which `status_s` was a single observation rather than a median, so it
is not the comparison used here.

At 10,000 files — `bench/results/raw/adr8-scaling-before.json`:

```json
{
  "files": 10000,
  "first_save_s": 0.242,
  "incremental_save_s": 3.445,
  "status_s": 8.361
}
```

and `bench/results/raw/adr8-scaling.json`:

```json
{
  "files": 10000,
  "first_save_s": 0.245,
  "incremental_save_s": 0.236,
  "status_s": 0.038
}
```

The first save is unchanged, which is the control: it always wrote its content
and never looked a checkpoint up, so nothing about it should have moved, and
nothing did.

The sharpest version of the result comes from the instrument that found the
defect. ADR-6 used `probe_scaling.py --attribute` to separate the commands that
answer from redb from the ones that go through `checkpoints()`, and at 10,000
files they were two orders of magnitude apart. Running the same mode afterwards
— `bench/results/raw/adr8-attribution.json`:

```json
{
  "files": 10000,
  "samples": 5,
  "internals_oplog_s": 0.035,
  "line_list_s": 0.035,
  "log_forensic_s": 0.039,
  "status_s": 0.038
}
```

The two groups have collapsed into one. `log --forensic` was 7.782 s and is
0.039 s; `status` was 11.524 s and is 0.038 s; the two commands that never
scanned are where they always were. Every read command now costs process startup
plus an indexed lookup, and nothing in the list can be told from anything else —
which is what it means for the scan to be gone rather than merely smaller.

`status` went from 8.361 s to 0.038 s and stopped growing: 0.035, 0.035, 0.038
at 1,000, 5,000 and 10,000 files.

**Read that number for what it is.** `Repo::status` reports the head
checkpoint, the checkpoint and operation counts, and the chunk and pack counts.
It does not compare the working tree against the tip, so a file edited after a
save changes nothing it prints. Its whole cost was the scan, which is why
removing the scan leaves a flat line — and why the flatness is evidence about
the read path rather than about a working-tree comparison nobody has written
yet.

The incremental save is the number to weigh, because a save does walk the tree:
0.236 s where it was 3.445 s, still growing, and growing for a reason.

Across the same three sizes an incremental save costs 0.073, 0.134 and 0.236 s.
That residue is the tree walk, and saying so is a measurement rather than an
inference. Against the same 10,000-file tree:

```console
$ find . -type f -not -path './.lattice/*' >/dev/null      # metadata only
real 0.01
$ find . -type f -not -path './.lattice/*' -exec cat {} + >/dev/null
real 0.17
$ ltx save "one file changed"
real 0.20
```

<!-- evidence: /usr/bin/time -p, median of three runs each, on the 10,000-file tree scripts/probe_scaling.py builds; the commands are quoted in full above and reproduce directly -->

Reading every file costs 0.17 s and the save costs 0.20 s, so almost all of what
remains is the bytes going through, and Lattice's own share above raw reading is
a few hundredths of a second. That is not a defect of the same kind: a save that
must notice which of 10,000 files changed has to look at 10,000 files.

It also sizes the next slice rather than gesturing at it. Walking the same tree
for metadata alone costs 0.01 s — seventeen times less than reading it — so a
working-tree index that reads only what the metadata says has changed has that
much headroom to work in. That is a separate slice and is not attempted here.

**G1.4 is closer and still does not fit.** The probe's own extrapolation falls
from 306 hours to 21, and 21 hours is not a budget anyone accepts either. What
changed is the character of what remains: the scan was accidental, and the tree
walk is work. The honest statement is that ADR-6's finding is answered and
G1.4's obstacle is now a different one.

## Consequences

- **`Checkpoint` loses two fields from the wire.** `id` and `oplog_seq` are no
  longer serialised; both are resolved when the blob is read — `id` from the
  address it was found at, `oplog_seq` from the op-log, as it always was. The
  in-memory struct keeps both, so no caller changes.
- **`is_authentic` is deleted.** The check it performed is now done by
  `Store::read` for every chunk in the repository, not just for checkpoints.
  The one place it survives is the migration, which reads pre-format-5 blobs
  that still carry a declared `id` — and must not hand a forgery the address the
  real checkpoint needs.
- **G1.4, G1.5, G1.6 and G1.7 are unblocked, and none of them is claimed.**
  ADR-6 named all four as sharing this scan, and the section above shows it
  gone. That is not a gate result: each of those gates measures a reference
  repository this checkout does not contain, and none of them has been run.
  `status` at 0.038 s against G1.5's 100 ms budget is not a pass, and is worth
  less than it looks for the reason given above.
- **The pack-count cost is untouched.** ADR-6 also names `retain_unknown`, whose
  cost grows with the number of packs rather than the number of blobs. It is a
  different fix and it is not in this slice.
- **The store gains no new index.** This is the point: the fastest lookup is the
  one whose data structure already existed.
- **ADR-3's Decision is amended, not quietly outgrown.** Checkpoints are content
  in packs, not rows in the metadata store. Its consequence — "a checkpoint in
  redb may reference chunks in a pack" — becomes "an op-log entry in redb
  references a checkpoint in a pack", which is the same ordering rule pointed at
  a different pair and needs no change to the invariant G1.1 attacks.

## Open conflicts recorded, not resolved

1. **Unreferenced pre-migration blobs accumulate.** A repository that migrates
   keeps one dead blob per checkpoint forever. Collection is a verb nothing
   implements, and ADR-10's ephemeral tier is the natural home for it.
2. **G1.5 times a command whose output nothing checks.** The harness runs
   `ltx status` on the reference repo and reports p95; it does not assert that
   the command says anything. Since `status` does not look at the working tree,
   the gate as frozen would be satisfied by a command that printed a constant.
   That is not an accusation against the harness — it measures the latency it
   was written to measure — but a latency budget on a command that has not yet
   been built to do the expensive thing is a budget met in advance, and it
   should be met again once it has. Making `status` a working-tree status is
   its own slice; §0.3 then governs whether G1.5's measurement has become
   stricter.
3. **A store that stays damaged re-scans on every open.** §4 holds the version
   back so a repaired store can finish, which means an unrepairable one never
   finishes and pays the scan every time. That is slow and it is loud, which is
   the right way round — but it is a real cost, and the thing that would remove
   it is a way to re-run a migration on demand rather than only at open.
4. **The migration's cost is the scan it abolishes.** One pass, once, at the
   first open after upgrade — seconds on the ten-thousand-file repository
   measured above. That is a one-time cost paid at an unpredictable moment
   rather than at an announced one, and no progress is reported while it runs.
