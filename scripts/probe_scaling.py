#!/usr/bin/env python3
"""Bounded scaling probe. NOT A GATE — a diagnostic beside G1.4 and G1.5.

ADR-6 recorded one question and declined to answer it:

    G1.4's real shape is 80,000 operations over a tree that grows to ~80,000
    files, where each `save` walks and hashes the whole tree; whether that fits
    any time budget is not answered here and must be measured before G1.4 is
    claimed.

This measures it. The first run answered no, and located why: **a checkpoint's
identity was not its storage address.** `Checkpoint::body_id` hashed
`(tree, message, parent, at_unix_ms)` while the blob was stored under the hash
of the whole serialised struct, so nothing mapped one to the other and finding a
checkpoint meant reading and deserialising every chunk in the store. `status`,
which saves nothing, was slower than a save, because it did that twice.

ADR-8 made the body itself the blob, at the body's own address. `status` at
10,000 files went from seconds to tens of milliseconds and stopped growing with
the tree at all; an incremental save kept a cost that does grow, which is the
tree walk doing work the save actually needs.

`--attribute` is what showed this rather than asserted it: it times each read
command separately on one tree, so what is INDEXED and what is SCANNED separate
by two orders of magnitude in one artifact. The numbers are not repeated here —
`bench/results/raw/adr6-attribution.json` holds the pre-ADR-8 run, and a figure
copied into a comment is a figure that drifts from its measurement.

Weigh the two differently. `ltx status` reports the head checkpoint and some
counts and does NOT compare the working tree against the tip, so its whole cost
was the scan and a flat line afterwards is the expected shape rather than a
surprising one. The incremental save is the number that reflects work: a save
walks the tree, and it is still the command this probe exists to worry about.

`PackWriter::retain_unknown` asking `Store::contains` per chunk remains, and is
NOT a scan of the same kind: `contains` binary-searches each pack's index and
reads no payload. It grows with the pack count rather than the blob count, so it
is nothing at the few saves measured here and material only once a history has
many packs.

Measured, not projected: the numbers below are of real commands on real trees.
The projection at the end IS an extrapolation and is labelled as one.

One arm per run, one file per arm — the same shape as the concurrency probe's
locked and unlocked artifacts. A single file holding both would leave a reader,
and the ADR-evidence check, unable to tell which arm a quoted number came from.
`--ltx` points the probe at a binary built from another revision, which is how
the before-and-after arms of ADR-8 were produced on one machine with one probe.
Comparing against an artifact from an EARLIER version of this file would not
have been sound: `status_s` became a median here, having been one observation.

    python3 scripts/probe_scaling.py --out bench/results/raw/adr9-scaling.json
    python3 scripts/probe_scaling.py --ltx /other/ltx --out .../adr8-scaling-before.json
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

        # A command that saves nothing at all. Sampled like the saves are: it
        # is the sharpest number this probe reports, and one observation of a
        # sharp number is an anecdote.
        statuses = []
        for _ in range(samples):
            rc, elapsed = run(ltx, ["status"], work)
            if rc != 0:
                return None
            statuses.append(elapsed)

        # A switch between two lines that differ by ONE file. If a switch
        # costs the size of the tree, this grows with `files` like the first
        # save does; if it costs the size of the change, it does not.
        rc, _ = run(ltx, ["start", "other"], work)
        if rc != 0:
            return None
        (work / "only-on-other.txt").write_text("other\n")
        rc, _ = run(ltx, ["save", "one more file"], work)
        if rc != 0:
            return None
        switches = []
        for target in ["main", "other"] * samples:
            rc, elapsed = run(ltx, ["switch", target], work)
            if rc != 0:
                return None
            switches.append(elapsed)

        return {
            "files": files,
            # One observation by nature — a repository has exactly one first
            # save, so a median would need a repository per sample.
            "first_save_s": round(first_save, 3),
            "incremental_save_s": round(statistics.median(edits), 3),
            "status_s": round(statistics.median(statuses), 3),
            "switch_s": round(statistics.median(switches), 3),
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
