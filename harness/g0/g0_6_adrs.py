#!/usr/bin/env python3
"""
G0.6 — Foundational ADRs (HARD).

Target: ADR-1 merge model, ADR-2 chunk parameters, ADR-3 store backend, ADR-4
daemon — each with prototype benchmarks or literature — plus the §0.3
calibration procedure frozen. Value = how many of those five artifacts are
structurally complete.

An ADR that decides without evidence is an opinion with a number on it, so the
harness requires either a committed prototype benchmark result or cited
literature, and rejects an ADR whose Decision section is empty.
"""
from __future__ import annotations
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import mdcheck  # noqa: E402

ADR_DIR = REPO / "docs" / "adr"
REQUIRED_ADRS = {
    "ADR-1": "merge model",
    "ADR-2": "chunk parameters",
    "ADR-3": "store backend",
    "ADR-4": "daemon",
}
REQUIRED_SECTIONS = ("context", "options", "decision", "consequences")
MIN_WORDS = 500
MIN_SECTION_WORDS = 60


def check_adr(num: str, topic: str) -> tuple[bool, dict]:
    matches = sorted(ADR_DIR.glob(f"{num.lower()}-*.md"))
    r = {"adr": num, "topic": topic, "problems": []}
    if not matches:
        r["problems"].append("missing")
        return False, r
    path = matches[0]
    r["path"] = str(path.relative_to(REPO))
    text = mdcheck.read(path)
    secs = mdcheck.sections(text)
    r["words"] = mdcheck.substantive_words(text)

    if r["words"] < MIN_WORDS:
        r["problems"].append(f"{r['words']} words < {MIN_WORDS}")

    for name in REQUIRED_SECTIONS:
        hit = mdcheck.find_section(secs, name)
        if hit is None:
            r["problems"].append(f"no '{name}' section")
        elif mdcheck.substantive_words(hit[1]) < MIN_SECTION_WORDS:
            r["problems"].append(f"'{name}' section is thin "
                                 f"({mdcheck.substantive_words(hit[1])} words)")

    # Evidence: a committed benchmark artifact, or cited literature.
    ev = mdcheck.find_section(secs, "evidence")
    bench_refs = [ln for ln in text.splitlines()
                  if "prototypes/" in ln or "bench/results/" in ln]
    n_urls = len(mdcheck.urls(text))
    r["benchmark_refs"] = len(bench_refs)
    r["sources"] = n_urls
    if not bench_refs and n_urls < 2:
        r["problems"].append("neither a committed prototype benchmark nor ≥2 cited sources")
    if ev is None:
        r["problems"].append("no 'Evidence' section")

    # A decision must actually decide.
    dec = mdcheck.find_section(secs, "decision")
    if dec and not any(w in dec[1].lower() for w in
                       ("we will", "chosen", "we choose", "decided", "adopt")):
        r["problems"].append("Decision section states no decision")

    return not r["problems"], r


def main() -> int:
    details, ok = [], 0
    for num, topic in REQUIRED_ADRS.items():
        good, r = check_adr(num, topic)
        ok += good
        details.append(r)

    # The calibration procedure, frozen before the G1 harness freeze (§0.3).
    calib = {"artifact": "calibration procedure", "problems": []}
    proc = REPO / "harness" / "lib" / "calibrate.py"
    ref = REPO / "bench" / "calibration-reference.json"
    env = REPO / "bench" / "ENVIRONMENT.md"
    if not proc.exists():
        calib["problems"].append("harness/lib/calibrate.py missing")
    if not ref.exists():
        calib["problems"].append("bench/calibration-reference.json not recorded")
    if not env.exists() or "calibration" not in mdcheck.read(env).lower():
        calib["problems"].append("bench/ENVIRONMENT.md does not document the procedure")
    if not calib["problems"]:
        ok += 1
    details.append(calib)

    print(json.dumps({
        "gate": "G0.6", "value": ok, "unit": "ADRs",
        "note": f"{ok}/5 foundational artifacts complete",
        "detail": {"required": len(REQUIRED_ADRS) + 1, "artifacts": details},
        "evidence": [d.get("path") for d in details if d.get("path")],
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
