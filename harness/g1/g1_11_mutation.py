#!/usr/bin/env python3
"""
G1.11 — Mutation testing (SOFT).

Target: >= 80% mutation-kill rate on ltx-core's store, op-log and merge modules.

Scoped to those three modules deliberately: they are where a silent behavioural
change loses data, and a repo-wide number would let a well-tested CLI mask an
under-tested store.

Equivalent mutants (semantically identical to the original, so no test CAN kill
them) are excluded ONLY when listed in a committed adjudication file with a
written reason. An independent review noted the brief specifies 80% without
saying how equivalents are handled, and the permissive reading -- excluding
anything that survives -- makes any percentage reachable.
"""
from __future__ import annotations
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import ltxrun as L  # noqa: E402

GATE = "G1.11"
CORE = REPO / "crates" / "ltx-core" / "src"
SCOPED_MODULES = ["store", "oplog", "merge"]
EQUIVALENTS = REPO / "crates" / "ltx-core" / "equivalent-mutants.json"
OUTPUT = REPO / "bench" / "results" / "raw" / "mutants.json"


def main() -> int:
    if not CORE.exists():
        return L.not_implemented(
            GATE, "ltx-core does not exist yet (Stage G0 forbids product code)")
    if not any((CORE / m).exists() or (CORE / f"{m}.rs").exists()
               for m in SCOPED_MODULES):
        return L.not_implemented(
            GATE, f"none of the scoped modules exist yet: "
                  f"{', '.join(SCOPED_MODULES)}")

    exe = subprocess.run(["cargo", "mutants", "--version"],
                         capture_output=True, text=True)
    if exe.returncode != 0:
        return L.not_implemented(
            GATE, "cargo-mutants not installed (cargo install cargo-mutants)")

    files = [f"crates/ltx-core/src/{m}**" for m in SCOPED_MODULES]
    args = ["cargo", "mutants", "--no-shuffle", "--json", "--output",
            str(OUTPUT.parent)]
    for f in files:
        args += ["--file", f]
    proc = subprocess.run(args, cwd=REPO, capture_output=True, text=True,
                          errors="replace", timeout=86400)

    summary = OUTPUT.parent / "mutants.out" / "outcomes.json"
    if not summary.exists():
        return L.emit({
            "gate": GATE, "value": 0.0, "unit": "percent",
            "note": f"cargo-mutants produced no outcomes file "
                    f"(exit {proc.returncode}): {proc.stderr.strip()[:200]}"})
    doc = json.loads(summary.read_text())
    outcomes = doc.get("outcomes", [])

    equivalents = set()
    reasons = {}
    if EQUIVALENTS.exists():
        adjudicated = json.loads(EQUIVALENTS.read_text())
        for entry in adjudicated.get("equivalents", []):
            if entry.get("reason"):
                equivalents.add(entry["mutant"])
                reasons[entry["mutant"]] = entry["reason"]

    killed = missed = unviable = 0
    unadjudicated: list[str] = []
    for o in outcomes:
        name = o.get("scenario", {}).get("Mutant", {}).get("function", {}).get("function_name", "")
        summary_line = json.dumps(o.get("scenario", {}))[:200]
        status = o.get("summary", "")
        if status == "CaughtMutant":
            killed += 1
        elif status == "MissedMutant":
            if summary_line in equivalents:
                unviable += 1
            else:
                missed += 1
                unadjudicated.append(summary_line)
        elif status in ("Unviable", "Timeout"):
            unviable += 1

    total = killed + missed
    rate = (100.0 * killed / total) if total else 0.0
    return L.emit({
        "gate": GATE, "value": round(rate, 2), "unit": "percent",
        "note": (f"{killed} killed / {total} viable = {rate:.1f}% on "
                 f"{', '.join(SCOPED_MODULES)}; {len(equivalents)} adjudicated "
                 f"equivalents excluded"),
        "detail": {
            "scoped_modules": SCOPED_MODULES,
            "killed": killed, "missed": missed, "unviable": unviable,
            "adjudicated_equivalents": len(equivalents),
            "surviving_unadjudicated": unadjudicated[:25],
        },
        "evidence": [str(summary.relative_to(REPO))],
    })


if __name__ == "__main__":
    raise SystemExit(main())
