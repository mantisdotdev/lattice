# ADR-18 — G1.1's fault injector must see the barrier macOS actually uses

**Status:** Accepted · **Required by:** §0.3 (harness changes after freezing)
**Gates touched:** G1.1 (crash & power-loss safety, HARD)

## Context

G1.1 reported **1,683 failures**. The number that mattered was not 1,683 but
the ratio: 683 injected SIGKILL trials plus 1,000 power-loss trials is 1,683
trials, and every single one failed. A 100% failure rate is one systematic
fault, not a thousand independent bugs.

Two faults were stacked, and the first hid the second.

## Fault 1 — the gate could not read the engine's answer

`harness/g1/g1_1_crash_safety.py:124` evaluates the verify document as:

```python
bool(doc.get("complete")) and not doc.get("errors")
```

The CLI emitted `{"ok":…,"report":{"complete":…,"errors":…}}`. Nested,
`doc.get("complete")` is `None`, so **no trial could ever pass**, and
`doc.get("errors", [])` returned its `[]` default — which is exactly why every
recorded failure carried `"why": "[]"` alongside `checkpoints_lost: 0`. The
engine was not losing data; the gate could not see that it wasn't.

The harness is frozen, so the shape it reads is a contract the CLI owes it.
Fixed in the product, not the harness, and pinned by
`crates/ltx/tests/json_contract.rs`.

With that corrected the gate could finally measure: **726 failures**, of which
roughly 429 were `sync`, `internals compact` and `internals thin` — three of
the seven operations in the pool that do not exist yet — leaving ~297 real
failures, matching `checkpoints_lost_total: 299`.

## Fault 2 — the injector was blind to the only barrier macOS honours

The remaining failures were all the same shape: the baseline checkpoint
destroyed, `sections_hit: ["store_write"]`, and the reopened repository
reporting `DB corrupted: Failed to repair database. All roots are corrupted`.

Dumping a journal explained it. Across 23 records for one `start`, there was
**not one FSYNC or DIRSYNC record** — although `store.rs` calls `sync_all()` on
the pack and on the index, and `platform::sync_dir` fsyncs the directory.

`fsync(2)` on macOS does not flush the drive's write cache. ADR-4 measured the
difference on this machine — 63 µs versus 4,688 µs — and concluded that
"`fsync(2)` … is not crash-safe" and "is not available to us". The barrier that
does flush the media is `fcntl(fd, F_FULLFSYNC)`, and that is what Rust's
`File::sync_all` and `File::sync_data` compile to on macOS.

`iofault.c` interposed `write, pwrite, fsync, rename, ftruncate, unlink`. It did
not interpose `fcntl`. Measured directly:

| Call | Journalled (before) | Journalled (after) |
|---|---|---|
| C `fsync(fd)` | `WRITE, FSYNC` | `WRITE, FSYNC` |
| C `fcntl(fd, F_FULLFSYNC)` | `WRITE` | `WRITE, FSYNC` |
| Rust `File::sync_all()` | `WRITE` | `WRITE, FSYNC` |

So the replayer saw no barrier, marked **every** write volatile, then dropped
35% of them, shuffled the rest and tore one — onto a redb file mid-transaction.
The resulting corruption was attributed to the engine.

This is precisely the failure `replay.py`'s own contract forbids:

> a write is durable once an fsync/fdatasync on the SAME path follows it in the
> journal … That is exactly what fsync buys, and a replayer that violated it
> would fail the engine for a promise the platform never made — producing
> "bugs" nobody could fix.

## Decision

`iofault.c` interposes `fcntl` and records `F_FULLFSYNC` (and `F_BARRIERFSYNC`)
as a durability barrier, under the same two rules the `fsync` path already
applies: only a **successful** call counts, and a directory sync is journalled
as `OP_DIRSYNC` rather than `OP_FSYNC`, because the two confer different
guarantees.

The interposer is declared variadic, and must be: `fcntl`'s third argument is
passed variadically, which on arm64 arrives on the stack. A fixed
three-parameter signature would read a register the caller never set and
forward garbage for commands such as `F_SETFD`.

`harness/lib/iofault/iofault.c` and `harness/lib/iofault/replay.py` are added to
G1.1's frozen dependency set in `harness/FREEZE.json`. They were load-bearing
for the measurement while not being hash-pinned, so a change to either could
previously alter the result without tripping the freeze. That hole is closed
here rather than relied upon.

## Classification under §0.3: **more lenient than the frozen text, and correct**

Stated plainly, because the honest classification is the whole value of this
record: honouring a barrier the replayer previously ignored means fewer writes
are volatile, so the engine faces **less** mutilation than before. Measured
against the frozen text alone, this change makes G1.1 easier to pass.

It is nevertheless a correction rather than a weakening, and the argument is
not "the new number is nicer":

**Under the old shim, no correct engine could pass, and an incorrect one
could.** An engine using Rust's standard file API on macOS issues
`F_FULLFSYNC` and was recorded as issuing no barrier at all — every write
volatile, guaranteed corruption. An engine using raw `fsync(2)` would have been
recorded as fully durable and passed. But ADR-4 established that `fsync(2)` is
*not* crash-safe on this platform. The gate therefore rewarded the unsafe
implementation and punished the safe one — it was not a strict measurement, it
was an inverted one.

No threshold moved. The target is still 0 failures over ≥2,000 SIGKILL and
≥1,000 power-loss injections, and the §6 coverage contract is untouched. What
changed is the fidelity of the fault model: it no longer simulates a fault the
platform prevents.

Per §0.3, G1.1 reverts to `FAIL(stale)` and is re-measured; its ratchet baseline
is re-earned from the new run.

## What this does NOT excuse

G1.1 still does not pass, and this ADR does not claim it should:

- `sync`, `internals compact` and `internals thin` are three of the seven
  operations in the pool and do not exist, so those trials fail on
  "unrecognized subcommand" and the coverage contract records
  `compaction`, `thinning`, `merge` and `sync` as critical sections the
  injector never reached. G1.1 cannot pass until that surface is built. That is
  the harness-first discipline working as intended, not a defect.
- Any residual failure after this change is a real crash-safety finding and is
  to be treated as one.

## Measured effect

G1.1 itself could not be re-run to completion on this machine: it retains a
repository and a ~1.7 MB journal for each of ~4,500 trials, needing roughly
22–24 GB of scratch. Two attempts died with `ENOSPC` at power-loss trials 431
and 512 of 1,000, with the work directory growing ~1.6 GB/min to 16 GB. The
machine has 17 GB free, 26 GB of it held by `corpus/data`. Freeing that is a
decision for the corpus's owner, not a step to be taken to make a measurement
fit, so the gate remains unmeasured here and this is recorded rather than
rounded off.

`scripts/probe_powerloss.py` answers the narrower question the fix is about. It
runs the same power-loss sequence with per-trial cleanup, so disk stays flat.
It is a diagnostic and not a gate: it draws only the four implemented
operations and omits the SIGKILL half, both of which make it weaker than G1.1.

Identical seed, identical trials, the only variable being whether the injector
can see `F_FULLFSYNC`:

| | old shim | fixed shim |
|---|---|---|
| trials run | 60 | 60 |
| failures | **37** | **0** |
| checkpoints lost | 36 | 0 |
| trials where the replayer saw no barrier at all | **60 / 60** | 1 / 60 |

Two further seeds on the fixed shim, 80 trials each: 0 failures, 0 checkpoints
lost. 220 trials in total.

The "no barrier" row is the finding in one line. Under the old shim the
replayer never once observed a durability barrier, so every write in every
trial was volatile. Under the fixed shim that happens in a handful of trials —
legitimately, when the crash point falls before the first sync — and those
trials still pass, because redb recovers a torn uncommitted transaction when
the state beneath it was durable.

## Evidence

- `crates/ltx/tests/json_contract.rs` — pins the shape the frozen parser reads;
  restoring the nesting fails it.
- The three-way probe table above, reproducible with a six-line C file and a
  four-line Rust file under the shim.
- Single-trial reproduction before the fix: seeds 3, 5 and 6 of a `start`
  operation each reported `durable: 0` and left
  `DB corrupted: … All roots are corrupted`. After the fix the same seeds report
  `durable` of 6, 11 and 4, retain the baseline checkpoint, and verify clean.
