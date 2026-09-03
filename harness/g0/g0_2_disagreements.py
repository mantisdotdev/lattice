#!/usr/bin/env python3
"""
G0.2 — Disagreements memo (HARD).

Target: ≥5 substantive challenges to the spec, each resolved with evidence or
accepted-as-bet in writing (§0.7).

A challenge counts only if it is structurally complete:
  - it names the specific spec claim it challenges (with a § or gate reference),
  - it carries evidence, not just an opinion,
  - it reaches one of the three permitted resolutions, and
  - if the resolution is ACCEPTED-AS-BET, it states a falsification condition,
    because a bet you cannot lose is not a bet.
"""
from __future__ import annotations
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import mdcheck  # noqa: E402

MEMO = REPO / "docs" / "DISAGREEMENTS.md"
CHALLENGE_HEAD = re.compile(r"^challenge\s+(\d+)\s*[—:-]\s*(.+)$", re.I)
SPEC_REF = re.compile(r"(§\s?\d+(\.\d+)*|\bG\d\.\d+\b|\bADR-\d+\b)")
RESOLUTIONS = ("ACCEPTED", "REJECTED", "ACCEPTED-AS-BET")
MIN_CHALLENGE_WORDS = 120


def main() -> int:
    if not MEMO.exists():
        print(json.dumps({
            "gate": "G0.2", "value": 0, "unit": "challenges",
            "note": f"{MEMO.relative_to(REPO)} does not exist",
            "detail": {"challenges": []}}))
        return 0

    text = mdcheck.read(MEMO)
    secs = mdcheck.sections(text)
    results, valid = [], 0

    for head, body in secs.items():
        m = CHALLENGE_HEAD.match(head.strip())
        if not m:
            continue
        r = {"n": int(m.group(1)), "title": m.group(2).strip(), "problems": []}
        low = body.lower()
        r["words"] = mdcheck.substantive_words(body)

        if r["words"] < MIN_CHALLENGE_WORDS:
            r["problems"].append(f"{r['words']} words < {MIN_CHALLENGE_WORDS}")

        # Must challenge something specific in the spec.
        refs = SPEC_REF.findall(body + " " + head)
        r["spec_refs"] = len(refs)
        if not refs:
            r["problems"].append("cites no §/gate/ADR — not a challenge to anything specific")

        for field in ("claim challenged", "evidence", "resolution"):
            if f"**{field}" not in low:
                r["problems"].append(f"missing **{field.title()}** block")

        # Exactly one permitted resolution verdict, stated explicitly.
        found = [v for v in ("ACCEPTED-AS-BET", "REJECTED", "ACCEPTED")
                 if re.search(rf"\*\*Resolution[^*]*\*\*[^\n]*{v}", body)
                 or re.search(rf"^\s*{v}\b", body, re.M)]
        # ACCEPTED-AS-BET contains ACCEPTED; prefer the most specific match.
        verdict = found[0] if found else None
        r["resolution"] = verdict
        if verdict is None:
            r["problems"].append(
                f"no explicit resolution verdict (one of {', '.join(RESOLUTIONS)})")
        elif verdict == "ACCEPTED-AS-BET" and "falsif" not in low:
            r["problems"].append("accepted-as-bet without a falsification condition (§0.7)")

        if not r["problems"]:
            valid += 1
        results.append(r)

    print(json.dumps({
        "gate": "G0.2",
        "value": valid,
        "unit": "challenges",
        "note": f"{valid} structurally complete challenges of {len(results)} present",
        "detail": {
            "min_words_per_challenge": MIN_CHALLENGE_WORDS,
            "total_present": len(results),
            "by_resolution": {v: sum(1 for r in results if r["resolution"] == v)
                              for v in RESOLUTIONS},
            "challenges": results,
        },
        "evidence": [str(MEMO.relative_to(REPO))],
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
