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

# The contract, transcribed. Changing a bound here without changing the
# contract document is caught by the hash pin above.
BOUNDS = {
    "file_count": (90_000, 130_000),
    "total_working_bytes": (1.5 * 2**30, None),
    "history_bytes": (2.0 * 2**30, None),
    "history_depth": (20_000, None),
    "binary_fraction": (0.05, 0.35),
    "p50_file_bytes": (512, 8192),
    "p90_file_bytes": (8192, None),
    "p99_file_bytes": (262_144, None),
    "max_file_bytes": (33_554_432, None),
    "directory_count": (6_000, None),
    "max_directory_fanout": (500, None),
    "mean_directory_fanout": (3.0, 40.0),
    "max_path_depth": (8, None),
    "distinct_extensions": (30, None),
}
BINARY_BOUNDS = {
    "seed_bytes": (500 * 2**20, None),
    "total_bytes_with_history": (5 * 2**30, None),
    "versions_per_seed": (8, None),
    "mutation_kinds_used": (4, None),
}


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
                pass
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

    ref = measure_reference()
    if ref is None:
        problems.append("reference repo not built "
                        "(scripts/corpus/build_reference_repo.py)")
    else:
        detail["reference_repo"] = ref
        problems += [f"reference repo {p}" for p in check(ref, BOUNDS)]
        if PINS.exists():
            detail["base_pins"] = json.loads(PINS.read_text()).get("bases", {})
            if len(detail["base_pins"]) < 3:
                problems.append("fewer than 3 pinned base repositories")
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
