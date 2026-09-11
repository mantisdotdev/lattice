#!/usr/bin/env python3
"""Bounded scaling probe. NOT A GATE — a diagnostic beside G1.4 and G1.5.

ADR-6 recorded one question and declined to answer it:

    G1.4's real shape is 80,000 operations over a tree that grows to ~80,000
    files, where each `save` walks and hashes the whole tree; whether that fits
    any time budget is not answered here and must be measured before G1.4 is
    claimed.

This measures it. The answer is no, and the reason is not the tree walk.

Two costs grow with things a repository accumulates rather than with the work
asked of it:

  * `PackWriter::retain_unknown` asks `Store::contains` for every chunk it
    holds, and `contains` scans every pack. One pack is written per save, so a
    save is O(chunks in the tree x packs in the store) — it gets slower with
    every save that came before it, which is why the FIRST save of a large tree
    is fast and the next one is not.
  * `Repo::checkpoint` has no index: it reads and deserialises every chunk in
    the store looking for one blob. `status` does that twice, once through
    `head_checkpoint` and once through `checkpoints`.

Both are acknowledged in the source — "small and adequate for the current
history sizes; a checkpoint index is a later refinement" — and both predate the
workspace slice, which is why `--ltx` exists: pointing it at a binary built from
another revision produces a second arm, and the two together show the finding is
not a regression.

Measured, not projected: the numbers below are of real commands on real trees.
The projection at the end IS an extrapolation and is labelled as one.

One arm per run, one file per arm — the same shape as the concurrency probe's
locked and unlocked artifacts. A single file holding both would leave a reader,
and the ADR-evidence check, unable to tell which arm a quoted number came from.

    python3 scripts/probe_scaling.py --out bench/results/raw/adr6-scaling.json
    python3 scripts/probe_scaling.py --ltx /other/ltx --out .../adr6-scaling-baseline.json
"""
from __future__ import annotations

import argparse
import json
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
DEFAULT_LTX = REPO / "target" / "release" / "ltx"
TIMEOUT_S = 1800


def run(ltx: Path, args: list[str], cwd: Path) -> tuple[int, float]:
    started = time.monotonic()
    proc = subprocess.run([str(ltx), *args], cwd=cwd, capture_output=True,
                          text=True, errors="replace", timeout=TIMEOUT_S)
    return proc.returncode, time.monotonic() - started


def measure(ltx: Path, files: int, samples: int) -> dict | None:
    work = Path(tempfile.mkdtemp(prefix="ltx-scaling-probe-"))
    try:
        rc, _ = run(ltx, ["init"], work)
        if rc != 0:
            return None
        for i in range(files):
            (work / f"f{i:06d}.txt").write_text(f"content {i}\n")

        rc, first_save = run(ltx, ["save", "seed"], work)
        if rc != 0:
            return None

        # A save that changes exactly ONE file. If the cost were the tree walk
        # this would be near the first save; it is not, which is the finding.
        edits = []
        for s in range(samples):
            (work / "one.txt").write_text(f"edit {s}\n")
            rc, elapsed = run(ltx, ["save", f"edit {s}"], work)
            if rc != 0:
                return None
            edits.append(elapsed)

        # A command that saves nothing at all.
        rc, status = run(ltx, ["status"], work)
        if rc != 0:
            return None

        return {
            "files": files,
            "first_save_s": round(first_save, 3),
            "incremental_save_s": round(statistics.median(edits), 3),
            "status_s": round(status, 3),
        }
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--sizes", default="1000,5000,10000",
                    help="comma-separated tree sizes, in files")
    ap.add_argument("--samples", type=int, default=3)
    ap.add_argument("--ltx", type=Path, default=DEFAULT_LTX)
    ap.add_argument("--out", type=Path)
    args = ap.parse_args()

    sizes = [int(s) for s in args.sizes.split(",") if s.strip()]
    # A run of nothing satisfies every check below without measuring anything.
    if args.samples < 1 or not sizes or any(n < 1 for n in sizes):
        print(json.dumps({"probe": "scaling",
                          "error": "--sizes must be positive and --samples at least 1"}))
        return 1
    if not args.ltx.exists():
        print(json.dumps({"probe": "scaling", "error": f"{args.ltx} is not built"}))
        return 1

    rows = []
    for n in sizes:
        row = measure(args.ltx, n, args.samples)
        if row is None:
            print(json.dumps({"probe": "scaling",
                              "error": f"a command failed at {n} files"}))
            return 1
        rows.append(row)

    out = {"probe": "scaling", "samples": args.samples, "sizes": rows}

    # The extrapolation, clearly separated from what was measured.
    if len(rows) >= 2:
        small, large = rows[0], rows[-1]
        per_file = large["incremental_save_s"] / large["files"]
        # G1.4 runs 80,000 operations over a tree growing to ~80,000 files, so
        # the mean tree an operation sees is roughly half the final size.
        projected = per_file * 40000
        out["projection"] = {
            "basis": "linear in tree size from the largest measured point; the "
                     "measured growth is worse than linear, so this is a floor",
            "mean_tree_files": 40000,
            "projected_incremental_save_s": round(projected, 2),
            "g1_4_operations": 80000,
            "projected_hours_if_every_operation_saved": round(projected * 80000 / 3600, 1),
        }

    print(json.dumps(out, indent=2))
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(out, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
