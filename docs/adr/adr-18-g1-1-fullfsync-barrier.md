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

G1.1 still does not pass, and this ADR does not claim it should. The measured
run confirms both points rather than leaving them as assertions:

- `sync`, `internals compact` and `internals thin` are three of the seven
  operations in the pool and do not exist, so those trials fail on
  "unrecognized subcommand" and the coverage contract records `compaction`,
  `thinning`, `merge` and `sync` as critical sections the injector never
  reached. G1.1 cannot pass until that surface is built. That is the
  harness-first discipline working as intended, not a defect.
- Any residual failure after this change is a real crash-safety finding and is
  to be treated as one. None surfaced on this run, within the limits stated
  above: zero checkpoints lost across all 3,000 trials, and every retained
  failure reason an unimplemented command.

What this run does establish is narrower than "the engine is crash-safe", and
the difference matters. The validated scope is **2,000 SIGKILL injections and
573 power-loss injections**, not 1,000 of the latter: the other 427 power-loss
trials drew a command that does not exist and exited before any fault was
injected, so they measured nothing about crash safety. On the SIGKILL side the
same operations are excluded by a different route — an unrecognised subcommand
exits before reaching its byte milestone, so the trial is not counted as
injected and is retried, which is why 3,532 attempts were needed to land 2,000.
Across that scope, over `save`, `start`, `switch` and `undo`, nothing was lost.

The operations that do not exist have never been fault-injected, and the four
critical sections in the coverage note have never been entered.

`merge` among those four is a different case from `compaction`, `thinning` and
`sync`, and should not be folded in with them. Those three name commands that
trials drew and that failed because they do not exist. `merge` is not a command
in `OPERATION_POOL` at all — `CRITICAL_SECTIONS` (`g1_1_crash_safety.py:71`) is
a list of code regions the replayer reports as touched, not of CLI verbs. So
its section was never entered because nothing in the pool reaches it, and
nothing measured here establishes anything further about why.

## Measured effect

### The full gate, once there was disk for it

`bench/results/iteration-13.json`. The estimate above proved right: the work
directory peaked at 23 GB, which is why two earlier attempts died with `ENOSPC`
at power-loss trials 431 and 512 against 16–17 GB free.

```
value:  427 failures            status: FAIL
note:   coverage contract not satisfied: fault injector never hit critical
        section(s): compaction, thinning, merge, sync
sigkill_trials_attempted: 3532     sigkill_trials_injected: 2000
powerloss_trials: 1000             checkpoints_lost_total: 0
critical_section_hits: {store_write: 563, compaction: 0, thinning: 0,
                        merge: 0, sync: 0}
```

**Zero checkpoints lost across 3,000 fault injections.** That field is a
complete aggregate over every trial, not a sample, so it does establish its
claim: no trial lost a checkpoint. Every real failure in the previous run
carried `checkpoints_lost: 1`, so the failure class this ADR is about is
provably absent.

What the record does **not** establish is that all 427 failures were
unimplemented commands, and an earlier draft of this section claimed it did.
The harness stores `failures[:25]`; the other 402 reasons are not retained. All
25 that are retained are `unrecognized subcommand`, and three of the seven pool
operations do not exist, giving an expected 1000 × 3/7 = 428.6 against 427
observed — but that count is Binomial(1000, 3/7) with a standard deviation of
15.6, so the draws could plausibly have been as low as 397 and left ~30
failures from another cause. The arithmetic is consistent with the conclusion;
it does not force it.

Bounding what could hide there: it cannot be a checkpoint loss, since that
aggregate is 0 over all trials. It would have to be a verify failure that lost
nothing.

`scripts/probe_powerloss.py` measures that directly, because it classifies
**every** failure rather than the first 25. Run with G1.1's pool verbatim,
unimplemented operations included, over 300 power-loss trials:

```json
{"pool": "full (G1.1 verbatim)", "trials_run": 300,
 "failures": 130,
 "failure_kinds": {"unimplemented command": 130},
 "failures_by_operation": {"sync --dry-run": 44,
                           "internals compact": 42,
                           "internals thin": 44},
 "checkpoints_lost_total": 0,
 "trials_where_replayer_saw_no_barrier": 7}
```

130 of 300 against 300 × 3/7 = 128.6 expected, and **every one of the 130
classified**, not sampled: no verify failure and no checkpoint loss in any
trial.

`trials_where_replayer_saw_no_barrier: 7` counts trials in which no write in
the replayed prefix was covered by a barrier, so all of them were eligible for
dropping, reordering and tearing. What the artifact supports is that those
seven passed: a trial only reaches the replay step if its operation succeeded,
every recorded failure is an unimplemented command that returns before replay,
and `checkpoints_lost_total` is 0. It does not record where the crash point
fell relative to the first sync, and an earlier draft asserted that; the claim
is withdrawn rather than supported by inference.

The full output is committed at
`bench/probes/g1-1-powerloss-full-pool-300.json`, with the command that
produced it. This is a controlled sample rather than the gate's own 1,000, so
it supports the reading of the 427 without replacing it.

**Follow-up, not done here:** the honest fix is for G1.1 to retain a
machine-readable count of failure reasons instead of truncating at 25. That is
a change to a frozen harness and needs its own §0.3 record, so it is noted
rather than taken.

The SIGKILL half is silent for the same reason it needed 3,532 attempts to land
2,000 injections: an unrecognised subcommand exits before reaching its byte
milestone, so the trial is not counted as injected and is retried. All 2,000
injected SIGKILL trials and all 573 power-loss trials that drew an implemented
operation passed.

**The gate still FAILs, and correctly.** Not on its value but on §6's coverage
contract: the injector never reached compaction, thinning, merge or sync,
because none of them exists. G1.1 cannot pass until that surface is built. That
is the harness-first discipline doing its job — the gate refuses to certify
crash safety for code that has not been written.

### The controlled comparison that isolated the cause

The full run above shows the outcome; it does not by itself prove the cause,
because it changed alongside the JSON fix. `scripts/probe_powerloss.py` isolates
the variable. It runs the same power-loss sequence with per-trial cleanup, so
disk stays flat, and was run against both shims with everything else held
fixed. It is a diagnostic and not a gate: it draws only the four implemented
operations and omits the SIGKILL half, both of which make it weaker than G1.1.

Identical seed, identical trials, the only variable being whether the injector
can see `F_FULLFSYNC`:

| | pre-fix shim | fixed shim |
|---|---|---|
| trials run | 60 | 60 |
| failures | **34** | **0** |
| checkpoints lost | 34 | 0 |
| trials where the replayer saw no barrier at all | **60 / 60** | 1 / 60 |

Output committed at `bench/probes/g1-1-powerloss-ab-prefix-60.json` and
`bench/probes/g1-1-powerloss-ab-fixed-60.json`. The pre-fix arm is built from
`harness/lib/iofault/iofault.c` at `0f1f6c8^`, verified to contain no
`F_FULLFSYNC` handling.

**These trials are not bit-reproducible, and this record should say so rather
than present one run as fixed.** Op-log entries embed `SystemTime::now()`, so
the same RNG seed does not fix the bytes the replayer tears, and the pre-fix
failure count varies between runs: an earlier execution of this same comparison
gave 37 failures and 36 checkpoints lost. The fixed arm has been 0 on every
run.

Two further seeds on the fixed shim, so the comparison does not rest on one,
80 trials each over the implemented operations only:

| seed | trials | failures | checkpoints lost | no barrier seen |
|---|---|---|---|---|
| 777 | 80 | 0 | 0 | 6 |
| 31337 | 80 | 0 | 0 | 3 |

Output committed at `bench/probes/g1-1-powerloss-implemented-80-seed777.json`
and `bench/probes/g1-1-powerloss-implemented-80-seed31337.json`, each with the
command that produced it.

**220 fixed-shim trials** across the three seeds (60 + 80 + 80). Counting the
pre-fix arm as well, 280 trial executions are reported in this section.

The "no barrier" row is the finding in one line. Under the old shim the
replayer never once observed a durability barrier — 60 trials out of 60 — so
every write in every trial was volatile and eligible for dropping, reordering
and tearing. Under the fixed shim it falls to a handful, and those trials still
pass. Why the residue exists at all is not established by this data and is not
asserted here.

## Evidence

- `crates/ltx/tests/json_contract.rs` — pins the shape the frozen parser reads;
  restoring the nesting fails it.
- The three-way probe table above, reproducible with a six-line C file and a
  four-line Rust file under the shim.
- Single-trial reproduction before the fix: seeds 3, 5 and 6 of a `start`
  operation each reported `durable: 0` and left
  `DB corrupted: … All roots are corrupted`. After the fix the same seeds report
  `durable` of 6, 11 and 4, retain the baseline checkpoint, and verify clean.
