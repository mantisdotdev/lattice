#!/usr/bin/env python3
"""
G0.1 — Prior-art analyses (HARD).

Target: one analysis per §3 entry (11 entries), all three mandated questions
answered in each, with substance.

Measured value = the number of §3 entries whose analysis satisfies EVERY
structural requirement. A file that exists but answers two of three questions
does not count; that is the whole point of measuring instead of asserting.
"""
from __future__ import annotations
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import mdcheck  # noqa: E402

DOCS = REPO / "docs" / "prior-art"

# The §3 list, verbatim in scope. Bundled entries must answer per-system where
# answers differ, so each bundle declares the systems its text must name.
REQUIRED = {
    "jujutsu": ["jujutsu", "jj"],
    "mercurial-evolve": ["mercurial", "obsolescence"],
    "pijul-darcs": ["pijul", "darcs"],
    "fossil": ["fossil"],
    "sapling": ["sapling"],
    "unison": ["unison"],
    "structural-diff-merge": ["semanticmerge", "difftastic", "gumtree"],
    "cdc-storage": ["restic", "borg", "casync", "xet"],
    "modern-git": ["partial clone", "sparse", "commit-graph"],
    "centralized-and-data": ["dolt", "perforce", "subversion"],
    "rust-git-implementations": ["gitoxide", "libgit2"],
}

MIN_WORDS = 400            # the gate floor, frozen in gates.toml's metric
MIN_SECTION_WORDS = 80     # per mandated question
MIN_SOURCES = 3


def check(slug: str, keywords: list[str]) -> tuple[bool, dict]:
    path = DOCS / f"{slug}.md"
    r: dict = {"slug": slug, "path": str(path.relative_to(REPO)), "problems": []}
    if not path.exists():
        r["problems"].append("missing")
        return False, r

    text = mdcheck.read(path)
    low = text.lower()
    secs = mdcheck.sections(text)
    r["words"] = mdcheck.substantive_words(text)

    if r["words"] < MIN_WORDS:
        r["problems"].append(f"{r['words']} substantive words < {MIN_WORDS}")

    # All three mandated questions, each with real content under it.
    for label, needles in (
        ("Q1 what it got right", ("what did it get right",)),
        ("Q2 why it didn't win", ("why didn't it win",)),
        ("Q3 what Lattice does differently", ("what will lattice do differently",)),
    ):
        hit = mdcheck.find_section(secs, *needles)
        if hit is None:
            r["problems"].append(f"no section for {label}")
            continue
        body_words = mdcheck.substantive_words(hit[1])
        r[f"words_{label.split()[0]}"] = body_words
        if body_words < MIN_SECTION_WORDS:
            r["problems"].append(
                f"{label} has {body_words} words < {MIN_SECTION_WORDS}")

    # Bundled entries must actually discuss each bundled system.
    absent = [k for k in keywords if k not in low]
    if absent:
        r["problems"].append(f"does not mention: {', '.join(absent)}")

    found_urls = mdcheck.urls(text)
    r["sources"] = len(found_urls)
    if len(found_urls) < MIN_SOURCES:
        r["problems"].append(f"{len(found_urls)} sourced URLs < {MIN_SOURCES}")

    # §3's purpose is to constrain design, so Q3 must reach into the spec.
    q3 = mdcheck.find_section(secs, "what will lattice do differently")
    if q3 and not any(t in q3[1].lower() for t in
                      ("g0.", "g1.", "g2.", "g3.", "g4.", "g5.", "g6.", "adr-")):
        r["problems"].append("Q3 cites no gate or ADR — not a checkable commitment")

    return not r["problems"], r


def main() -> int:
    details, passing = [], 0
    for slug, keywords in REQUIRED.items():
        ok, r = check(slug, keywords)
        details.append(r)
        passing += ok
    payload = {
        "gate": "G0.1",
        "value": passing,
        "unit": "analyses",
        "note": f"{passing}/{len(REQUIRED)} §3 entries fully satisfy the structural contract",
        "detail": {
            "required_entries": len(REQUIRED),
            "min_words": MIN_WORDS,
            "min_section_words": MIN_SECTION_WORDS,
            "min_sources": MIN_SOURCES,
            "total_substantive_words": sum(d.get("words", 0) for d in details),
            "entries": details,
        },
        "evidence": [d["path"] for d in details],
    }
    print(json.dumps(payload))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
