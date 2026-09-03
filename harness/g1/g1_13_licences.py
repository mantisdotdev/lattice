#!/usr/bin/env python3
"""
G1.13 — Licence compatibility (HARD).

ADDED under docs/DISAGREEMENTS.md Challenge 16. §8 requires Apache-2.0 and flags
"check tree-sitter grammar licenses" as a parenthetical with no gate behind it.
Every other project-level obligation in the brief has a gate; this one did not.

The exposure is concrete rather than theoretical: tree-sitter grammars are
individually licensed and several widely-used ones are not permissive; RocksDB
(a §5.1 store-backend candidate) is dual-licensed GPLv2/Apache-2.0; libgit2 is
GPLv2-with-linking-exception. A licence violation found after launch cannot be
fixed by a patch release -- it can require removing a language, or a backend,
from a shipped product.

Measured value = the number of dependencies whose licence falls outside an
allowlist of licences compatible with Apache-2.0 distribution. Target 0.

Two deliberate strictnesses:

  1. A dependency whose licence cannot be DETERMINED counts as a violation, not
     as a pass. "We could not tell" is the state a licence audit exists to
     eliminate.
  2. Vendored tree-sitter grammars are scanned separately from the Cargo graph,
     because a grammar checked into the tree does not appear in `cargo metadata`
     and would otherwise be invisible to exactly the check §8 asked for.
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CARGO_TOML = REPO / "Cargo.toml"
GRAMMAR_DIRS = [REPO / "vendor" / "grammars", REPO / "grammars"]

# Licences compatible with distributing an Apache-2.0 binary. Weak-copyleft
# entries (MPL-2.0, LGPL with linking exception) are file-level or linking-scoped
# and are permitted; strong copyleft is not.
ALLOWED = {
    "Apache-2.0", "MIT", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib",
    "Unlicense", "CC0-1.0", "MPL-2.0", "BSL-1.0", "Unicode-3.0",
    "Unicode-DFS-2016", "OpenSSL", "Apache-2.0 WITH LLVM-exception",
    "0BSD", "CDLA-Permissive-2.0", "NCSA",
}
# Named explicitly so the report can distinguish "incompatible" from "unknown".
KNOWN_INCOMPATIBLE = {
    "GPL-2.0", "GPL-3.0", "AGPL-3.0", "LGPL-2.1", "LGPL-3.0",
    "GPL-2.0-only", "GPL-3.0-only", "AGPL-3.0-only", "SSPL-1.0",
    "BUSL-1.1", "Elastic-2.0", "CC-BY-NC-4.0",
}
SPDX_OR = re.compile(r"\s+OR\s+|/", re.I)
SPDX_AND = re.compile(r"\s+AND\s+", re.I)
LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING)", re.I)


def not_implemented(note: str) -> int:
    print(json.dumps({"gate": "G1.13", "status": "not-implemented", "note": note}))
    return 0


def classify(expr: str | None) -> tuple[str, str]:
    """Return (verdict, reason) for an SPDX expression.

    OR and AND are NOT interchangeable, and an earlier version treated them as
    if they were:
      - "MIT OR Apache-2.0" is a choice, so it passes if ANY term is allowed.
      - "GPL-3.0-only AND MIT" imposes BOTH obligations, so it passes only if
        EVERY term is allowed. Treating it like OR made a GPL dependency
        report as clean.
    """
    if not expr or not expr.strip():
        return "unknown", "no licence declared"

    def term_verdict(term: str) -> str:
        term = term.strip().strip("()")
        if term in ALLOWED:
            return "allowed"
        if term in KNOWN_INCOMPATIBLE:
            return "incompatible"
        return "unknown"

    # Each OR alternative is itself a conjunction; an alternative is acceptable
    # only if every one of its conjuncts is.
    alternatives = [a for a in SPDX_OR.split(expr) if a.strip()]
    verdicts = []
    for alt in alternatives:
        conjuncts = [c for c in SPDX_AND.split(alt) if c.strip()]
        per = [term_verdict(c) for c in conjuncts]
        if all(v == "allowed" for v in per):
            verdicts.append("allowed")
        elif any(v == "incompatible" for v in per):
            verdicts.append("incompatible")
        else:
            verdicts.append("unknown")

    if "allowed" in verdicts:
        return "allowed", expr
    if all(v == "incompatible" for v in verdicts):
        return "incompatible", expr
    return "unknown", expr


def scan_cargo() -> tuple[list[dict], str] | None:
    """Returns (deps, source), or None when there is no workspace to scan.

    A workspace that EXISTS but cannot be read raises, so the caller can fail
    the gate: silently treating an unreadable dependency graph as an empty one
    would report 0 violations having measured nothing.
    """
    if not CARGO_TOML.exists():
        return None
    r = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features"],
        cwd=REPO, capture_output=True, text=True, errors="replace", timeout=900)
    if r.returncode != 0:
        raise RuntimeError(f"cargo metadata failed: {r.stderr.strip()[:300]}")
    try:
        meta = json.loads(r.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"cargo metadata emitted unparseable JSON: {exc}")

    workspace = set(meta.get("workspace_members", []))
    out = []
    for pkg in meta.get("packages", []):
        if pkg.get("id") in workspace:
            continue  # our own crates; licensed by §8
        verdict, expr = classify(pkg.get("license"))
        if verdict != "allowed" and pkg.get("license_file"):
            verdict, expr = "unknown", f"license_file: {pkg['license_file']}"
        out.append({"name": pkg.get("name"), "version": pkg.get("version"),
                    "licence": expr, "verdict": verdict,
                    "source": "cargo"})
    return out, "cargo metadata"


def scan_grammars() -> list[dict]:
    """Vendored tree-sitter grammars, which never appear in `cargo metadata`."""
    out = []
    for root in GRAMMAR_DIRS:
        if not root.exists():
            continue
        for grammar in sorted(p for p in root.iterdir() if p.is_dir()):
            licence = None
            for child in grammar.iterdir():
                if child.is_file() and LICENSE_FILE.match(child.name):
                    head = child.read_text(errors="replace")[:4000]
                    for spdx in list(ALLOWED) + list(KNOWN_INCOMPATIBLE):
                        if spdx.lower().replace("-", " ") in \
                                head.lower().replace("-", " "):
                            licence = spdx
                            break
                    if licence:
                        break
            verdict, expr = classify(licence)
            out.append({"name": grammar.name, "version": None, "licence": expr,
                        "verdict": verdict, "source": "vendored-grammar"})
    return out


def main() -> int:
    try:
        cargo = scan_cargo()
    except RuntimeError as exc:
        print(json.dumps({
            "gate": "G1.13", "value": 1, "unit": "violations",
            "note": f"dependency graph could not be scanned, so no dependency "
                    f"was measured: {exc}",
            "detail": {"scan_error": str(exc)}}))
        return 0
    grammars = scan_grammars()

    if cargo is None and not grammars:
        return not_implemented(
            "no Cargo workspace and no vendored grammars yet "
            "(Stage G0 forbids product code)")

    deps = (cargo[0] if cargo else []) + grammars
    incompatible = [d for d in deps if d["verdict"] == "incompatible"]
    unknown = [d for d in deps if d["verdict"] == "unknown"]
    violations = incompatible + unknown

    print(json.dumps({
        "gate": "G1.13",
        "value": len(violations),
        "unit": "violations",
        "note": (f"{len(deps)} dependencies scanned; "
                 f"{len(incompatible)} incompatible, {len(unknown)} undetermined"
                 + (" (an undetermined licence counts as a violation)"
                    if unknown else "")),
        "detail": {
            "scanned": len(deps),
            "sources": ([cargo[1]] if cargo else []) + (
                ["vendored grammars"] if grammars else []),
            "allowlist": sorted(ALLOWED),
            "incompatible": incompatible,
            "undetermined": unknown,
        },
        "evidence": ["harness/g1/g1_13_licences.py"],
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
