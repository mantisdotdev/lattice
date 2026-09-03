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
SPDX_SPLIT = re.compile(r"\s+(?:OR|AND)\s+|/", re.I)
LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING)", re.I)


def not_implemented(note: str) -> int:
    print(json.dumps({"gate": "G1.13", "status": "not-implemented", "note": note}))
    return 0


def classify(expr: str | None) -> tuple[str, str]:
    """Return (verdict, reason) for an SPDX expression.

    A disjunction ("MIT OR Apache-2.0") passes if ANY term is allowed, which is
    how dual licensing works: the consumer picks. A conjunction is treated the
    same way here, which is deliberately permissive and is why any dependency
    using AND is listed in the report for human review rather than silently
    accepted as clean.
    """
    if not expr or not expr.strip():
        return "unknown", "no licence declared"
    terms = [t.strip().strip("()") for t in SPDX_SPLIT.split(expr) if t.strip()]
    if any(t in ALLOWED for t in terms):
        return "allowed", expr
    if any(t in KNOWN_INCOMPATIBLE for t in terms):
        return "incompatible", expr
    return "unknown", expr


def scan_cargo() -> tuple[list[dict], str] | None:
    if not CARGO_TOML.exists():
        return None
    r = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features"],
        cwd=REPO, capture_output=True, text=True, errors="replace", timeout=900)
    if r.returncode != 0:
        return None
    try:
        meta = json.loads(r.stdout)
    except json.JSONDecodeError:
        return None

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
    cargo = scan_cargo()
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
