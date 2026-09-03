#!/usr/bin/env python3
"""
Prove that every scorecard in GAUNTLET.md was generated from committed harness
output and not typed by hand.

For each `### Iteration N` section found in GAUNTLET.md, the corresponding
bench/results/iteration-N.json must exist, and re-rendering it must reproduce
the section byte-for-byte. A hand-edited row fails here.
"""
from __future__ import annotations
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import gauntlet as G  # noqa: E402

SECTION = re.compile(r"^### Iteration (\d+) — ", re.M)


def main() -> int:
    if not G.GAUNTLET_MD.exists():
        print("GAUNTLET.md missing — it is part of the delivery (§0.6)", file=sys.stderr)
        return 1
    text = G.GAUNTLET_MD.read_text()
    starts = [(int(m.group(1)), m.start()) for m in SECTION.finditer(text)]
    if not starts:
        print("no iterations recorded in GAUNTLET.md yet — nothing to verify")
        return 0

    problems: list[str] = []
    for idx, (n, start) in enumerate(starts):
        end = starts[idx + 1][1] if idx + 1 < len(starts) else len(text)
        recorded = text[start:end].strip()
        results_file = G.RESULTS_DIR / f"iteration-{n}.json"
        if not results_file.exists():
            problems.append(f"iteration {n} appears in GAUNTLET.md but "
                            f"{results_file.relative_to(REPO)} is missing — "
                            f"a scorecard without harness output is a lie (§0.1)")
            continue
        payload = json.loads(results_file.read_text())
        prev_file = G.RESULTS_DIR / f"iteration-{n-1}.json"
        prev = json.loads(prev_file.read_text()) if prev_file.exists() else None
        expected = G.render(payload, prev).strip()
        if expected != recorded:
            problems.append(f"iteration {n} in GAUNTLET.md does not match a fresh "
                            f"render of its results file — the table was edited by hand")

    if problems:
        print("SCORECARD INTEGRITY FAILURE:", file=sys.stderr)
        for p in problems:
            print(f"  ✗ {p}", file=sys.stderr)
        return 1
    print(f"scorecard integrity OK: {len(starts)} iteration(s) reproduce exactly "
          f"from committed harness output")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
