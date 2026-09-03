#!/usr/bin/env python3
"""
Hardware calibration (§0.3), frozen before the G1 harness freeze.

Three microbenchmarks, chosen because the gated operations are bounded by
exactly these three resources:

  cpu    single-core integer/branch mix   -> hashing, chunk boundary scanning
  mem    sequential memory bandwidth      -> chunk buffers, index scans
  fsync  4 KiB durable write latency      -> the op-log append on every command

The normalization factor is the geometric mean of the three ratios against the
values recorded on the reference machine, capped at 2× (§0.3). A machine
requiring more than 2× is ineligible to produce gate measurements.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import statistics
import tempfile
import time
from pathlib import Path

REF_FILE = Path(__file__).resolve().parents[2] / "bench" / "calibration-reference.json"
CAP = 2.0


def bench_cpu(rounds: int = 3) -> float:
    """Deterministic integer/branch mix. Returns seconds (best of rounds)."""
    def once() -> float:
        t0 = time.perf_counter()
        x = 1
        for i in range(1, 400_001):
            x = (x * 1103515245 + 12345) & 0xFFFFFFFF
            if x & 1:
                x ^= i
        t = time.perf_counter() - t0
        return t if x else t  # keep x live
    return min(once() for _ in range(rounds))


def bench_mem(rounds: int = 3, size: int = 64 << 20) -> float:
    """Sequential read bandwidth over a 64 MiB buffer. Seconds (best)."""
    buf = bytearray(size)
    mv = memoryview(buf)

    def once() -> float:
        t0 = time.perf_counter()
        total = 0
        step = 1 << 20
        for off in range(0, size, step):
            total += len(bytes(mv[off:off + step]))
        return time.perf_counter() - t0 if total == size else float("inf")
    return min(once() for _ in range(rounds))


def bench_fsync(samples: int = 60) -> float:
    """Median 4 KiB durable-write latency in seconds. Median, not min:
    the op-log's cost is the typical fsync, not the luckiest one."""
    page = os.urandom(4096)
    times: list[float] = []
    with tempfile.TemporaryDirectory() as td:
        path = Path(td) / "calib.bin"
        fd = os.open(path, os.O_CREAT | os.O_WRONLY, 0o600)
        try:
            for _ in range(samples):
                t0 = time.perf_counter()
                os.write(fd, page)
                os.fsync(fd)
                times.append(time.perf_counter() - t0)
        finally:
            os.close(fd)
    return statistics.median(times)


def measure() -> dict[str, float]:
    return {"cpu_s": bench_cpu(), "mem_s": bench_mem(), "fsync_s": bench_fsync()}


def factor(current: dict[str, float], reference: dict[str, float]) -> tuple[float, bool]:
    ratios = [current[k] / reference[k] for k in ("cpu_s", "mem_s", "fsync_s")]
    geo = math.exp(sum(math.log(r) for r in ratios) / len(ratios))
    normalized = max(1.0, geo)          # never speed a target up
    return min(normalized, CAP), normalized <= CAP


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--record", action="store_true",
                    help="write this machine's values as the reference")
    args = ap.parse_args()

    current = measure()
    if args.record:
        REF_FILE.parent.mkdir(parents=True, exist_ok=True)
        REF_FILE.write_text(json.dumps(
            {"recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
             **current}, indent=2) + "\n")
        print(json.dumps({"recorded": current}, indent=2))
        return 0

    if not REF_FILE.exists():
        print(json.dumps({"error": "no calibration reference recorded"}))
        return 1
    ref = json.loads(REF_FILE.read_text())
    f, eligible = factor(current, ref)
    print(json.dumps({"current": current, "reference":
                      {k: ref[k] for k in ("cpu_s", "mem_s", "fsync_s")},
                      "factor": round(f, 4), "capped_at": CAP,
                      "eligible": eligible}, indent=2))
    return 0 if eligible else 2


if __name__ == "__main__":
    raise SystemExit(main())
