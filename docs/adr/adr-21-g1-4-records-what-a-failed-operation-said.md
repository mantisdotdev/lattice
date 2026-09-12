# ADR-21 — G1.4 records what a failed operation said

**Status:** Accepted · **Required by:** §0.3 (harness changes after freezing) · **Amends:** the frozen `harness/g1/g1_4_concurrency.py`
**Gates touched:** G1.4 (concurrency safety, HARD) — verdict unchanged

## Context

The first full G1.4 run on the fixed engine (iteration 14, 2026-09-12)
finished with 7 operation failures in 80,000, 0 deadlocks, 0 linearizability
violations, 0 unsequenced successes and a clean verify. The seven were
`split`, `lens use`, `sync --dry-run`, `switch main` and `undo`, spread over
six workspaces, each with an empty `stderr` — because with `--json` the engine
reports its error on stdout, and the frozen harness kept only
`stderr[:160]`.

So the record cannot say what the seven were. Every pilot on a calm machine
was clean; the one pilot under a load average near 100 produced failures of
exactly this shape, all of them "another command has held this repository
for longer than 60 seconds", and the full run spanned an afternoon of active
use with load averages between 14 and 19. That is a strong reading, and it is
still a reading: a measurement that cannot distinguish a lock that waited too
long from a corrupt repository has not measured the thing the gate is about.

## Decision

The failure record gains one field, `stdout[:160]` — what the command said —
beside `stderr[:160]`. Nothing that computes the verdict reads either field:
a failure is still any non-zero exit, and the counts, the linearizability
check, the coverage contract and the verify pass are untouched.

**§0.3 classification: equivalent.** The same engine gets the same verdict
from both files; the amended one carries more evidence for each failure.
Checked by running the pre-amendment copy and the amended file, at reduced
counts, against the real engine (both: 0 failures) and against a wrapper that
makes `lens use` exit non-zero with a JSON error (both: 20 failures in 800
operations, value 66; only the amended record names the category, `busy` in
the wrapper's case).

## Consequences

- Iteration 14 stands as measured: FAIL, 7. It is not reinterpreted.
- The next full run records, for any failure, the engine's own words, so an
  environmental cause is evidence rather than inference — and the run is to be
  made on a machine that is otherwise idle.
