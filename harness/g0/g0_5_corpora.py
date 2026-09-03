#!/usr/bin/env python3
"""
G0.5 — Corpora assembly (HARD).

Asserts every row of the frozen statistics contract
(corpus/manifests/g0-5-statistics-contract.md, hash-pinned) against the built
composite reference repo, plus the large-binary corpus bounds. Value is 1 only
when every assertion holds; the harness does not accept "close enough," because
a corpus that misses the contract makes every downstream perf gate measure
something other than what the contract promised.
"""
from __future__ import annotations
import collections
import hashlib
import json
import os
import re
import statistics
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CONTRACT = REPO / "corpus" / "manifests" / "g0-5-statistics-contract.md"
CONTRACT_PIN = REPO / "corpus" / "manifests" / "g0-5-statistics-contract.sha256"
REFREPO = REPO / "corpus" / "data" / "reference-repo"
PINS = REPO / "corpus" / "manifests" / "g0-5-pins.json"
BINMAN = REPO / "corpus" / "manifests" / "g0-5-binary-corpus.json"

# Bounds are PARSED from the pinned contract document at run time (see
# parse_bounds); this dict is only the expected key set and unit interpretation.
# An earlier version transcribed the numbers here and claimed the hash pin
# protected them. It did not: the pin compares the document's SHA-256 and never
# reads this file, so a weakened bound here passed silently while the gate still
# reported 1. Found by review; the fix is to stop transcribing.
EXPECTED_KEYS = {
    "file_count",
    "total_working_bytes",
    "history_bytes",
    "history_depth",
    "binary_fraction",
    "p50_file_bytes",
    "p90_file_bytes",
    "p99_file_bytes",
    "max_file_bytes",
    "directory_count",
    "max_directory_fanout",
    "mean_directory_fanout",
    "max_path_depth",
    "distinct_extensions",
}
BINARY_BOUNDS = {
    "seed_bytes": (500 * 2**20, None),
    "total_bytes_with_history": (5 * 2**30, None),
    "versions_per_seed": (8, None),
    "mutation_kinds_used": (4, None),
}


SIZE_SUFFIX = {"gib": 2 ** 30, "mib": 2 ** 20, "kib": 2 ** 10, "g": 2 ** 30,
               "m": 2 ** 20, "k": 2 ** 10}


def _num(token: str) -> float | None:
    """Parse a contract bound token.

    Handles the forms the contract actually uses: `90,000`, `1.5 GiB`, `0.05`,
    `262,144`, `20,000 commits`, `40.0`. A trailing unit word (commits, files,
    entries) is ignored; a size suffix is applied as a multiplier.
    """
    token = token.strip().replace("`", "").replace(",", "")
    m = re.match(r"^\s*([0-9]*\.?[0-9]+)\s*([A-Za-z]*)", token)
    if not m:
        return None
    value = float(m.group(1))
    unit = m.group(2).lower()
    return value * SIZE_SUFFIX.get(unit, 1)


def parse_bounds(text: str) -> dict[str, tuple[float | None, float | None]]:
    """Derive the bounds from the pinned contract document itself.

    The contract is the authority. Parsing it means a weakened bound cannot be
    smuggled into the harness without changing the document, which moves the
    hash pin, which fails the gate.
    """
    out: dict[str, tuple[float | None, float | None]] = {}
    for line in text.splitlines():
        if not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.strip("|").split("|")]
        if len(cells) < 2:
            continue
        key = cells[0].strip().strip("`")
        if key not in EXPECTED_KEYS:
            continue
        bound = cells[1].strip()
        lo = hi = None
        m = re.match(r"^≥\s*(.+?)\s+and\s+≤\s*(.+)$", bound)
        if m:
            lo, hi = _num(m.group(1)), _num(m.group(2))
        elif bound.startswith("≥"):
            lo = _num(bound[1:])
        elif bound.startswith("≤"):
            hi = _num(bound[1:])
        if lo is not None or hi is not None:
            out[key] = (lo, hi)
    return out


def contract_bases(text: str) -> set[str]:
    """The base repositories the contract names, parsed from its own table."""
    out: set[str] = set()
    for line in text.splitlines():
        if not line.startswith("|"):
            continue
        cells = [c.strip().strip("`") for c in line.strip("|").split("|")]
        if len(cells) >= 2 and re.fullmatch(r"[\w.-]+/[\w.-]+", cells[1]):
            out.add(cells[1])
    return out


def dir_bytes(p: Path) -> int:
    total = 0
    for root, _, files in os.walk(p):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(root, f))
            except OSError:
                pass
    return total


def measure_reference() -> dict | None:
    if not (REFREPO / ".git").exists():
        return None
    sizes, dirs, exts, depths = [], collections.Counter(), collections.Counter(), []
    binary = 0
    files = 0
    unreadable: list[str] = []
    for root, dirnames, filenames in os.walk(REFREPO):
        if ".git" in root.split(os.sep):
            continue
        rel = os.path.relpath(root, REFREPO)
        dirs[rel] = len(filenames) + len(dirnames)
        for f in filenames:
            path = os.path.join(root, f)
            try:
                st = os.lstat(path)
            except OSError:
                continue
            if not os.path.isfile(path) or os.path.islink(path):
                continue
            sizes.append(st.st_size)
            files += 1
            exts[Path(f).suffix.lower()] += 1
            depths.append(len(Path(os.path.relpath(path, REFREPO)).parts))
            try:
                with open(path, "rb") as fh:
                    if b"\x00" in fh.read(8000):
                        binary += 1
            except OSError:
                # An unreadable file is NOT "not binary". Counting it in `files`
                # while omitting it from `binary` lowers binary_fraction and can
                # bring a non-conforming corpus under the 0.35 ceiling, so the
                # gate must fail rather than absorb it.
                unreadable.append(os.path.relpath(path, REFREPO))
    if not sizes:
        return None
    sizes.sort()

    def q(p):
        return sizes[min(len(sizes) - 1, int(len(sizes) * p))]

    depth = subprocess.run(["git", "-C", str(REFREPO), "rev-list", "--all", "--count"],
                           capture_output=True, text=True).stdout.strip()
    return {
        "file_count": files,
        "total_working_bytes": sum(sizes),
        "history_bytes": dir_bytes(REFREPO / ".git"),
        "history_depth": int(depth or 0),
        "binary_fraction": binary / files,
        "p50_file_bytes": q(0.50),
        "p90_file_bytes": q(0.90),
        "p99_file_bytes": q(0.99),
        "max_file_bytes": sizes[-1],
        "directory_count": len(dirs),
        "max_directory_fanout": max(dirs.values()) if dirs else 0,
        "mean_directory_fanout": statistics.mean(dirs.values()) if dirs else 0.0,
        "max_path_depth": max(depths) if depths else 0,
        "distinct_extensions": len(exts),
        "unreadable_files": unreadable,
    }


def check(measured: dict, bounds: dict) -> list[str]:
    out = []
    for key, (lo, hi) in bounds.items():
        v = measured.get(key)
        if v is None:
            out.append(f"{key}: not measured")
            continue
        if lo is not None and v < lo:
            out.append(f"{key}: {v:,.4g} < required {lo:,.4g}")
        if hi is not None and v > hi:
            out.append(f"{key}: {v:,.4g} > allowed {hi:,.4g}")
    return out


def main() -> int:
    problems, detail = [], {}

    if not CONTRACT.exists() or not CONTRACT_PIN.exists():
        problems.append("statistics contract or its hash pin missing")
    else:
        actual = hashlib.sha256(CONTRACT.read_bytes()).hexdigest()
        if actual != CONTRACT_PIN.read_text().split()[0]:
            problems.append("CONTRACT HASH MISMATCH — the contract changed after freezing")
        detail["contract_sha256"] = actual

    bounds: dict = {}
    if CONTRACT.exists():
        bounds = parse_bounds(CONTRACT.read_text())
        missing_bounds = sorted(EXPECTED_KEYS - set(bounds))
        if missing_bounds:
            problems.append(
                "contract does not define bounds for: " + ", ".join(missing_bounds)
                + " — the harness refuses to substitute its own")
        detail["bounds_parsed_from_contract"] = {
            k: list(v) for k, v in sorted(bounds.items())}

    ref = measure_reference()
    if ref is None:
        problems.append("reference repo not built "
                        "(scripts/corpus/build_reference_repo.py)")
    elif not bounds:
        problems.append("no bounds could be parsed from the pinned contract")
    else:
        detail["reference_repo"] = ref
        problems += [f"reference repo {p}" for p in check(ref, bounds)]
        if ref.get("unreadable_files"):
            problems.append(
                f"{len(ref['unreadable_files'])} files could not be read while "
                f"classifying binary content; binary_fraction is therefore "
                f"unmeasured for them")
        if PINS.exists():
            detail["base_pins"] = json.loads(PINS.read_text()).get("bases", {})
            # Compare against the bases the CONTRACT names, not a bare count.
            # `len(...) < 3` let a composite omit a named base, still satisfy
            # every numeric bound, and report 1 -- the contract names five.
            named = contract_bases(CONTRACT.read_text()) if CONTRACT.exists() else set()
            detail["contract_bases"] = sorted(named)
            missing_bases = sorted(named - set(detail["base_pins"]))
            if not named:
                problems.append("could not parse the base set from the contract")
            elif missing_bases:
                problems.append(
                    "composite omits contract-named base(s): "
                    + ", ".join(missing_bases))
        else:
            problems.append("base repository pins not recorded")

    if not BINMAN.exists():
        problems.append("binary corpus not built "
                        "(scripts/corpus/simulate_binary_history.py)")
    else:
        b = json.loads(BINMAN.read_text())
        bm = {
            "seed_bytes": b.get("seed_bytes", 0),
            "total_bytes_with_history": b.get("total_bytes_with_history", 0),
            "versions_per_seed": b.get("versions_per_seed", 0),
            "mutation_kinds_used": sum(1 for v in b.get("mutation_kinds", {}).values() if v > 0),
        }
        detail["binary_corpus"] = bm
        problems += [f"binary corpus {p}" for p in check(bm, BINARY_BOUNDS)]

    print(json.dumps({
        "gate": "G0.5",
        "value": 0 if problems else 1,
        "unit": "contract",
        "note": ("; ".join(problems[:6]) + (f" (+{len(problems)-6} more)"
                                            if len(problems) > 6 else "")
                 if problems else "reference repo and binary corpus satisfy the frozen contract"),
        "detail": {**detail, "problems": problems},
        "evidence": [str(CONTRACT.relative_to(REPO))],
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
