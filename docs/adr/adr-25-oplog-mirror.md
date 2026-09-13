# ADR-25 — The op-log mirrors itself append-only; the database is an index that can burn

**Status:** Accepted · **Amends:** ADR-3 (store backend) · **Required by:** gate G1.1 (crash & power-loss safety, HARD)
**Gates touched:** G1.1 — engine change; no harness change in this ADR

## Context

ADR-3 bought crash atomicity from redb: `meta.redb` holds the ENTRIES,
LINES, HEADS and SAVED tables, and its two-phase commit was the durability
story for the operation log — the metadata whose torn write G1.1 punishes
hardest, in ADR-3's own words.

G1.1's amended harness (ADR-24) measured the purchase failing: 34
power-loss trials left `meta.redb` in a state where redb 2.6.3 ABORTS ON
OPEN — `assertion left == right failed` in
`page_store/page_manager.rs:266`, a panic, not an error — with every one of
the repository's checkpoints unreachable behind it. The replay model was
fair: writes fsynced through the platform's real barrier were never torn.
The failing states are legal power-loss states. No 2.x release fixes it
(2.6.3 is the last), and 3.x/4.x are major bumps a fix must not smuggle in.
The 34-failure run's artifact was overwritten by the validation runs that
followed; its figures are recorded in the validation-progression comment on
this ADR's PR. The run that closes the loop is quotable, and quoted:

<!-- evidence: the G1.1 block of `GAUNTLET_HARNESS_TIMEOUT=14400 python3
scripts/gauntlet measure G1.1`, run 2026-09-13 against this branch merged
with ADR-24's amended harness, detail truncated to the fields quoted in
this ADR; the durable scorecard is the gauntlet iteration the results
branch records once this merges. -->
```json
  {
    "gate": "G1.1",
    "status": "PASS",
    "measured": null,
    "note": "0 failures over 2037 SIGKILL + 1000 power-loss injections",
    "detail": {
      "sigkill_trials_attempted": 2037,
      "sigkill_trials_injected": 2000,
      "powerloss_trials": 1000,
      "checkpoints_lost_total": 0,
      "critical_section_hits": {
        "store_write": 992,
        "compaction": 70,
        "thinning": 79,
        "sync": 67
      }
    }
  }
```

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

**Torn mirror tails truncate silently; mid-file damage refuses.** A
frame that runs past end-of-file, or fails inside the final claimed frame,
is a tail whose fsync never completed — never acknowledged, safe to drop.
A frame that fails with bytes still after it is different: accepting the
prefix would silently discard acknowledged history, so a rebuild REFUSES
it, and with a healthy database standing, the damaged mirror is set aside
and rewritten whole from the database instead. Frame sizes are bounded by
the file that holds them, not by an arbitrary cap a legitimately large
line-state snapshot could one day cross.

**Namespace changes are synced like data.** The quarantine rename is
directory-synced before a replacement exists under the old name; the
replacement and a newly created mirror are directory-synced before anyone
relies on their names; a mirror whose frames cannot be rolled back after a
failed database commit is renamed aside durably, so a later open writes a
fresh mirror from the database rather than resurrecting a refused batch.

**Only readers retry.** After a mid-command panic and repair, a command
that only reads runs again; a command that writes is never rerun by the
machinery — the record is written before the store, so the crashed
attempt's operation may already be durable, and rerunning could commit it
twice. The error says so and points at `ltx log`.

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
  already amortises fsyncs — the 25× figure is ADR-4's own recorded
  measurement, cited here, not remeasured — and the mirror rides the same
  batch.
- `.lattice` grows a file that scales with history, like the entries table
  it shadows. Compaction of the mirror is future work and is bounded by the
  same ADR-13 archive story as the table.
- Unit-tested: rebuild-from-mirror with the database replaced by garbage,
  torn-tail truncation, heal-forward from a hand-written ahead frame, and
  legacy migration then rebuild. The full-stack proof is G1.1 itself.
