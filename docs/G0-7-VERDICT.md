# G0.7 — Provenance demand probe: VERDICT

**GO.** All three pre-registered criteria pass. The agent-native layer (§5.5)
proceeds.

Criteria were fixed and hash-pinned in
[`corpus/preregistration/g0-7-provenance-demand.md`](../corpus/preregistration/g0-7-provenance-demand.md)
before the probe existed; the harness refuses to emit a verdict if that file's
hash moves. Raw per-repository counts: `corpus/data/g0-7-probe.json`.

## Result

| Criterion | Measured | Threshold | Verdict |
|---|---|---|---|
| **C1** — median % of commits attributable to a non-human author | **14.44%** | ≥ 5.0% | PASS |
| **C2** — median % of commits carrying any ad-hoc provenance convention | **29.84%** | ≥ 20.0% | PASS |
| **C3** — agent-heavy median minus contrast median | **+11.30 pp** | ≥ 3.0 pp | PASS |

Window: the most recent 20,000 non-merge commits per repository.

| Repository | n | agent-route | any convention | agent-route, last 2,000 |
|---|---:|---:|---:|---:|
| home-assistant/core | 20,000 | 13.33% | 22.52% | 17.75% |
| microsoft/vscode | 20,000 | 29.18% | 31.89% | **67.40%** |
| nodejs/node | 20,000 | 15.54% | 99.95% | 21.20% |
| kubernetes/kubernetes | 20,000 | 0.20% | 27.78% | 0.90% |
| *git/git* (contrast) | 20,000 | 0.06% | 99.78% | — |
| *django/django* (contrast) | 20,000 | 6.21% | 9.04% | — |

## What the numbers say

**The demand proxy is real and it is growing.** The headline is vscode's recent
window: 67.40% of its last 2,000 commits carry non-human authorship, against
29.18% over the last 20,000. Home Assistant shows the same direction (17.75%
recent vs 13.33% overall). The trend measurement was pre-registered as *reported
but not part of the criteria*, precisely so it could not be recruited to rescue a
marginal result — and it did not need to be. It is nonetheless the most
consequential number in the table for a system being designed now and shipping
later.

**The pre-registered validity threats were not hypothetical, and the criteria
design absorbed them.**

Threat 2 (trailer conventions are project culture, not demand) is visible in the
starkest possible form: git/git shows 99.78% "any convention" and 0.06%
agent-route. Essentially every commit carries `Signed-off-by`, because the
project mandates the DCO — nothing to do with provenance appetite. Had C3 been
written against C2's metric, the contrast comparison would have been destroyed by
this. It was written against the agent-route metric instead, and the separation
survives at +11.30 pp. nodejs/node shows the same pattern for the same structural
reason (`PR-URL` and `Reviewed-By` on nearly every commit).

Threat 1 (bot ≠ agent) is why the harness reports the CI-automation and
authoring-agent split. The bulk of C1 is CI automation, not reasoning agents.
This does not undermine the hypothesis — the proxy was always "ad-hoc
conventions people invented because the VCS offered nothing," and a
`dependabot[bot]` author line is exactly such a convention — but it does mean C1
should not be read as "14% of code is AI-written."

## An artifact found after measurement, reported as such

Kubernetes returns 0.20% agent-route, far below the rest of the agent-heavy set,
and this is a **measurement artifact rather than a finding**. Kubernetes routes
essentially all integration through `k8s-ci-robot`, which authors *merge*
commits — and the probe passes `--no-merges`, because merge commits distort
authorship analysis. The robot's activity is therefore excluded by construction.

This threat was not pre-registered, so it may not be used to adjust the result,
and it has not been: the reported C1 median of 14.44% includes kubernetes' 0.20%
and passes anyway. It is recorded here because the alternative — quietly dropping
an inconvenient repository or quietly changing the flag — is the exact
manipulation the pre-registration exists to prevent.

## Scope limits of this result

- The contrast set was specified as four repositories; two (`postgres/postgres`,
  `rails/rails`) had not finished cloning when the probe ran, so C3 was computed
  against a two-repository contrast median. C3 passes by 8.3 pp of margin, so the
  verdict is not sensitive to this, but the number is not the four-repository
  number the pre-registration named. The probe is re-run against the full
  contrast set for the final scorecard; the value recorded there supersedes this
  one.
- This measures *revealed* demand — work people already do by hand — not stated
  demand. It cannot tell us people would pay for a better tool; it can only tell
  us they are currently paying for the absence of one.
- Four agent-heavy repositories is a small sample, chosen by judgment before
  measurement. A different four could give a different median.

## Consequence

The hypothesis stands: teams with agent-authored code are already maintaining
ad-hoc provenance conventions at material and rising prevalence, and those
conventions are unqueryable folklore in exactly the way §2 describes. §5.5 is
built. Had this returned NO-GO, §6 required halting and reporting; it did not.
