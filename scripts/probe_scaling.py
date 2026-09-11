#!/usr/bin/env python3
"""Bounded scaling probe. NOT A GATE — a diagnostic beside G1.4 and G1.5.

ADR-6 recorded one question and declined to answer it:

    G1.4's real shape is 80,000 operations over a tree that grows to ~80,000
    files, where each `save` walks and hashes the whole tree; whether that fits
    any time budget is not answered here and must be measured before G1.4 is
    claimed.

This measures it. The answer is no, and the reason is not the tree walk.

One cost dominates, and it is not the tree walk. **A checkpoint's identity is
not its storage address.** `Checkpoint::body_id` hashes `(tree, message, parent,
at_unix_ms)`, while the blob is stored under the hash of the whole serialised
struct — so nothing maps one to the other, and finding a checkpoint means
reading and deserialising every chunk in the store until one matches. The source
says so itself: "a checkpoint is content-addressed like everything else, but its
own address is over its body rather than its serialised form, so the lookup is by
scanning the addresses we know ... a checkpoint index is a later refinement."

`--attribute` is what shows this rather than asserts it: it times each read
command separately on one tree, so what is INDEXED and what is SCANNED separate
by two orders of magnitude in one artifact. The numbers are not repeated here —
`bench/results/raw/adr6-attribution.json` holds them, and a figure copied into a
comment is a figure that drifts from its measurement.

`save` pays the same scan, through `head_checkpoint`. `PackWriter::retain_unknown`
asking `Store::contains` per chunk is a second cost but NOT a scan of the same
kind: `contains` binary-searches each pack's index and reads no payload. It
grows with the pack count rather than the blob count, so it is nothing at the
few saves measured here and material only once a history has many packs.

Both predate the workspace slice, which is why `--ltx` exists: pointing it at a
binary built from another revision produces a second arm, and the two together
show the finding is not a regression.

Measured, not projected: the numbers below are of real commands on real trees.
The projection at the end IS an extrapolation and is labelled as one.

One arm per run, one file per arm — the same shape as the concurrency probe's
locked and unlocked artifacts. A single file holding both would leave a reader,
and the ADR-evidence check, unable to tell which arm a quoted number came from.

    python3 scripts/probe_scaling.py --out bench/results/raw/adr6-scaling.json
    python3 scripts/probe_scaling.py --ltx /other/ltx --out .../adr6-scaling-baseline.json
    python3 scripts/probe_scaling.py --attribute --sizes 10000 \
        --out bench/results/raw/adr6-attribution.json
"""
from __future__ import annotations

import argparse
import json
import os
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


class ProbeError(Exception):
    """Anything that makes a run meaningless, carried to the one place that
    prints the structured error other failure paths already print.

    Without this the file had two ways to fail: a JSON document for the
    conditions it thought of, and a traceback for a bad `--sizes`, an `--ltx`
    that is a directory, or a command that hangs. A caller parsing the output
    cannot tell the second kind from a crash in the probe itself.
    """


def run(ltx: Path, args: list[str], cwd: Path) -> tuple[int, float]:
    started = time.monotonic()
    try:
        proc = subprocess.run([str(ltx), *args], cwd=cwd, capture_output=True,
                              text=True, errors="replace", timeout=TIMEOUT_S,
                              check=False)
    except subprocess.TimeoutExpired:
        raise ProbeError(f"`ltx {' '.join(args)}` did not finish within "
                         f"{TIMEOUT_S}s") from None
    except OSError as exc:
        raise ProbeError(f"could not run {ltx}: {exc}") from None
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


def emit(out: dict, to: Path | None) -> int:
    print(json.dumps(out, indent=2))
    if to:
        to.parent.mkdir(parents=True, exist_ok=True)
        to.write_text(json.dumps(out, indent=2) + "\n")
    return 0


def fail(why: str) -> int:
    """One shape for every failure, so a caller never has to parse a traceback."""
    print(json.dumps({"probe": "scaling", "error": why}))
    return 1


# The commands the attribution mode times, and the key each is recorded under.
# Chosen to separate what is INDEXED from what is SCANNED: the first two answer
# from redb, the last two go through `checkpoints()`.
ATTRIBUTED = [
    ("internals_oplog_s", ["internals", "oplog"]),
    ("line_list_s", ["line", "list"]),
    ("log_forensic_s", ["log", "--forensic"]),
    ("status_s", ["status"]),
]


def attribute(ltx: Path, files: int, samples: int) -> dict | None:
    """Time each read command separately on one tree.

    The scaling rows say a save and a `status` cost seconds; they cannot say
    WHERE those seconds go. This does, by timing commands that differ in
    exactly that respect — and the artifact it writes is what lets a reader
    check the attribution rather than take it.
    """
    work = Path(tempfile.mkdtemp(prefix="ltx-attribution-probe-"))
    try:
        rc, _ = run(ltx, ["init"], work)
        if rc != 0:
            return None
        for i in range(files):
            (work / f"f{i:06d}.txt").write_text(f"content {i}\n")
        rc, _ = run(ltx, ["save", "seed"], work)
        if rc != 0:
            return None
        # A second checkpoint, so the history these commands walk has more than
        # one node and `log --forensic` has something to do.
        (work / "one.txt").write_text("edit\n")
        rc, _ = run(ltx, ["save", "second"], work)
        if rc != 0:
            return None

        out = {"probe": "attribution", "files": files, "samples": samples}
        for key, argv in ATTRIBUTED:
            timings = []
            for _ in range(samples):
                rc, elapsed = run(ltx, argv, work)
                if rc != 0:
                    return None
                timings.append(elapsed)
            out[key] = round(statistics.median(timings), 3)
        return out
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--attribute", action="store_true",
                    help="time each read command separately on one tree, "
                         "instead of measuring save and status across sizes")
    ap.add_argument("--sizes", default="1000,5000,10000",
                    help="comma-separated tree sizes, in files")
    ap.add_argument("--samples", type=int, default=3)
    ap.add_argument("--ltx", type=Path, default=DEFAULT_LTX)
    ap.add_argument("--out", type=Path)
    args = ap.parse_args()

    try:
        sizes = [int(s) for s in args.sizes.split(",") if s.strip()]
    except ValueError as exc:
        return fail(f"--sizes must be whole numbers: {exc}")
    # A run of nothing satisfies every check below without measuring anything.
    if args.samples < 1 or not sizes or any(n < 1 for n in sizes):
        return fail("--sizes must be positive and --samples at least 1")
    # `exists` is not enough: a directory or a file without the execute bit
    # gets past it and fails later, inside a measurement, as an OSError.
    if not args.ltx.is_file() or not os.access(args.ltx, os.X_OK):
        return fail(f"{args.ltx} is not a runnable binary")

    if args.attribute:
        try:
            out = attribute(args.ltx, sizes[-1], args.samples)
        except ProbeError as exc:
            return fail(str(exc))
        if out is None:
            return fail(f"a command failed at {sizes[-1]} files")
        return emit(out, args.out)

    rows = []
    try:
        for n in sizes:
            row = measure(args.ltx, n, args.samples)
            if row is None:
                return fail(f"a command failed at {n} files")
            rows.append(row)
    except ProbeError as exc:
        return fail(str(exc))

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

    return emit(out, args.out)


if __name__ == "__main__":
    sys.exit(main())
