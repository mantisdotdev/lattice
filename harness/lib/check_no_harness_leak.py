#!/usr/bin/env python3
"""
§0.3 anti-gaming, mechanised.

  "no product code path may detect or special-case harness execution
   (grep-able rule: no harness/test identifiers referenced in `ltx-core`)"

Product code is scanned for any means of noticing that it is being measured.
Rust `#[cfg(test)]` unit tests inside ltx-core are legitimate and exempt; what
is forbidden is *runtime* awareness — env vars, path sniffing, or references to
the harness by name in code that ships.
"""
from __future__ import annotations
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PRODUCT = [REPO / "crates" / "ltx-core", REPO / "crates" / "ltx",
           REPO / "crates" / "ltx-daemon"]

FORBIDDEN = [
    (re.compile(r"\bGAUNTLET_[A-Z_]+\b"), "reads a Gauntlet environment variable"),
    (re.compile(r"\bLTX_(?:BENCH|HARNESS|GAUNTLET)[A-Z_]*\b"), "reads a harness environment variable"),
    (re.compile(r"""["'][^"'\n]*\bharness\b[^"'\n]*["']""", re.I), "string mentions the harness"),
    (re.compile(r"""["'][^"'\n]*/bench/[^"'\n]*["']"""), "string references the bench tree"),
    (re.compile(r"\bcfg!\s*\(\s*test\s*\)"), "branches on cfg!(test) at runtime"),
    (re.compile(r"\bstd::env::var\w*\s*\(\s*[\"']LTX_TEST"), "reads a test-mode env var"),
]

# `#[cfg(test)] mod tests { ... }` blocks are legitimate; strip them first.
CFG_TEST_MOD = re.compile(r"#\[cfg\(test\)\]\s*mod\s+\w+\s*\{", re.M)


def strip_test_modules(src: str) -> str:
    out, i = [], 0
    for m in CFG_TEST_MOD.finditer(src):
        out.append(src[i:m.start()])
        depth, j = 1, m.end()
        while j < len(src) and depth:
            if src[j] == "{":
                depth += 1
            elif src[j] == "}":
                depth -= 1
            j += 1
        i = j
    out.append(src[i:])
    return "".join(out)


def main() -> int:
    findings: list[str] = []
    scanned = 0
    for root in PRODUCT:
        if not root.exists():
            continue
        for path in sorted(root.rglob("*.rs")):
            scanned += 1
            src = strip_test_modules(path.read_text(errors="replace"))
            for lineno, line in enumerate(src.splitlines(), 1):
                if line.lstrip().startswith("//"):
                    continue
                for pattern, why in FORBIDDEN:
                    if pattern.search(line):
                        findings.append(
                            f"{path.relative_to(REPO)}:{lineno}: {why}\n      {line.strip()}")

    if findings:
        print("§0.3 VIOLATION — product code can detect harness execution:", file=sys.stderr)
        for f in findings:
            print(f"  ✗ {f}", file=sys.stderr)
        return 1
    print(f"§0.3 anti-gaming check clean ({scanned} product source files scanned)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
