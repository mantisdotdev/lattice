#!/usr/bin/env python3
"""Bounded concurrency probe. NOT A GATE — a diagnostic beside G1.4.

Nothing here measures anything of record. G1.4 owns that, is frozen, and must
not be replaced by this. What this exists for is the case where G1.4 cannot be
RUN: it needs `ltx workspace new`, which does not exist, so it returns
not-implemented before executing a single operation and can say nothing about
whether concurrent commands are safe.

This probe runs concurrent commands against one repository from N processes and
answers the one question that blocked the workspace design: does a second
process fail outright, and if it is made to queue instead, is the resulting
op-log a total order?

Four deliberate differences from G1.4, all of which make this WEAKER, and none
of which may be read back into the gate:

  * processes share one directory rather than N workspaces, because the
    workspace noun is what this probe exists to unblock;
  * only the implemented operations are drawn — split, lens, thin, compact and
    sync fail on "unrecognized subcommand" and say nothing about concurrency;
  * the operation count is a small multiple of the workers, not 10,000 each;
  * no deadlock watchdog beyond the subprocess timeout.

Linearizability is checked with G1.4's algorithm, line for line, and for the
same reason: if A finished before B started, A must precede B in the op-log.
That is checkable directly from the intervals because the op-log gives the
candidate order. Copied rather than reimplemented — a diagnostic that claims to
mirror a frozen gate and then differs from it is worse than one that makes no
claim, and the first version of this file sorted by op-log position where the
gate sorts by end time.

    python3 scripts/probe_concurrency.py --workers 8 --ops 25
"""
from __future__ import annotations

import argparse
import json
import random
import shutil
import subprocess
import sys
import tempfile
import time
from concurrent import futures
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
LTX = REPO / "target" / "release" / "ltx"

# The implemented half of G1.4's pool. `assign .` is drawn because it is the
# operation this concurrency question was raised by.
OPERATIONS = [
    ["save", "concurrent edit"],
    ["start", "line"],
    ["switch", "main"],
    ["undo"],
    ["assign", "."],
]
OP_TIMEOUT_S = 120


def worker(root: Path, index: int, ops: int, seed: int) -> dict:
    rng = random.Random(seed)
    events, failures = [], []
    counts: dict[str, int] = {}
    for i in range(ops):
        argv = rng.choice(OPERATIONS)
        name = " ".join(argv[:2])
        counts[name] = counts.get(name, 0) + 1
        (root / f"w{index}-{i}.txt").write_text(f"{index}:{i}\n")
        start = time.monotonic()
        try:
            proc = subprocess.run(
                [str(LTX), *argv, "--json"], cwd=root, capture_output=True,
                text=True, errors="replace", timeout=OP_TIMEOUT_S)
        except subprocess.TimeoutExpired:
            failures.append({"worker": index, "op": name, "why": "timeout"})
            continue
        end = time.monotonic()
        if proc.returncode != 0:
            # The error document goes to stdout under --json; stderr is empty.
            failures.append({"worker": index, "op": name,
                             "why": proc.stdout.strip()[:160]})
            continue
        try:
            doc = json.loads(proc.stdout)
        except json.JSONDecodeError:
            failures.append({"worker": index, "op": name,
                             "why": "unparseable --json output"})
            continue
        events.append({"worker": index, "op": name, "start": start, "end": end,
                       "oplog_seq": doc.get("oplog_seq")})
    return {"events": events, "failures": failures, "counts": counts}


def linearizability_violations(events: list[dict]) -> tuple[list[dict], list[dict]]:
    """If A finished before B started, A must precede B in the op-log.

    A successful operation carrying no `oplog_seq` is reported as its own
    failure rather than skipped, so omitting the field cannot be a way to pass.
    """
    unsequenced = [e for e in events if e.get("oplog_seq") is None]
    ordered = sorted((e for e in events if e.get("oplog_seq") is not None),
                     key=lambda e: e["end"])
    violations = []
    for i, a in enumerate(ordered):
        for b in ordered[i + 1:]:
            if b["start"] < a["end"]:
                # Overlapping: any order is permitted for THIS pair, but a
                # later non-overlapping pair may still violate, so continue
                # rather than break.
                continue
            if b["oplog_seq"] <= a["oplog_seq"]:
                violations.append({
                    "earlier": {k: a[k] for k in ("worker", "op", "oplog_seq")},
                    "later": {k: b[k] for k in ("worker", "op", "oplog_seq")},
                    "why": "op that finished later has an earlier op-log position",
                })
                if len(violations) >= 50:
                    return violations, unsequenced
    return violations, unsequenced


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--ops", type=int, default=25)
    ap.add_argument("--seed", type=int, default=20260905)
    ap.add_argument("--out", type=Path)
    args = ap.parse_args()

    if not LTX.exists():
        print(json.dumps({"error": "target/release/ltx is not built"}))
        return 1

    work = Path(tempfile.mkdtemp(prefix="ltx-concurrency-probe-"))
    try:
        subprocess.run([str(LTX), "init"], cwd=work, capture_output=True, timeout=60)
        (work / "seed.txt").write_text("seed\n")
        subprocess.run([str(LTX), "save", "seed"], cwd=work,
                       capture_output=True, timeout=60)

        started = time.monotonic()
        with futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
            results = list(pool.map(
                lambda i: worker(work, i, args.ops, args.seed + i),
                range(args.workers)))
        elapsed = time.monotonic() - started

        events = [e for r in results for e in r["events"]]
        failures = [f for r in results for f in r["failures"]]
        counts: dict[str, int] = {}
        for r in results:
            for k, v in r["counts"].items():
                counts[k] = counts.get(k, 0) + v

        violations, unsequenced = linearizability_violations(events)

        # The repository must still be intact and its chain unbroken.
        verify = subprocess.run([str(LTX), "verify", "--complete", "--json"],
                                cwd=work, capture_output=True, text=True,
                                errors="replace", timeout=600)
        try:
            report = json.loads(verify.stdout)
        except json.JSONDecodeError:
            report = {"errors": ["verify emitted unparseable JSON"]}

        out = {
            "probe": "concurrency",
            "workers": args.workers,
            "ops_per_worker": args.ops,
            "ops_attempted": args.workers * args.ops,
            "ops_succeeded": len(events),
            "failures": len(failures),
            "failure_sample": failures[:5],
            "linearizability_violations": len(violations),
            "violation_sample": violations[:3],
            "unsequenced_operations": len(unsequenced),
            "verify_errors": len(report.get("errors", [])),
            "verify_complete": bool(report.get("complete")),
            "counts": counts,
            "wall_clock_s": round(elapsed, 3),
            "ops_per_s": round(len(events) / elapsed, 1) if elapsed else None,
            "seed": args.seed,
        }
        print(json.dumps(out, indent=2))
        if args.out:
            args.out.parent.mkdir(parents=True, exist_ok=True)
            args.out.write_text(json.dumps(out, indent=2) + "\n")
        return 0 if not failures and not violations else 1
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
