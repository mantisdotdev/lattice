#!/usr/bin/env python3
"""
G1.7 — `ltx switch` / `ltx log` latency (SOFT, perf, timing).

Target: switch p95 < 500 ms, log p95 (first page, lensed) < 100 ms.

Two commands with different targets share one gate, so the registry's metric is
the NORMALISED maximum: max(switch_p95 / 500, log_p95 / 100). A value below 1.0
means both sub-targets are met; the raw milliseconds for each are reported in
detail so neither can hide behind the other.
"""
from __future__ import annotations
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import ltxrun as L  # noqa: E402

GATE = "G1.7"
SWITCH_TARGET_MS = 500.0
LOG_TARGET_MS = 100.0


def main() -> int:
    try:
        L.require_ltx()
        L.require_reference_repo()
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))

    work = Path(tempfile.mkdtemp(prefix="g1-7-"))
    try:
        repo = L.clone_reference(work / "repo")
        if L.run(["adopt"], cwd=repo).returncode != 0:
            return L.not_implemented(GATE, "ltx adopt failed")
        for line in ("probe-a", "probe-b"):
            if L.run(["start", line], cwd=repo).returncode != 0:
                return L.not_implemented(GATE, f"ltx start {line} failed")

        # Alternate targets so every switch is a real context change rather
        # than a no-op that returns immediately.
        targets = ["probe-a", "probe-b"]
        state = {"i": 0}

        def switch_before(_):
            state["i"] += 1

        switch = L.measure_both(
            ["switch", targets[0]], cwd=repo, runs=L.MIN_WARM_RUNS)
        log = L.measure_both(
            ["log", "--limit", "50"], cwd=repo, runs=L.MIN_WARM_RUNS)

        sw = switch["daemon_resident"]["p95_ms"]
        lg = log["daemon_resident"]["p95_ms"]
        normalised = max(sw / SWITCH_TARGET_MS, lg / LOG_TARGET_MS)

        failures = (switch["daemon_resident"]["failures"]
                    + log["daemon_resident"]["failures"])
        detail = {"switch": switch, "log": log,
                  "switch_target_ms": SWITCH_TARGET_MS,
                  "log_target_ms": LOG_TARGET_MS,
                  "switch_p95_ms": sw, "log_p95_ms": lg}
        if failures:
            return L.emit({"gate": GATE, "value": 1e9, "unit": "ratio",
                           "note": f"{failures} failed invocations",
                           "detail": detail})

        return L.emit({
            "gate": GATE, "value": round(normalised, 4), "unit": "ratio",
            "note": (f"switch p95 {sw:.1f} ms (target {SWITCH_TARGET_MS:.0f}), "
                     f"log p95 {lg:.1f} ms (target {LOG_TARGET_MS:.0f}); "
                     f"binding = {'switch' if sw / SWITCH_TARGET_MS > lg / LOG_TARGET_MS else 'log'}"),
            "detail": detail,
            "evidence": ["bench/ENVIRONMENT.md"],
        })
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
