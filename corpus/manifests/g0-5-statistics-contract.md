# G0.5 — Reference repo statistics contract (FROZEN)

§6 requires the composite reference repo to satisfy "a **frozen
corpus-statistics contract** asserted by the harness itself: file count,
size-distribution percentiles, binary fraction, history depth, churn
distribution, directory fan-out."

This file is that contract. It is frozen before the composite is built, and
`harness/g0/g0_5_corpora.py` asserts every row. A composite that misses any row
fails the gate; the harness does not accept "close enough."

## Why these numbers

The reference repo exists so that G1.5–G1.9, G4.6, G5.4 and G5.8 measure
something resembling the workloads Lattice claims to serve (§1: monorepo and
large-asset teams; ~100k files per §5.7). Every bound below is chosen so that a
composite satisfying it cannot be a degenerate corpus that flatters the engine —
in particular, the binary fraction and the p99 file size stop the composite from
collapsing into "a large pile of small text files," which is the corpus shape
content-defined chunking looks best on relative to nothing and worst on relative
to git.

## Pinned base repositories

The composite is built by `scripts/corpus/build_reference_repo.py` from
hash-pinned snapshots of three public repositories, one per required character.
Pins are recorded in `corpus/manifests/g0-5-pins.json` at build time (commit SHA
per base) and asserted on every rebuild.

| Role | Repository | Tip files | Commits | Why |
|---|---|---:|---:|---|
| source-heavy | `microsoft/TypeScript` | 65,988 | 42,055 | very large real source tree; deep nesting |
| source-heavy (2) | `symfony/symfony` | 15,008 | 83,279 | second source character: PHP monorepo component layout |
| binary-heavy | `opencv/opencv_extra` | 12,894 | 2,141 | real test media — images and video, genuinely incompressible |
| deep-history | `git/git` | 4,850 | 85,525 | the deepest freely available history of a real codebase |

### AMENDMENT 1 — four bases, not three

**Recorded because this file was frozen and hash-pinned, and this change moved
the pin.** The original table named three bases: `symfony/symfony`,
`opencv/opencv_extra`, `git/git`. On building the composite, those three were
measured to contribute **32,752 files at their tips** — against this contract's
own `file_count ≥ 90,000` bound. The contract as first frozen was therefore
**unsatisfiable**, and the error was in the base selection, not in the bound.

The bound is unchanged; §5.7's "~100k files" is the requirement and weakening it
to fit the corpus I happened to clone would be precisely the manipulation the
hash pin exists to prevent. Instead `microsoft/TypeScript` is added as a second
source-heavy base, bringing the composite to ~98,700 files.

§6 requires "≥ 3 named public repos", a floor rather than a cap, so four bases
are faithful to the brief. Under §0.3's classification this amendment is
**equivalent in strictness**: no bound moved, and the corpus it produces is
strictly larger and more varied than the one originally named.

The superseded pin was `a3109bd2b4db370c…`; the current pin is recorded in
`corpus/manifests/g0-5-statistics-contract.sha256`.

## The contract

| Property | Bound | Rationale |
|---|---|---|
| `file_count` | ≥ 90,000 and ≤ 130,000 | §5.7's "~100k files" with a ±30% band |
| `total_working_bytes` | ≥ 1.5 GiB | a working set that cannot sit in page cache incidentally |
| `history_bytes` | ≥ 2.0 GiB | §6's "~2 GB-history" |
| `history_depth` | ≥ 20,000 commits | deep enough that history-walking cost is visible |
| `binary_fraction` | ≥ 0.05 and ≤ 0.35 | must be genuinely mixed; below 5% the binary path is untested, above 35% the corpus stops resembling a codebase |
| `p50_file_bytes` | ≥ 512 and ≤ 8,192 | source-file-shaped median |
| `p90_file_bytes` | ≥ 8,192 | a real tail exists |
| `p99_file_bytes` | ≥ 262,144 | large files are present, not just implied |
| `max_file_bytes` | ≥ 33,554,432 | at least one ≥32 MiB file, so chunk trees are exercised |
| `directory_count` | ≥ 6,000 | real tree depth |
| `max_directory_fanout` | ≥ 500 | at least one directory with many entries — the case that breaks naive tree encodings |
| `mean_directory_fanout` | ≥ 3.0 and ≤ 40.0 | neither a flat pile nor a pathological chain |
| `max_path_depth` | ≥ 8 | deep nesting, relevant to Windows path limits (tier-1 per §5.7) |
| `churn_p50_files_per_commit` | ≥ 1 | commits touch real files |
| `churn_p95_files_per_commit` | ≥ 20 | large commits exist, so `ltx save` cost-∝-changed-data (G1.6) is testable |
| `distinct_extensions` | ≥ 30 | language and asset diversity |

## Edit sets for latency gates

§6: *"'Edit sets' for latency gates are replayed sequences of real consecutive
commits from the pinned base repos — never synthetic edits you design."*

`scripts/corpus/build_edit_sets.py` extracts, for each base repository, runs of
consecutive commits with their real per-file diffs, and writes them as replayable
edit sets. Gate harnesses for G1.5–G1.7 replay these; no harness may construct
an edit itself.

## Large-binary corpus (separate from the reference repo)

§6/G0.5 additionally requires a "large-binary corpus ≥ 5 GB with edit-history
simulation script."

| Property | Bound |
|---|---|
| `seed_bytes` | ≥ 500 MiB of real binary material (media, fonts, compiled artifacts) |
| `total_bytes_with_history` | ≥ 5 GiB |
| `versions_per_seed` | ≥ 8 |
| `mutation_kinds` | ≥ 4 distinct kinds, each exercised |

The history is simulated because no public corpus provides ≥5 GB of binary edit
history under a permissive licence. The simulation is honest about being a
simulation: `scripts/corpus/simulate_binary_history.py` applies four mutation
kinds that model how binary assets actually change —

1. **localized in-place patch** (an image region re-encoded; bytes change in a
   bounded span) — the case CDC should win decisively;
2. **prefix insertion / header rewrite** (metadata block grows) — the case that
   defeats fixed-size chunking and is the reason CDC exists;
3. **append** (log-like or streaming assets);
4. **whole-file re-encode** (a lossy re-export; almost nothing is shared) — the
   case where CDC must *not* claim savings it does not have.

Mutation selection is seeded (`G0_5_SEED = 20260903`) and recorded, so the
corpus is byte-reproducible from the seeds.
