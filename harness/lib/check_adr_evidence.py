#!/usr/bin/env python3
"""
Every measurement an ADR cites must exist, and every number it quotes from one
must match.

This check exists because of a specific, repeated failure. While recording
ADR-18, five successive review rounds found claims that outran their evidence:
a statistical inference stated as proof, a scope of 1,000 trials where 573 were
injected, an explanation of crash positions the artifact never recorded, and —
worst — a placeholder figure of 141 written into the document *before the run
that would produce it had finished*. The real number was 130.

None of those was caught by a test, because nothing mechanically connected the
prose to the output. This is that connection:

  1. Every `bench/**.json` path an ADR names must exist.
  2. Every `"key": value` pair quoted inside a fenced JSON block in an ADR must
     match the artifact that ADR cites, when the key is present in it.

Rule 2 is deliberately narrow. It checks quoted JSON against committed output;
it cannot check prose, and a document can still overstate in sentences. It
removes the class of error where a document and its evidence simply disagree.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
ADR_DIR = REPO / "docs" / "adr"

ARTIFACT = re.compile(r"`?(bench/[\w./-]+\.json)`?")
JSON_BLOCK = re.compile(r"```json\n(.*?)```", re.S)
# "key": number  — strings are excluded: prose often quotes a note verbatim
# with the line wrapped, and a wrapped string is not a mismatch.
PAIR = re.compile(r'"([\w_]+)"\s*:\s*(-?\d+(?:\.\d+)?)')


def flatten(doc, prefix=""):
    """Every scalar in a document, by key name, so a quoted pair can be found
    wherever the artifact happens to nest it."""
    out = {}
    if isinstance(doc, dict):
        for k, v in doc.items():
            if isinstance(v, (dict, list)):
                out.update(flatten(v, k))
            else:
                out[k] = v
    elif isinstance(doc, list):
        for v in doc:
            out.update(flatten(v, prefix))
    return out


def main() -> int:
    if not ADR_DIR.is_dir():
        print("no docs/adr — nothing to check")
        return 0

    problems: list[str] = []
    checked_paths = checked_pairs = 0

    for adr in sorted(ADR_DIR.glob("*.md")):
        text = adr.read_text()
        rel = adr.relative_to(REPO)

        cited = []
        for m in ARTIFACT.finditer(text):
            path = REPO / m.group(1)
            checked_paths += 1
            if not path.is_file():
                problems.append(
                    f"{rel} cites {m.group(1)}, which does not exist — "
                    f"an ADR may not point at evidence that is not committed")
            else:
                cited.append(path)

        if not cited:
            continue

        artifacts: dict[str, dict] = {}
        for path in cited:
            try:
                artifacts[path.name] = flatten(json.loads(path.read_text()))
            except json.JSONDecodeError as e:
                problems.append(f"{rel} cites {path.name}, which is not valid JSON: {e}")

        # A block is checked against the artifact it DESCRIBES, not against all
        # of them merged: an ADR may cite several runs, and merging would
        # compare a 300-trial block against whichever file happened to be last.
        # A block is satisfied if some cited artifact agrees on every numeric
        # key the two share.
        for block in JSON_BLOCK.findall(text):
            pairs = [(k, float(v)) for k, v in PAIR.findall(block)]
            if not pairs:
                continue
            # The block describes the artifact it has the MOST keys in common
            # with, and that artifact must then agree on all of them. Accepting
            # any artifact that agrees on its shared keys is not enough: a file
            # sharing a single incidental key — `checkpoints_lost_total: 0`
            # appears in almost every run — would satisfy a block whose other
            # figures were wrong, which is exactly the error this check exists
            # to catch.
            candidates = []
            for name, doc in artifacts.items():
                shared = [(k, v) for k, v in pairs
                          if k in doc and isinstance(doc[k], (int, float))
                          and not isinstance(doc[k], bool)]
                if shared:
                    miss = [(k, v, doc[k]) for k, v in shared if float(doc[k]) != v]
                    candidates.append((len(shared), -len(miss), name, shared, miss))
            if not candidates:
                continue
            _, _, name, shared, miss = max(candidates)
            checked_pairs += len(shared)
            for k, quoted, actual in miss:
                problems.append(
                    f'{rel} quotes "{k}": {quoted:g}, but {name} — the artifact '
                    f'it most closely describes — records {actual}')

    if problems:
        print("ADR evidence check FAILED:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1

    print(f"ADR evidence OK: {checked_paths} artifact reference(s) resolve, "
          f"{checked_pairs} quoted value(s) match their artifact")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
