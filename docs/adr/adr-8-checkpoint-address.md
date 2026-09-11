# ADR-8 — A checkpoint's address is its identity

**Status:** Accepted · **Answers:** ADR-6's measured finding · **Amends:** ADR-3 (§ what a checkpoint blob holds)
**Gates:** G1.4 (concurrency, HARD), G1.5/G1.6/G1.7 (latency) — **all four blocked by the defect below, none claimed by this ADR**

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
changed exactly one of them, costs eighteen times that. `status`, which saves
nothing at all, costs fifty-seven times it. Whatever dominates is therefore not
the tree walk, not hashing, and not the repository lock — it is on the *read*
path, and it grows with the size of the store rather than the size of the work.

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

### 2. An index was the other option, and it loses

The obvious fix is a second redb table beside `SAVED`: checkpoint id → chunk
address, written in the same transaction as the `Save`, so the two can never
disagree. It is smaller to write and it needs no migration for new saves.

It loses because it *adds* a thing to keep true. The blob would still carry an
`id` field that has to be checked against what it hashes to, `is_authentic`
would still exist, and the index would be a third place recording a fact the
other two already imply. §1 instead deletes both: the blob carries no `id` —
its address is where it lives — and `Store::read` already re-hashes every chunk
it returns and refuses a mismatch. Authenticity stops being a check a caller
must remember to make and becomes a property of having read the thing at all.

A blob that lies about what it is cannot be written, rather than being written
and then caught.

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

### 4. The orphaned blobs are inert, and `verify` must keep saying so

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
That residue is the tree walk, and it is not a defect of the same kind: a save
that must notice which of 10,000 files changed has to look at 10,000 files.
Making it proportional to the *change* instead needs a working-tree index, which
is a separate slice and is not attempted here.

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
3. **The migration's cost is the scan it abolishes.** One pass, once, at the
   first open after upgrade — seconds on the ten-thousand-file repository
   measured above. That is a one-time cost paid at an unpredictable moment
   rather than at an announced one, and no progress is reported while it runs.
