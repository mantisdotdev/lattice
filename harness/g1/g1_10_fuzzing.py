#!/usr/bin/env python3
"""
G1.10 — Fuzzing (HARD).

Target: every parser/decoder/store-reader target, >= 24 CPU-hours per release
candidate, 0 outstanding crashes/hangs/leaks.

Two things this harness insists on, because "we fuzzed it" is otherwise
unfalsifiable:

  1. TARGETS ARE DISCOVERED, not listed here. Every `fuzz_target!` in
     fuzz/fuzz_targets/ is enumerated, so adding a parser without a fuzz target
     is caught. A hand-maintained list would silently stop covering new code --
     the same failure mode §6's coverage contract addresses for commands.
  2. CPU-HOURS ARE PER TARGET, summed from each target's own recorded run time.
     "24 CPU-hours" spent entirely on one easy target is not 24 CPU-hours of
     fuzzing, so the harness fails if any target falls short.
"""
from __future__ import annotations
import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import ltxrun as L  # noqa: E402

GATE = "G1.10"
FUZZ_DIR = REPO / "fuzz"
TARGETS_DIR = FUZZ_DIR / "fuzz_targets"
ARTIFACTS = FUZZ_DIR / "artifacts"
LEDGER = REPO / "bench" / "results" / "fuzz-ledger.json"
REQUIRED_CPU_HOURS = 24.0

# A crash file that has been triaged and fixed leaves its input behind as a
# regression seed. Only files listed as OUTSTANDING count against the gate.
TRIAGED = FUZZ_DIR / "triaged.json"


def discover_targets() -> list[str]:
    if not TARGETS_DIR.exists():
        return []
    out = []
    for path in sorted(TARGETS_DIR.glob("*.rs")):
        if re.search(r"fuzz_target!\s*\(", path.read_text(errors="replace")):
            out.append(path.stem)
    return out


def main() -> int:
    targets = discover_targets()
    if not targets:
        return L.not_implemented(
            GATE, "no fuzz targets found in fuzz/fuzz_targets/ "
                  "(Stage G0 forbids product code, so there is nothing to fuzz)")

    if not LEDGER.exists():
        return L.emit({
            "gate": GATE, "value": len(targets), "unit": "defects",
            "note": f"{len(targets)} fuzz targets exist but no run ledger at "
                    f"{LEDGER.relative_to(REPO)}; unrecorded fuzzing is unmeasured "
                    f"fuzzing"})
    ledger = json.loads(LEDGER.read_text())
    runs = ledger.get("targets", {})

    triaged = set()
    if TRIAGED.exists():
        triaged = set(json.loads(TRIAGED.read_text()).get("fixed", []))

    outstanding = []
    if ARTIFACTS.exists():
        for artifact in sorted(ARTIFACTS.rglob("*")):
            if artifact.is_file() and artifact.name not in triaged:
                outstanding.append(str(artifact.relative_to(REPO)))

    short = {t: round(runs.get(t, {}).get("cpu_hours", 0.0), 2)
             for t in targets
             if runs.get(t, {}).get("cpu_hours", 0.0) < REQUIRED_CPU_HOURS}
    unrun = [t for t in targets if t not in runs]

    coverage_problems = []
    if unrun:
        coverage_problems.append(f"targets never run: {', '.join(unrun)}")
    if short:
        coverage_problems.append(
            f"below {REQUIRED_CPU_HOURS} CPU-hours: "
            + ", ".join(f"{t}={h}h" for t, h in list(short.items())[:6]))

    return L.emit({
        "gate": GATE,
        "value": len(outstanding),
        "unit": "defects",
        "note": (f"{len(outstanding)} outstanding artifacts across "
                 f"{len(targets)} discovered targets"),
        "coverage": {"ok": not coverage_problems, "note": "; ".join(coverage_problems)},
        "detail": {
            "targets": targets,
            "required_cpu_hours_per_target": REQUIRED_CPU_HOURS,
            "cpu_hours": {t: runs.get(t, {}).get("cpu_hours", 0.0) for t in targets},
            "outstanding_artifacts": outstanding[:40],
            "triaged_and_fixed": len(triaged),
        },
    })


if __name__ == "__main__":
    raise SystemExit(main())
