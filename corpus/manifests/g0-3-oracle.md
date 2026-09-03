# G0.3 merge oracle — normalization and exclusion rules

**Frozen before any G4 work.** §6 requires the mis-merge oracle to be
"enumerated and hash-pinned here, before any G4 work." This file is that
enumeration; `corpus/manifests/g0-3-oracle.sha256` pins it, and the replay
harness records the pin in every result file it writes.

## What the oracle decides

Given a mined two-parent merge `M` with parents `P1`, `P2`:

1. **Replay.** Recompute the merge with a pinned three-way line merge
   (`git merge-tree --write-tree P1 P2`, git 2.50.1, ORT strategy, default
   rename detection). This is the *line-based* result.
2. **Compare** the replayed tree against `M`'s recorded tree.
3. **Classify** into exactly one bucket (below).
4. For divergent cases, apply **normalization** before deciding the trees really
   differ, then apply **exclusion rules**.

## Classification buckets

| Bucket | Condition | Used for |
|---|---|---|
| `CLEAN_MATCH` | replay clean, replayed tree == recorded tree | the population G4.3-a measures over |
| `CLEAN_DIVERGE` | replay clean, trees differ after normalization, not excluded | **divergence baseline** (Challenge 2, baseline 2) |
| `CLEAN_DIVERGE_NORMALIZED` | replay clean, trees differ only under normalization rules | reported, counted as match |
| `EXCLUDED_EVIL` | replay clean, difference contains content present in neither parent nor base | excluded per §6 |
| `REPLAY_CONFLICT` | replay reports conflicts; the human resolution is ground truth | **conflict baseline**, and the G4.4 denominator |
| `ERROR` | replay could not run (missing objects, no merge base) | excluded, counted separately |

## Why three baselines are reported, not one

Empirically confirmed on this corpus: for a clean auto-merge, the tree recorded
in the merge commit *is* the tree the line merge produces — because that is
literally how the human made it. A "line-based silent mis-merge rate" computed
over that population is therefore identically zero for a tautological reason,
not because line merge is safe. See `docs/DISAGREEMENTS.md`, Challenge 2.

The three reported baselines:

1. **Naive** — mis-merges among `CLEAN_MATCH`. Zero by construction. Reported so
   the number is never mistaken for evidence.
2. **Divergence** — `CLEAN_DIVERGE` ÷ (all clean replays). The honest measure of
   "line merge cleanly produced something the human did not commit." Non-zero
   because replay tooling, rename-detection settings, merge drivers and git
   version differ from the original merge.
3. **Resolved-conflict** — the `REPLAY_CONFLICT` population, where the human
   resolution is genuine ground truth. This is the correct comparator for
   structural merge's dangerous population (Challenge 1's G4.3-b).

## Normalization rules (applied to text blobs before comparison)

Applied in this order. Each is language-agnostic unless stated.

| # | Rule | Rationale |
|---|---|---|
| N1 | Line endings normalized to `\n` (CRLF, CR → LF) | `.gitattributes` and platform differences are not merge errors |
| N2 | Trailing whitespace stripped from every line | invisible, never semantic |
| N3 | Trailing blank lines at end-of-file removed; runs of ≥3 blank lines collapsed to 2 | formatting churn |
| N4 | A comma immediately preceding a closing `)`, `]`, `}` (ignoring intervening whitespace/newlines) is removed | trailing-comma style differs by tool and is non-semantic in JS/TS/Rust/Go/Java/Python |
| N5 | Contiguous runs of import lines are sorted. A run is ≥2 consecutive lines each matching the language's import form: `import …` / `from … import …` (Python, JS/TS, Java), `use …;` (Rust), `#include …` (C/C++), `require …` (Ruby/PHP). Runs are delimited by any non-matching line | import order is the canonical example the spec names |

Binary blobs (containing a NUL byte in the first 8000 bytes) are compared
byte-exactly; no normalization applies.

## Exclusion rules

| # | Rule | Rationale |
|---|---|---|
| X1 | **Evil merges.** For each path differing between replayed and recorded tree, if the recorded blob (post-normalization) matches none of: the replayed blob, `P1`'s blob, `P2`'s blob, or the merge base's blob — then content was authored during the merge. Excluded, per §6's explicit instruction | a human writing new code during a merge is not a merge algorithm error |
| X2 | **Octopus merges** (≠2 parents) are never mined | three-way replay is undefined for them |
| X3 | **No merge base** (unrelated histories) | three-way merge is undefined |
| X4 | **Submodule-only differences** (gitlink entries) | submodule pointer resolution is policy, not merge |

## Pinned tooling

| Tool | Version |
|---|---|
| git (replay) | 2.50.1 (Apple Git-155) |
| merge strategy | ORT (git's default since 2.34) |
| rename detection | git default (`-M`, 50% similarity) |

## Sampling

The mined corpus is the full set of two-parent merges in the pinned clones.
Baselines are measured over a **stratified deterministic sample**: seeded with
`G0_3_SEED = 20260903`, capped per repository so no single repository dominates,
recorded in the result file alongside the sample size, so any result is exactly
reproducible.
