#!/usr/bin/env python3
"""
G0.8 — Revised-spec package (SOFT [S]).

Target: the spec with G0.2 resolutions merged, plus the disagreements memo,
delivered to the stakeholder before G1 entry.

[S] gate: the autonomous floor is that the package exists, is complete, and is
addressed to the stakeholder as a committed file (§0.7). Whether the stakeholder
accepts it is the human part, so the harness reports human_kit and the runner
holds the gate at PASS-PENDING-HUMAN until a response is recorded.
"""
from __future__ import annotations
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import mdcheck  # noqa: E402

STAKEHOLDER = REPO / "STAKEHOLDER"
MEMO = REPO / "docs" / "DISAGREEMENTS.md"
REVISED = REPO / "docs" / "SPEC-REVISED.md"


def main() -> int:
    problems, present = [], []
    delivery = sorted(STAKEHOLDER.glob("001-*.md")) if STAKEHOLDER.exists() else []

    if not delivery:
        problems.append("no STAKEHOLDER/001-*.md delivery note")
    else:
        present.append(str(delivery[0].relative_to(REPO)))
        text = mdcheck.read(delivery[0])
        low = text.lower()
        for token, why in (
            ("disagreement", "does not reference the disagreements memo"),
            ("resolution", "does not summarise resolutions"),
            ("g1", "does not state the G1-entry consequence"),
        ):
            if token not in low:
                problems.append(f"delivery note {why}")
        if mdcheck.substantive_words(text) < 250:
            problems.append("delivery note is thin (<250 words)")

    if not MEMO.exists():
        problems.append("docs/DISAGREEMENTS.md missing")
    else:
        present.append(str(MEMO.relative_to(REPO)))

    if not REVISED.exists():
        problems.append("docs/SPEC-REVISED.md missing (spec with resolutions merged)")
    else:
        present.append(str(REVISED.relative_to(REPO)))
        rev = mdcheck.read(REVISED)
        if "challenge" not in rev.lower():
            problems.append("revised spec does not trace back to the challenges")

    response = sorted(STAKEHOLDER.glob("*-response*.md")) if STAKEHOLDER.exists() else []

    print(json.dumps({
        "gate": "G0.8",
        "value": 0 if problems else 1,
        "unit": "package",
        "note": "; ".join(problems) if problems else "package complete and delivered",
        "human_kit": present[0] if present and not problems else None,
        "human_results_present": bool(response),
        "detail": {"problems": problems, "artifacts": present,
                   "stakeholder_response": [str(p.relative_to(REPO)) for p in response]},
        "evidence": present,
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
