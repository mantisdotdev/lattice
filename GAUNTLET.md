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

### Iteration 4 — 2026-09-03T06:15:59Z

Stages measured: **G0** CLEAR, **G1** BLOCKED, **G2** IN PROGRESS, **G3** IN PROGRESS, **G4** IN PROGRESS, **G5** IN PROGRESS, **G6** IN PROGRESS  
Delivery state (§0.6): **NOT DELIVERABLE**

| Gate | Title | Type | Metric target | Measured | Status | Δ |
|---|---|---|---|---|---|---|
| G0.1 | Prior-art analyses | HARD [A] | `>= 11 analyses` | 11 analyses | PASS<br><sub>11/11 §3 entries fully satisfy the structural contract</sub> | new |
| G0.2 | Disagreements memo | HARD [A] | `>= 5 challenges` | 16 challenges | PASS<br><sub>16 structurally complete challenges of 16 present</sub> | new |
| G0.3 | Merge-replay corpus + oracle | HARD [A] | `>= 10000 merge commits` | 284316 merge commits | PASS<br><sub>284316 merges, 24 repos, 10 languages; line conflict rate 8.6655%, divergence baseline 0.2957%</sub> | new |
| G0.4 | Refactoring corpus | HARD [A] | `>= 1000 instances` | 214463 instances | PASS<br><sub>214463 ground-truthed instances across 22 repos, 12 languages; 204975 are cross-directory moves</sub> | +214463 |
| G0.5 | Corpora assembly | HARD [A] | `>= 1 contract` | 1 contract | PASS<br><sub>reference repo and binary corpus satisfy the frozen contract</sub> | new |
| G0.6 | Foundational ADRs | HARD [A] | `>= 5 ADRs` | 5 ADRs | PASS<br><sub>5/5 foundational artifacts complete</sub> | new |
| G0.7 | Provenance demand probe | HARD [A] | `>= 3 repos` | 4 repos | PASS<br><sub>verdict GO: C1=14.43% (≥5.0), C2=29.84% (≥20.0), C3=+14.23pp (≥3.0)</sub> | new |
| G0.8 | Revised-spec package | SOFT [S] | `>= 1 package` | 1 package | PASS-PENDING-HUMAN<br><sub>package complete and delivered | human kit: STAKEHOLDER/001-disagreements-and-revised-spec.md</sub> | new |
| G1.1 | Crash & power-loss safety | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G1.2 | Byte fidelity | HARD [A] | `== 0 mismatches` | — | N/A-yet<br><sub>ltx binary not built yet (Stage G0 forbids product code)</sub> | — |
| G1.3 | Universal undo | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>ltx binary not built yet (Stage G0 forbids product code)</sub> | — |
| G1.4 | Concurrency safety | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G1.5 | `ltx status` latency | SOFT [A] | `< 100 ms` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G1.6 | `ltx save` latency | SOFT [A] | `< 250 ms` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G1.7 | `ltx switch` / `ltx log` latency | SOFT [A] | `< 1 ratio` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G1.8 | Chunking efficiency | SOFT [A] | `< 262144 bytes` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G1.9 | Storage honesty (dual baseline) | SOFT [A] | `< 1 ratio` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G1.10 | Fuzzing | HARD [A] | `== 0 defects` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G1.11 | Mutation testing | SOFT [A] | `>= 80 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G1.12 | Cross-platform | HARD [A] | `>= 3 platforms` | 0 platforms | FAIL<br><sub>job succeeded without building on: macos-latest, ubuntu-latest, windows-latest (a self-skipped job is not a platform pass); measured run is at 1bb6cfc6ce85, HEAD is 4dd6d841ca4b — stale, so no platform is credited</sub> | new |
| G1.13 | Licence compatibility | HARD [A] | `== 0 violations` | — | N/A-yet<br><sub>no Cargo workspace and no vendored grammars yet (Stage G0 forbids product code)</sub> | — |
| G2.1 | Ten-minute scenario — floor | SOFT [A] | `>= 90 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.2 | Ten-minute scenario — human | SOFT [S] | `>= 80 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.3 | Concept lint | HARD [A] | `== 0 violations` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.4 | Error recoverability | HARD [A] | `== 0 deficient errors` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.5 | JSON contract | HARD [A] | `== 0 gaps` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.6 | Changeset partitioning | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.7 | Lens integrity | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.8 | Thinning safety | HARD [A] | `== 0 violations` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.1 | Round-trip fidelity | HARD [A] | `>= 100 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.2 | Change-ID survival | SOFT [A] | `>= 99 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.3 | Invisible-to-teammates | HARD [A] | `== 0 anomalies` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.4 | Lossy-edge ledger | HARD [A] | `== 0 undocumented edges` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.5 | Dogfood by volume | HARD [A] | `>= 1 composite` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.6 | Lens sufficiency | SOFT [A+S] | `>= 100 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.1 | Entity-match precision | HARD [A] | `>= 99 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.2 | Entity-match recall | SOFT [A] | `>= 75 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.3 | Silent mis-merges | HARD [A] | `<= 0.1 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.4 | Conflict reduction | SOFT [A] | `>= 20 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.5 | Degradation honesty | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.6 | Structural diff performance | SOFT [A] | `< 2 ratio` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.7 | Index regenerability | HARD [A] | `== 0 divergences` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.1 | Provenance correctness | HARD [A] | `== 0 errors` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.2 | Attestation integrity | HARD [A] | `== 0 misses` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.3 | Multi-agent trial | HARD [A] | `>= 1 composite` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.4 | Provenance query latency | SOFT [A] | `< 200 ms` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.5 | Daemon/API parity | HARD [A] | `== 0 gaps` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.6 | Native sync convergence | HARD [A] | `== 0 divergences` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.7 | Redaction under sync | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.8 | Partial clone | SOFT [A] | `<= 25 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.1 | Data-loss hunter | HARD [A] | `== 0 confirmed findings` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.2 | Security review | HARD [A+S] | `== 0 confirmed findings` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.3 | Performance audit | HARD [A] | `== 0 regressions` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.4 | Spec-compliance audit | HARD [A] | `== 0 deviations` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.5 | Cold-start UX walkthrough | HARD [A] | `>= 1 completion` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |


### Iteration 6 — 2026-09-03T11:36:12Z

Stages measured: **G0** CLEAR, **G1** BLOCKED, **G2** IN PROGRESS, **G3** IN PROGRESS, **G4** IN PROGRESS, **G5** IN PROGRESS, **G6** IN PROGRESS  
Delivery state (§0.6): **NOT DELIVERABLE**

| Gate | Title | Type | Metric target | Measured | Status | Δ |
|---|---|---|---|---|---|---|
| G0.1 | Prior-art analyses | HARD [A] | `>= 11 analyses` | 11 analyses | PASS<br><sub>11/11 §3 entries fully satisfy the structural contract</sub> | new |
| G0.2 | Disagreements memo | HARD [A] | `>= 5 challenges` | 16 challenges | PASS<br><sub>16 structurally complete challenges of 16 present</sub> | new |
| G0.3 | Merge-replay corpus + oracle | HARD [A] | `>= 10000 merge commits` | 284316 merge commits | PASS<br><sub>284316 merges, 24 repos, 10 languages; line conflict rate 8.6655%, divergence baseline 0.2957%</sub> | new |
| G0.4 | Refactoring corpus | HARD [A] | `>= 1000 instances` | 214463 instances | PASS<br><sub>214463 ground-truthed instances across 22 repos, 12 languages; 204975 are cross-directory moves</sub> | new |
| G0.5 | Corpora assembly | HARD [A] | `>= 1 contract` | 1 contract | PASS<br><sub>reference repo and binary corpus satisfy the frozen contract</sub> | new |
| G0.6 | Foundational ADRs | HARD [A] | `>= 5 ADRs` | 5 ADRs | PASS<br><sub>5/5 foundational artifacts complete</sub> | new |
| G0.7 | Provenance demand probe | HARD [A] | `>= 3 repos` | 4 repos | PASS<br><sub>verdict GO: C1=14.43% (≥5.0), C2=29.84% (≥20.0), C3=+14.23pp (≥3.0)</sub> | new |
| G0.8 | Revised-spec package | SOFT [S] | `>= 1 package` | 1 package | PASS-PENDING-HUMAN<br><sub>package complete and delivered | human kit: STAKEHOLDER/001-disagreements-and-revised-spec.md</sub> | new |
| G1.1 | Crash & power-loss safety | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>ltx binary not built (target/release/ltx)</sub> | — |
| G1.2 | Byte fidelity | HARD [A] | `== 0 mismatches` | — | N/A-yet<br><sub>ltx binary not built yet (Stage G0 forbids product code)</sub> | — |
| G1.3 | Universal undo | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>ltx binary not built yet (Stage G0 forbids product code)</sub> | — |
| G1.4 | Concurrency safety | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>ltx binary not built (target/release/ltx)</sub> | — |
| G1.5 | `ltx status` latency | SOFT [A] | `< 100 ms` | — | N/A-yet<br><sub>ltx binary not built (target/release/ltx)</sub> | — |
| G1.6 | `ltx save` latency | SOFT [A] | `< 250 ms` | — | N/A-yet<br><sub>ltx binary not built (target/release/ltx)</sub> | — |
| G1.7 | `ltx switch` / `ltx log` latency | SOFT [A] | `< 1 ratio` | — | N/A-yet<br><sub>ltx binary not built (target/release/ltx)</sub> | — |
| G1.8 | Chunking efficiency | SOFT [A] | `< 262144 bytes` | — | N/A-yet<br><sub>engine not built, so the store cannot be measured. The chunking design alone measures p95 32,768 bytes against a 262,144 target -- reported as evidence, not claimed as a pass.</sub> | — |
| G1.9 | Storage honesty (dual baseline) | SOFT [A] | `< 1 ratio` | — | N/A-yet<br><sub>ltx binary not built (target/release/ltx)</sub> | — |
| G1.10 | Fuzzing | HARD [A] | `== 0 defects` | — | N/A-yet<br><sub>no fuzz targets found in fuzz/fuzz_targets/ (Stage G0 forbids product code, so there is nothing to fuzz)</sub> | — |
| G1.11 | Mutation testing | SOFT [A] | `>= 80 percent` | — | N/A-yet<br><sub>none of the scoped modules exist yet: store, oplog, merge</sub> | — |
| G1.12 | Cross-platform | HARD [A] | `>= 3 platforms` | 0 platforms | FAIL<br><sub>job succeeded without building on: macos-latest, ubuntu-latest, windows-latest (a self-skipped job is not a platform pass)</sub> | new |
| G1.13 | Licence compatibility | HARD [A] | `== 0 violations` | — | N/A-yet<br><sub>no Cargo workspace and no vendored grammars yet (Stage G0 forbids product code)</sub> | — |
| G2.1 | Ten-minute scenario — floor | SOFT [A] | `>= 90 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.2 | Ten-minute scenario — human | SOFT [S] | `>= 80 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.3 | Concept lint | HARD [A] | `== 0 violations` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.4 | Error recoverability | HARD [A] | `== 0 deficient errors` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.5 | JSON contract | HARD [A] | `== 0 gaps` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.6 | Changeset partitioning | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.7 | Lens integrity | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G2.8 | Thinning safety | HARD [A] | `== 0 violations` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.1 | Round-trip fidelity | HARD [A] | `>= 100 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.2 | Change-ID survival | SOFT [A] | `>= 99 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.3 | Invisible-to-teammates | HARD [A] | `== 0 anomalies` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.4 | Lossy-edge ledger | HARD [A] | `== 0 undocumented edges` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.5 | Dogfood by volume | HARD [A] | `>= 1 composite` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G3.6 | Lens sufficiency | SOFT [A+S] | `>= 100 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.1 | Entity-match precision | HARD [A] | `>= 99 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.2 | Entity-match recall | SOFT [A] | `>= 75 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.3 | Silent mis-merges | HARD [A] | `<= 0.1 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.4 | Conflict reduction | SOFT [A] | `>= 20 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.5 | Degradation honesty | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.6 | Structural diff performance | SOFT [A] | `< 2 ratio` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G4.7 | Index regenerability | HARD [A] | `== 0 divergences` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.1 | Provenance correctness | HARD [A] | `== 0 errors` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.2 | Attestation integrity | HARD [A] | `== 0 misses` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.3 | Multi-agent trial | HARD [A] | `>= 1 composite` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.4 | Provenance query latency | SOFT [A] | `< 200 ms` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.5 | Daemon/API parity | HARD [A] | `== 0 gaps` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.6 | Native sync convergence | HARD [A] | `== 0 divergences` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.7 | Redaction under sync | HARD [A] | `== 0 failures` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G5.8 | Partial clone | SOFT [A] | `<= 25 percent` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.1 | Data-loss hunter | HARD [A] | `== 0 confirmed findings` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.2 | Security review | HARD [A+S] | `== 0 confirmed findings` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.3 | Performance audit | HARD [A] | `== 0 regressions` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.4 | Spec-compliance audit | HARD [A] | `== 0 deviations` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |
| G6.5 | Cold-start UX walkthrough | HARD [A] | `>= 1 completion` | — | N/A-yet<br><sub>harness not implemented yet</sub> | — |

