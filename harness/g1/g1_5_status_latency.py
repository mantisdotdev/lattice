#!/usr/bin/env python3
"""
G1.5 — `ltx status` latency (SOFT, perf, timing).

Target: p95 < 100 ms with the watcher resident, on the reference repo and the
§0.3 reference environment.

Per docs/DISAGREEMENTS.md Challenge 5 the harness reports BOTH the resident and
the daemonless figure. The gate's value is the daemon-resident p95, because that
is what the brief specifies; re-scoping it upward on our own authority is as
forbidden as downward. The daemonless number appears in every scorecard so a
catastrophic degradation is visible rather than hidden.
"""
from __future__ import annotations
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import ltxrun as L  # noqa: E402

GATE = "G1.5"


def main() -> int:
    try:
        L.require_ltx()
        L.require_reference_repo()
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))

    work = Path(tempfile.mkdtemp(prefix="g1-5-"))
    try:
        repo = L.clone_reference(work / "repo")
        adopt = L.run(["adopt"], cwd=repo)
        if adopt.returncode != 0:
            return L.not_implemented(GATE, f"ltx adopt failed: {adopt.stderr[:200]}")

        both = L.measure_both(["status"], cwd=repo, runs=L.MIN_WARM_RUNS)
        resident = both["daemon_resident"]

        if resident["failures"] or resident["runs"] < L.MIN_WARM_RUNS:
            return L.emit({
                "gate": GATE, "value": 1e9, "unit": "ms",
                "note": f"{resident['failures']} failed invocations; "
                        f"{resident['runs']} of {L.MIN_WARM_RUNS} warm runs completed",
                "detail": both})

        return L.emit({
            "gate": GATE,
            "value": resident["p95_ms"],
            "unit": "ms",
            "note": (f"p95 {resident['p95_ms']:.1f} ms resident, "
                     f"{both['daemonless']['p95_ms']:.1f} ms daemonless "
                     f"({both['daemonless_slowdown']}x)"),
            "detail": both,
            "evidence": ["bench/ENVIRONMENT.md"],
        })
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
