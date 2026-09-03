#!/usr/bin/env python3
"""Record SHA-256 for every committed eval template (§0.9 freeze).

Refuses to record an entry for a file that does not exist, so the freeze record
cannot claim to pin a template that was never written.
"""
from __future__ import annotations
import hashlib
import json
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
EVAL = REPO / "eval"
OUT = EVAL / "FREEZE.json"


def main() -> int:
    entries = {}
    for path in sorted(list((EVAL / "personas").glob("*.md"))
                       + list((EVAL / "reviewers").glob("*.md"))):
        if not path.is_file() or path.stat().st_size == 0:
            continue
        entries[str(path.relative_to(REPO))] = {
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "bytes": path.stat().st_size,
        }
    OUT.write_text(json.dumps({
        "frozen_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "note": "§0.9 requires templates be frozen before the consuming stage "
                "begins. Templates absent here are not yet written; a consuming "
                "harness must treat an absent entry as 'not frozen', never as "
                "'unchanged'.",
        "templates": entries,
    }, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"frozen": len(entries),
                      "templates": sorted(entries)}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
