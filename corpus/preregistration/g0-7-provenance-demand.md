# G0.7 — Provenance demand probe: PRE-REGISTRATION

**This file is written and hash-pinned BEFORE the probe runs.** §6/G0.7 requires
"numeric go/no-go criteria against that proxy pre-registered in a committed file
before the probe runs." The commit that adds this file contains no probe results,
and `harness/g0/g0_7_demand_probe.py` refuses to report a verdict unless this
file's recorded hash matches the hash stored in the results, so the criteria
cannot be adjusted after seeing the data.

## Hypothesis under test

> Teams with agent-authored code have real demand for queryable provenance.

## Why this needs testing

The agent-native layer (§5.5) is Lattice's stated differentiator. If the demand
is imagined, the differentiator is decoration and the project should be a
storage-and-UX play instead. §6 states plainly: if the pre-registered criteria
say no-go, halt and report — and that report is a valid deliverable.

## Constraint on method

The probe **must not** require the Lattice bridge, store, or CLI. It runs
standalone against plain `git log` output. This is deliberate: a probe that needs
the product cannot inform whether to build the product.

## The demand proxy

Direct demand cannot be observed without surveying teams. The observable proxy is
**the prevalence of ad-hoc provenance conventions** — that is, work people
already do by hand, badly, because no tool does it for them. Three signals:

| Signal | Operationalisation |
|---|---|
| **S1 — non-human authorship is recorded at all** | commits whose author or committer identity matches a bot/agent pattern (`[bot]` suffix, known agent tool identities, `noreply` automation addresses) |
| **S2 — co-authorship trailers** | commits carrying a `Co-Authored-By:` trailer, and the subset whose co-author is an agent or bot identity |
| **S3 — structured provenance in message bodies** | commits carrying any structured trailer conveying origin, review, or generation metadata (`Generated-by`, `Reviewed-by`, `Signed-off-by`, `Assisted-by`, `Change-Id`, `PR-URL`, `Reviewed-On`) |

Each signal is a *convention invented because the VCS offered nothing*. That is
the demand being measured: the cost people already pay.

## Pre-registered numeric go/no-go criteria

Measured over the most recent **20,000 commits** of each probed repository
(fewer if the repository has fewer), across **≥ 3 repositories with heavy
agent/bot activity**, plus a **contrast set** of repositories chosen without
regard to bot activity.

**GO requires all three:**

- **C1.** In the agent-heavy set, the median across repositories of
  `(S1 ∪ S2-agent)` — commits attributable to a non-human author by either
  route — is **≥ 5.0%** of commits.
- **C2.** In the agent-heavy set, the median across repositories of
  `(S1 ∪ S2 ∪ S3)` — commits carrying *any* ad-hoc provenance convention — is
  **≥ 20.0%** of commits.
- **C3.** The convention is not merely universal boilerplate: the agent-heavy
  set's median for `(S1 ∪ S2-agent)` exceeds the contrast set's median by
  **≥ 3.0 percentage points**. Without C3, a high C1 would only show that bots
  exist everywhere, not that this population differs.

**NO-GO** if any of C1, C2, C3 fails.

**Ambiguous** — if C1 and C2 pass but C3 fails by less than 1.0 percentage
point, the verdict is reported as `WEAK-GO` and escalated to the stakeholder
under §0.7 rather than decided unilaterally. This band is declared now so that a
marginal result cannot be narrated into a clean one later.

## Declared threats to validity, recorded before measurement

1. **Bot ≠ agent.** CI bots (dependabot, release automation) inflate S1 without
   representing *authorship* by a reasoning agent. Mitigation: the detector
   classifies bots into `ci-automation` and `authoring-agent` classes, and C1 is
   additionally reported split by class. The pre-registered criteria use the
   union, and the split is reported so a reader can discount it.
2. **Trailer conventions are project culture, not demand.** `Signed-off-by` in
   the Linux kernel reflects the DCO, not provenance appetite. Mitigation: C3's
   contrast requirement, and S3 is reported per-trailer so `Signed-off-by`-only
   repositories are visible as such.
3. **Recency.** Agent-authored code is recent; a 20,000-commit window on a
   long-lived repository may predate it. Mitigation: the probe additionally
   reports the signal rate over the most recent 2,000 commits, so a rising trend
   is visible. This is reported, not part of the criteria — adding it to the
   criteria after the fact would be exactly the manipulation this file prevents.
4. **Selection.** "Repositories with heavy agent/bot activity" is a judgment
   made before measurement; the chosen repositories are listed below and may not
   be changed after the probe runs.

## Repositories, fixed now

**Agent-heavy set** (selected for known bot/automation authorship density):

- `home-assistant/core`
- `microsoft/vscode`
- `nodejs/node`
- `kubernetes/kubernetes`

**Contrast set** (selected without regard to bot activity; these are already
being cloned for G0.3/G0.4 and were chosen for merge and refactor density):

- `git/git`
- `postgres/postgres`
- `django/django`
- `rails/rails`

## Verdict recording

The harness writes `corpus/data/g0-7-probe.json` containing every per-repository
count, the criteria evaluation, this file's SHA-256, and the verdict. The written
verdict lives in `docs/G0-7-VERDICT.md`.
