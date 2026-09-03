# GAUNTLET.md — the auditable log

This file is the delivery. Per §0.6, *"a delivery not accompanied by the
complete `GAUNTLET.md` history and final scorecard is not a delivery."*

## How to read this file

Every scorecard below was **generated**, never written. `scripts/gauntlet
measure` executes each gate's harness, reads the number the harness printed,
compares it to the target frozen in `harness/gates.toml`, and appends the
result. There is no code path in this repository by which a gate can be
declared PASS without harness output. If a gate shows `N/A-yet`, its harness
does not exist yet and the gate therefore does not exist yet (§0.1).

Reproduce any scorecard:

```bash
scripts/gauntlet measure G0     # or G1, or a single gate id
scripts/gauntlet scorecard
```

## Status vocabulary

| Status | Meaning |
|---|---|
| `PASS` | harness ran, measured value satisfies the target |
| `FAIL` | harness ran, target not met |
| `FAIL(stale)` | harness file changed since its freeze (§0.3) — must be re-measured |
| `FAIL(harness-error)` | harness could not produce a measurement |
| `WAIVED` | SOFT gate, failing, carrying a §0.5 waiver that passed mechanical validation |
| `PASS-PENDING-HUMAN` | autonomous floor passed and the [S] kit is committed; awaiting real humans (§0.7) |
| `N/A-yet` | no harness yet — the gate is not measurable and is not claimed |

`PASS`, `WAIVED`, and `PASS-PENDING-HUMAN` are the three statuses that let a
stage reach CLEAR.

## Reference environment

Recorded in [`bench/ENVIRONMENT.md`](bench/ENVIRONMENT.md). Timing gates apply
unscaled at or above the §0.3 hardware class.

---

## Iteration log

## Protocol note — G0 harness sequencing, recorded rather than glossed

§0.3's harness-first rule requires that a stage's harnesses be "designed,
implemented, and **frozen before** implementation work on that stage begins."
For Stage G0 the record is as follows, stated precisely because the alternative
is to let a reader assume a stricter discipline than was actually applied:

- **G0.2, G0.5, G0.6, G0.8** — harness written and committed before the
  deliverable it measures existed. Fully compliant.
- **G0.7** — the pre-registration was hash-pinned before the probe harness was
  written, and the harness refuses a verdict if the pin moves. This is stronger
  than the rule requires.
- **G0.1** — the harness was written while the research it measures was already
  running in background instances. The harness author had no access to the
  analyses' content when writing the structural checks, so the anti-gaming intent
  is satisfied, but the literal ordering is not. Recorded as a deviation.
- **G0.3, G0.4** — corpus mining scripts and their harnesses were written
  together, before any baseline was measured. The G0.3 oracle was hash-pinned
  before replay ran.

From Stage G1 onward the ordering is strict: harnesses are frozen and their
hashes recorded in `harness/FREEZE.json` before any product code is written. The
freeze mechanism reverts any gate whose harness file changes to `FAIL(stale)`
until re-measured, so a late edit cannot silently preserve a PASS.

## Where the scorecard of record is produced

CI measures only the gates whose inputs are committed to the repository. The
corpus-dependent gates read corpora measured in gigabytes — a mined merge corpus,
a composite reference repository, and a large-binary corpus — which are built by
committed scripts from hash-pinned public sources rather than committed
themselves. Their measured sizes are not restated here: every figure lives in the
scorecard row of the gate that measured it, and in the harness output that row
cites (`corpus/data/merge-baselines.json`, `corpus/manifests/g0-5-pins.json`,
`corpus/manifests/g0-5-binary-corpus.json`). Restating a number in prose is how
prose and measurement drift apart. Running those gates in a clean CI checkout would report a
FAIL meaning "the corpus is not in this checkout," which is not what a FAIL is
supposed to mean.

The scorecard of record is therefore produced on the reference environment
recorded in [`bench/ENVIRONMENT.md`](bench/ENVIRONMENT.md), where the corpora
exist. CI's role is to prove the harnesses still run, that the registry is valid,
that no product code can detect harness execution, and that every scorecard in
this file re-renders byte-identically from committed harness output.

Rebuild the corpora yourself:

```bash
./scripts/clone-corpus.sh                        # pinned public bases
python3 scripts/corpus/mine_merges.py            # G0.3 corpus
python3 scripts/corpus/replay_merges.py          # G0.3 baselines
python3 scripts/corpus/mine_refactorings.py      # G0.4 corpus
python3 scripts/corpus/build_reference_repo.py   # G0.5 reference repo
python3 scripts/corpus/simulate_binary_history.py --force  # G0.5 binary corpus
```

