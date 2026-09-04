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
  2. Every `"key": value` pair quoted inside a fenced JSON block must match the
     artifact that block describes — ALL of its numeric keys, not merely the
     ones the artifact happens to share. A block quoting one real figure beside
     one invented figure passed the earlier version.
  3. A block whose numbers are not a committed measurement — an illustrative
     shape, or a figure quoted from git history — must carry an
     `<!-- evidence: ... -->` marker saying where they come from. That is not
     an escape hatch: it converts "silently unchecked" into "attributed", which
     is the discipline this file exists to impose.

Rule 2 is deliberately narrow. It checks quoted JSON against committed output;
it cannot check prose, and a document can still overstate in sentences. It
removes the class of error where a document and its evidence simply disagree.
"""
from __future__ import annotations

import json
import re
import sys
from decimal import Decimal
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
ADR_DIR = REPO / "docs" / "adr"

ARTIFACT = re.compile(r"`?(bench/[\w./-]+\.json)`?")
JSON_BLOCK = re.compile(r"```json\n(.*?)```", re.S)
# A block whose numbers are not a committed measurement must say where they DO
# come from. Silently skipping such a block is how an unverifiable figure hides
# next to verified ones.
EVIDENCE_MARKER = re.compile(r"<!--\s*evidence:\s*(.+?)\s*-->", re.S)
# "key": number — strings are excluded, because prose quotes a note verbatim
# with the line wrapped and a wrapped string is not a mismatch. The value
# pattern accepts every JSON number form, exponents included: omitting `1e3`
# would let a changed result slip through unchecked.
PAIR = re.compile(r'"([\w_]+)"\s*:\s*(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)')


def exact(value) -> Decimal:
    """Compare as Decimal, never as float.

    float() collapses integers above 2^53 — 9007199254740992 and
    9007199254740993 become equal — so a changed result could compare clean.
    Decimal keeps every digit that was written.
    """
    return Decimal(str(value))


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
    checked_paths = checked_pairs = attributed = 0

    for adr in sorted(ADR_DIR.glob("*.md")):
        text = adr.read_text()
        rel = adr.relative_to(REPO)

        cited = []
        bench_root = (REPO / "bench").resolve()
        for m in ARTIFACT.finditer(text):
            path = (REPO / m.group(1)).resolve()
            checked_paths += 1
            # `bench/../../secrets.json` matches the pattern but is not a
            # benchmark artifact. Resolve first, then require containment, or
            # the scope this check enforces is decorative.
            if not path.is_relative_to(bench_root):
                problems.append(
                    f"{rel} cites {m.group(1)}, which resolves outside bench/ — "
                    f"an ADR's evidence must be a committed benchmark artifact")
            elif not path.is_file():
                problems.append(
                    f"{rel} cites {m.group(1)}, which does not exist — "
                    f"an ADR may not point at evidence that is not committed")
            else:
                cited.append(path)

        artifacts: dict[str, dict] = {}
        for path in cited:
            try:
                # Keyed by repository-relative path, not basename: two bench
                # directories may hold the same filename, and one would
                # silently overwrite the other.
                key = str(path.relative_to(REPO))
                artifacts[key] = flatten(json.loads(path.read_text()))
            except json.JSONDecodeError as e:
                problems.append(
                    f"{rel} cites {path.relative_to(REPO)}, which is not valid JSON: {e}")

        # A block is checked against the artifact it DESCRIBES, not against all
        # of them merged: an ADR may cite several runs, and merging would
        # compare a 300-trial block against whichever file happened to be last.
        # A block is satisfied if some cited artifact agrees on every numeric
        # key the two share.
        for m in JSON_BLOCK.finditer(text):
            block = m.group(1)
            pairs = [(k, exact(v)) for k, v in PAIR.findall(block)]
            if not pairs:
                continue
            # An explicit attribution IMMEDIATELY before the fence exempts the
            # block from artifact matching, and only that.
            #
            # "Immediately" is load-bearing. Searching the preceding text for
            # any marker would let one attached to an earlier block also exempt
            # the block after it: two fences close together, one marker, both
            # skipped. So take the LAST marker before this fence and require
            # nothing but whitespace between it and the fence.
            preamble = text[max(0, m.start() - 400):m.start()]
            markers = list(EVIDENCE_MARKER.finditer(preamble))
            if markers and not preamble[markers[-1].end():].strip():
                attributed += 1
                continue
            # The block describes the artifact it has the MOST keys in common
            # with, and that artifact must then agree on all of them. Accepting
            # any artifact that agrees on its shared keys is not enough: a file
            # sharing a single incidental key — `checkpoints_lost_total: 0`
            # appears in almost every run — would satisfy a block whose other
            # figures were wrong, which is exactly the error this check exists
            # to catch.
            keys = [k for k, _ in pairs]
            candidates = []
            for name, doc in artifacts.items():
                shared = [(k, v) for k, v in pairs
                          if k in doc and isinstance(doc[k], (int, float))
                          and not isinstance(doc[k], bool)]
                if shared:
                    miss = [(k, v, doc[k]) for k, v in shared if exact(doc[k]) != v]
                    candidates.append((len(shared), -len(miss), name, shared, miss))
            if not candidates:
                problems.append(
                    f"{rel} has a JSON block quoting {', '.join(keys)}, none of "
                    f"which appears in any artifact it cites — add an "
                    f"<!-- evidence: ... --> marker saying where the figures "
                    f"come from, or cite the artifact that measured them")
                continue
            _, _, name, shared, miss = max(candidates)
            checked_pairs += len(shared)
            for k, quoted, actual in miss:
                problems.append(
                    f'{rel} quotes "{k}": {quoted}, but {name} — the artifact '
                    f'it most closely describes — records {actual}')
            # Keys the chosen artifact does not measure at all. Previously these
            # were skipped, so a block could carry one verified figure and one
            # unverifiable one and still pass.
            for k in keys:
                if k not in artifacts[name]:
                    problems.append(
                        f'{rel} quotes "{k}" in a block describing {name}, '
                        f"which does not record that key — an ADR may not "
                        f"present an unmeasured number as measured")

    if problems:
        print("ADR evidence check FAILED:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1

    print(f"ADR evidence OK: {checked_paths} artifact reference(s) resolve, "
          f"{checked_pairs} quoted value(s) match their artifact, "
          f"{attributed} block(s) attributed to a non-artifact source")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
