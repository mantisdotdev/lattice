#!/usr/bin/env python3
"""
G1.12 — Cross-platform (HARD).

Target: full suite green on Linux, macOS, Windows.

Windows is tier-1 from month one (§5.7), so this gate is measured from the
project's actual CI rather than from a local run: a suite that passes on the
developer's machine says nothing about the platform the developer does not have.

The measured value is the number of platforms whose job concluded `success` in
the most recent completed run of the `rust` workflow on the default branch.

Two guards, because the obvious implementations of this harness are wrong:

  1. A job that SKIPPED does not count as a platform that passed. The Rust
     workflow self-skips while Stage G0 forbids product code, so a naive
     "no failures" check would report 3/3 green against a workspace that does
     not exist. This harness requires each platform to have actually built and
     tested, detected by the presence of a completed `test` step.
  2. The run must be recent relative to HEAD. A green run from twenty commits
     ago is not evidence about the current tree, so the harness records the
     commit it measured and flags a stale result.
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
WORKFLOW = "rust"
REQUIRED_PLATFORMS = {"ubuntu-latest", "macos-latest", "windows-latest"}
# Steps that must have run for the job to count as a real platform pass.
REQUIRED_STEPS = {"test"}


def gh(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["gh", *args], cwd=REPO, capture_output=True,
                          text=True, errors="replace", timeout=120)


def unmeasurable(note: str) -> int:
    print(json.dumps({"gate": "G1.12", "status": "not-implemented", "note": note}))
    return 0


def main() -> int:
    if gh("--version").returncode != 0:
        return unmeasurable("gh CLI unavailable; cannot read CI results")

    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                          capture_output=True, text=True).stdout.strip()

    runs = gh("run", "list", "--workflow", WORKFLOW, "--branch", "main",
              "--status", "completed", "--limit", "1",
              "--json", "databaseId,headSha,conclusion,createdAt")
    if runs.returncode != 0:
        return unmeasurable(f"could not list CI runs: {runs.stderr.strip()[:200]}")
    try:
        rows = json.loads(runs.stdout or "[]")
    except json.JSONDecodeError:
        return unmeasurable("unparseable CI run listing")
    if not rows:
        return unmeasurable(f"no completed '{WORKFLOW}' runs on main yet")

    run = rows[0]
    jobs = gh("run", "view", str(run["databaseId"]), "--json",
              "jobs")
    if jobs.returncode != 0:
        return unmeasurable(f"could not read run jobs: {jobs.stderr.strip()[:200]}")
    job_rows = json.loads(jobs.stdout or "{}").get("jobs", [])

    per_platform: dict[str, dict] = {}
    for job in job_rows:
        name = job.get("name", "")
        platform = next((p for p in REQUIRED_PLATFORMS if p in name), None)
        if platform is None:
            continue
        steps = {s.get("name", "").strip().lower(): s.get("conclusion")
                 for s in job.get("steps", [])}
        ran_required = all(
            steps.get(req) == "success" for req in REQUIRED_STEPS)
        per_platform[platform] = {
            "job": name,
            "conclusion": job.get("conclusion"),
            "ran_required_steps": ran_required,
            "steps": steps,
            "counts_as_pass": job.get("conclusion") == "success" and ran_required,
        }

    green = sum(1 for v in per_platform.values() if v["counts_as_pass"])
    missing = sorted(REQUIRED_PLATFORMS - set(per_platform))
    skipped = sorted(p for p, v in per_platform.items()
                     if v["conclusion"] == "success" and not v["ran_required_steps"])

    notes = []
    if missing:
        notes.append(f"no job for: {', '.join(missing)}")
    if skipped:
        notes.append(f"job succeeded without building on: {', '.join(skipped)} "
                     f"(a self-skipped job is not a platform pass)")
    stale = run.get("headSha") != head
    if stale:
        # Stale evidence must change the VALUE, not just annotate it. A note
        # alone left `value: 3` on a run from an older commit, which the runner
        # would have compared against the >= 3 target and passed.
        notes.append(f"measured run is at {str(run.get('headSha'))[:12]}, "
                     f"HEAD is {head[:12]} — stale, so no platform is credited")
        green = 0

    print(json.dumps({
        "gate": "G1.12",
        "value": green,
        "unit": "platforms",
        "note": ("; ".join(notes) if notes
                 else f"{green}/3 platforms green with the suite actually run"),
        "detail": {
            "workflow": WORKFLOW,
            "run_id": run["databaseId"],
            "run_sha": run.get("headSha"),
            "run_created_at": run.get("createdAt"),
            "repo_head": head,
            "required_steps": sorted(REQUIRED_STEPS),
            "platforms": per_platform,
        },
        "evidence": [f".github/workflows/{WORKFLOW}.yml"],
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
