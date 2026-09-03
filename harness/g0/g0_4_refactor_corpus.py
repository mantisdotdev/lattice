#!/usr/bin/env python3
"""
G0.4 — Refactoring corpus (HARD).

Target: ≥1,000 mined rename/move instances, ≥10 repos, ≥3 languages,
ground-truthed.

Only TIER_A (byte-identical content at a new path) and TIER_B (similarity ≥0.90
under an independently computed measure) count. TIER_C candidates are mined but
excluded, so the corpus cannot be inflated with weak matches — which matters
because G4.1 scores Lattice's matcher against this corpus at ≥99% precision, and
a corpus containing junk would make that number meaningless.
"""
from __future__ import annotations
import collections
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DATA = REPO / "corpus" / "data" / "refactorings.jsonl"
SPOT = REPO / "corpus" / "data" / "refactorings-spotcheck.json"
MIN_REPOS = 10
MIN_LANGS = 3
GROUND_TRUTH_TIERS = ("TIER_A", "TIER_B")


def main() -> int:
    if not DATA.exists():
        print(json.dumps({"gate": "G0.4", "value": 0, "unit": "instances",
                          "note": "corpus/data/refactorings.jsonl not built"}))
        return 0

    rows = [json.loads(l) for l in DATA.read_text().splitlines() if l.strip()]
    truth = [r for r in rows if r["tier"] in GROUND_TRUTH_TIERS]
    repos = {r["repo"] for r in truth}
    langs = {r["language"] for r in truth}

    problems = []
    if len(repos) < MIN_REPOS:
        problems.append(f"{len(repos)} repos < {MIN_REPOS}")
    if len(langs) < MIN_LANGS:
        problems.append(f"{len(langs)} languages < {MIN_LANGS}")
    if not SPOT.exists():
        problems.append("no spot-check sample committed — the automatic ground-truth "
                        "derivation would be unauditable")

    kinds = collections.Counter(r["kind"] for r in truth)
    detail = {
        "ground_truth_instances": len(truth),
        "candidates_total": len(rows),
        "by_tier": dict(collections.Counter(r["tier"] for r in rows)),
        "by_language": dict(collections.Counter(r["language"] for r in truth)),
        "by_repo": dict(collections.Counter(r["repo"] for r in truth)),
        "by_kind": dict(kinds),
        "cross_directory_moves": kinds.get("cross_directory_move", 0),
        "problems": problems,
    }
    print(json.dumps({
        "gate": "G0.4",
        "value": 0 if problems else len(truth),
        "unit": "instances",
        "note": ("; ".join(problems) if problems else
                 f"{len(truth)} ground-truthed instances across {len(repos)} repos, "
                 f"{len(langs)} languages; "
                 f"{kinds.get('cross_directory_move', 0)} are cross-directory moves"),
        "detail": detail,
        "evidence": [str(DATA.relative_to(REPO))],
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
