#!/usr/bin/env python3
"""Fail CI if the gate registry is malformed, incomplete, or self-inconsistent."""
from __future__ import annotations
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gauntlet as G

# §6 names the performance gates explicitly. The registry must agree with the
# spec, or the waiver cap in §0.5 is being computed over the wrong set.
SPEC_PERF_GATES = {"G1.5", "G1.6", "G1.7", "G1.8", "G1.9", "G4.6", "G5.4", "G5.8"}
SPEC_TIMING_GATES = {"G1.5", "G1.6", "G1.7", "G4.6", "G5.4"}
SPEC_STAGE_COUNTS = {"G0": 8, "G1": 12, "G2": 8, "G3": 6, "G4": 7, "G5": 8, "G6": 5}
VALID_CMP = {">=", "<=", ">", "<", "=="}
VALID_TYPE = {"HARD", "SOFT"}
VALID_KLASS = {"A", "S", "A+S"}


def main() -> int:
    problems: list[str] = []
    gates = G.load_gates()

    counts: dict[str, int] = {}
    for gid, gate in gates.items():
        counts[gate.stage] = counts.get(gate.stage, 0) + 1
        if gate.cmp not in VALID_CMP:
            problems.append(f"{gid}: bad comparator {gate.cmp!r}")
        if gate.type not in VALID_TYPE:
            problems.append(f"{gid}: bad type {gate.type!r}")
        if gate.klass not in VALID_KLASS:
            problems.append(f"{gid}: bad class {gate.klass!r}")
        if not gate.metric.strip():
            problems.append(f"{gid}: empty metric")
        if not gate.harness:
            problems.append(f"{gid}: no harness argv — a gate without a harness "
                            f"does not exist (§0.1)")
        if gate.timing and not gate.perf:
            problems.append(f"{gid}: marked timing but not perf")

    perf = {gid for gid, g in gates.items() if g.perf}
    if perf != SPEC_PERF_GATES:
        problems.append(f"perf-gate set disagrees with §6: registry={sorted(perf)} "
                        f"spec={sorted(SPEC_PERF_GATES)}")
    timing = {gid for gid, g in gates.items() if g.timing}
    if timing != SPEC_TIMING_GATES:
        problems.append(f"timing-gate set disagrees with §6: registry={sorted(timing)} "
                        f"spec={sorted(SPEC_TIMING_GATES)}")
    if counts != SPEC_STAGE_COUNTS:
        problems.append(f"stage gate counts disagree with §6: {counts} != {SPEC_STAGE_COUNTS}")

    # A HARD gate must never carry a waiver (§0.5). Catch it at registry level
    # as well as at evaluation level.
    for gid, w in G.load_waivers().items():
        if gid not in gates:
            problems.append(f"waiver for unknown gate {gid}")
        elif gates[gid].type == "HARD":
            problems.append(f"waiver present for HARD gate {gid} — forbidden (§0.5)")

    if problems:
        print("REGISTRY INVALID:", file=sys.stderr)
        for p in problems:
            print(f"  ✗ {p}", file=sys.stderr)
        return 1
    print(f"registry OK: {len(gates)} gates, "
          f"{sum(1 for g in gates.values() if g.type=='HARD')} HARD, "
          f"{sum(1 for g in gates.values() if g.type=='SOFT')} SOFT, "
          f"{len(perf)} perf")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
