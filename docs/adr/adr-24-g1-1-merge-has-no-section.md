# ADR-24 — G1.1 stops requiring a critical section merge cannot have

**Status:** Accepted · **Required by:** §0.3 (harness changes after freezing) · **Amends:** the frozen `harness/g1/g1_1_crash_safety.py` and `harness/lib/iofault/replay.py`
**Gates touched:** G1.1 (crash & power-loss safety, HARD)

## Context

Iteration 18 (2026-09-13) measured G1.1 at 0 failures over 2,000 SIGKILL and
1,000 torn-write trials — and FAILed anyway: "fault injector never hit
critical section(s): merge." The coverage contract required at least one
injected fault inside a `merge` section, where a section is derived from the
path being written (`/merge`, `.merge`, `/conflict`).

<!-- evidence: the G1.1 block of iteration 18's recorded artifact on the
results branch (commit "Record iterations 18 and 19: G2.4 passes; G1.1 fails
its own coverage gate", 2026-09-13), detail truncated to the fields quoted
here; the artifact merges to main with that branch's results PR. -->
```json
  {
    "gate": "G1.1",
    "status": "FAIL",
    "measured": null,
    "note": "coverage contract not satisfied: fault injector never hit critical section(s): merge",
    "detail": {
      "sigkill_trials_attempted": 2005,
      "sigkill_trials_injected": 2000,
      "powerloss_trials": 1000,
      "checkpoints_lost_total": 0,
      "critical_section_hits": {
        "store_write": 994,
        "compaction": 113,
        "thinning": 82,
        "merge": 0,
        "sync": 92
      }
    }
  }
```

No such path exists to hit. The engine deliberately keeps merge state inside
the op-log entry itself: the tip move and the parked target tree go out in
one publish, so a crash before the working files are written leaves a merge
the next command finishes (ADR-16 §6, `repo.rs::merge_line`). There are no
merge-named scratch files, and there is nothing wrong with that — it is the
crash-safety design working. The section requirement encoded a speculative
engine shape ("merge will write merge files") rather than the engine.

Separately and worse, the operation pool never ran `merge` at all, so
crashes were never landed inside merge's capture–publish–materialise window
— the one place its crash safety could actually be probed.

## Decision

- `CRITICAL_SECTIONS` drops `merge`: a merge's durable writes are op-log and
  store writes, which the `store_write` section already covers. Requiring a
  path the design refuses to create made the gate unpassable by design
  rather than by defect.
- `["merge", "crash-line"]` joins the operation pool, so seeded kills land
  inside real merges. Every trial's baseline now establishes `crash-line`
  with one checkpoint of its own and returns to main, because the pool's
  contract is that every operation is valid in whatever single draw a trial
  makes: the first validation run left the line out of the baseline, and
  129 of 3,003 trials counted `merge`'s legitimate not-found error as
  failures — a setup artifact, not a byte lost (checkpoints_lost_total
  was 0). That interim run's artifact was overwritten by the runs that
  followed; its figures are recorded in PR #29's description and the
  validation-progression comment on PR #32, and the durable scorecard is
  the gauntlet iteration the results branch records once the amended
  harness measures on main.
- `replay.py` loses the dead `merge` marker tuple; classification of a path
  no engine writes is not coverage, it is decoration.

The verdict logic is untouched: a failure is still a lost durable
checkpoint, a corrupt store, or a verify that does not come back clean.

## Consequences

- G1.1 is refrozen over the amended files; iteration 18's FAIL stands in the
  record as measured, per §0.3 — this ADR is the recorded reason.
- If merge ever grows real scratch files (a conflict-materialising merge, a
  merge oracle work area), the section comes back with markers matching the
  paths that engine actually writes, through a refreeze recorded like this
  one.
