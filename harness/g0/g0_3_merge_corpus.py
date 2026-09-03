#!/usr/bin/env python3
"""
G0.3 — Merge-replay corpus + oracle (HARD).

Target: ≥10,000 real merge commits mined from ≥20 OSS repos, ≥5 languages;
line-based conflict AND line-based silent-mis-merge baselines measured; the
oracle enumerated and hash-pinned before any G4 work.

The measured value is the mined merge count, but it is reported as 0 unless
every structural precondition holds — repo count, language count, oracle pinned,
and both baselines actually measured. A corpus without its baselines is not the
thing this gate asks for.
"""
from __future__ import annotations
import hashlib
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
MERGES = REPO / "corpus" / "data" / "merges.jsonl"
BASELINES = REPO / "corpus" / "data" / "merge-baselines.json"
ORACLE = REPO / "corpus" / "manifests" / "g0-3-oracle.md"
ORACLE_PIN = REPO / "corpus" / "manifests" / "g0-3-oracle.sha256"

MIN_REPOS = 20
MIN_LANGUAGES = 5


def fail(note: str, detail=None) -> int:
    print(json.dumps({"gate": "G0.3", "value": 0, "unit": "merge commits",
                      "note": note, "detail": detail or {}}))
    return 0


def main() -> int:
    if not MERGES.exists():
        return fail("corpus/data/merges.jsonl not built — run scripts/corpus/mine_merges.py")
    if not ORACLE.exists() or not ORACLE_PIN.exists():
        return fail("oracle definition or its hash pin is missing")

    actual = hashlib.sha256(ORACLE.read_bytes()).hexdigest()
    pinned = ORACLE_PIN.read_text().split()[0]
    if actual != pinned:
        return fail(f"ORACLE HASH MISMATCH — the oracle changed after pinning "
                    f"(pinned {pinned[:12]}, actual {actual[:12]})")

    repos, langs, count = set(), set(), 0
    with MERGES.open() as fh:
        for line in fh:
            if not line.strip():
                continue
            r = json.loads(line)
            repos.add(r["repo"])
            langs.add(r["language"])
            count += 1

    problems = []
    if len(repos) < MIN_REPOS:
        problems.append(f"{len(repos)} repos < {MIN_REPOS} required")
    if len(langs) < MIN_LANGUAGES:
        problems.append(f"{len(langs)} languages < {MIN_LANGUAGES} required")

    baselines = None
    if not BASELINES.exists():
        problems.append("baselines not measured (run scripts/corpus/replay_merges.py)")
    else:
        b = json.loads(BASELINES.read_text())
        if b.get("oracle_sha256") != actual:
            problems.append("baselines were measured under a different oracle revision")
        baselines = b.get("baselines", {})
        if baselines.get("line_conflict_rate_pct") is None:
            problems.append("line-based conflict baseline missing")
        if baselines.get("divergence_baseline_pct") is None:
            problems.append("line-based divergence (mis-merge) baseline missing")

    detail = {
        "mined_merges": count,
        "repos": len(repos),
        "repo_list": sorted(repos),
        "languages": sorted(langs),
        "oracle_sha256": actual,
        "baselines": baselines,
        "problems": problems,
    }
    print(json.dumps({
        "gate": "G0.3",
        "value": 0 if problems else count,
        "unit": "merge commits",
        "note": ("; ".join(problems) if problems else
                 f"{count} merges, {len(repos)} repos, {len(langs)} languages; "
                 f"line conflict rate "
                 f"{baselines.get('line_conflict_rate_pct')}%, divergence baseline "
                 f"{baselines.get('divergence_baseline_pct')}%"),
        "detail": detail,
        "evidence": [str(MERGES.relative_to(REPO)), str(ORACLE.relative_to(REPO))]
                    + ([str(BASELINES.relative_to(REPO))] if BASELINES.exists() else []),
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
