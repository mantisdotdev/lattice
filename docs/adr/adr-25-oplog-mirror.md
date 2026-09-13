# ADR-25 — The op-log mirrors itself append-only; the database is an index that can burn

**Status:** Accepted · **Amends:** ADR-3 (store backend) · **Required by:** gate G1.1 (crash & power-loss safety, HARD)
**Gates touched:** G1.1 — engine change; no harness change in this ADR

## Context

ADR-3 bought crash atomicity from redb: `meta.redb` holds the ENTRIES,
LINES, HEADS and SAVED tables, and its two-phase commit was the durability
story for the operation log — the metadata whose torn write G1.1 punishes
hardest, in ADR-3's own words.

G1.1's amended harness (ADR-24) measured the purchase failing. Over 3,037
injections, 34 power-loss trials left `meta.redb` in a state where redb
2.6.3 ABORTS ON OPEN — `assertion left == right failed` in
`page_store/page_manager.rs:266`, a panic, not an error — with every one of
the repository's checkpoints unreachable behind it. The replay model was
fair: writes fsynced through the platform's real barrier were never torn.
The failing states are legal power-loss states. No 2.x release fixes it
(2.6.3 is the last), and 3.x/4.x are major bumps a fix must not smuggle in.

The deeper defect was architectural: the only queryable copy of the op-log
lived inside a third-party file format we cannot repair.

## Decision

**Every committed batch is appended to `.lattice/oplog.append` and fsynced
before the database commit.** A frame is exactly the `(entry, line-state
publish, format rung)` triple `commit_batch` writes to the tables —
length-prefixed, blake3-checksummed. `File::sync_all` is the barrier (the
platform's true one, F_FULLFSYNC on macOS, per ADR-18).

**A database that cannot open — or cannot survive first use — is
quarantined and rebuilt from the mirror.** Open runs under a panic guard
that also PROBES the database with one transaction over all four tables,
because redb refuses this class of damage by asserting, not erring, and at
two different moments: `page_manager.rs:266` during open, and
`page_manager.rs:243` (file shorter than the header's layout) only at first
use, where it would kill whatever command touched the database next. The
rebuilt database must pass the same probe. The damaged file is renamed
`meta.redb.corrupt-<millis>` — examined, never deleted — and the frames are
replayed through the same table-application code live commits use, in
chunks, so rebuild cost is streaming, not resident.

**A panic mid-command rebuilds and retries, once.** The probe catches
damage the open path can reach, but redb also asserts deep in commands
whose reads walk regions no cheap probe visits (`log --forensic` was the
witness). So the CLI runs every command under a panic guard: a panic
quarantines the index, rebuilds it from the mirror under the repository
lock, and runs the command a second time; a second panic is reported as
the corruption it is. The guard silences the default panic hook for the
attempt, so a recovered crash never sprays a backtrace over output the
JSON contract owns. A panic from a plain bug takes the same road — a
needless but harmless rebuild, then the same crash, reported with its
text — which errs on the side of the user's repository, not the
developer's backtrace.

**Torn mirror tails truncate silently.** A frame whose fsync did not
complete was never acknowledged to any caller; removing it loses nothing
anyone was promised.

**The mirror is authoritative when ahead.** Power lost between the mirror
fsync and the database commit heals forward on next open. A database commit
that FAILS (not crashes) rolls the mirror back out, so a batch whose caller
was told "no" cannot resurrect. Repositories from before this ADR grow a
mirror on first open; intermediate line-state snapshots no longer exist for
them, so the final migrated frame carries the current one, which reproduces
the tables exactly as they stand.

**No format bump.** Entry hashing, ids and the chain are untouched; the
mirror is infrastructure beside the entries, not a new shape for them.

## Consequences

- The op-log's home is now an append-only file in a format this repository
  owns end to end; redb remains the queryable index it should have been.
- Every commit pays one extra sequential write and one fsync. Group commit
  already amortises fsyncs (ADR-4's 25×); the mirror rides the same batch.
- `.lattice` grows a file that scales with history, like the entries table
  it shadows. Compaction of the mirror is future work and is bounded by the
  same ADR-13 archive story as the table.
- Unit-tested: rebuild-from-mirror with the database replaced by garbage,
  torn-tail truncation, heal-forward from a hand-written ahead frame, and
  legacy migration then rebuild. The full-stack proof is G1.1 itself.
