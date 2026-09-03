#!/usr/bin/env python3
"""
G1.4 — Concurrency safety (HARD).

Target: 8 concurrent workspaces x 10,000 randomized operations: 0 corruption,
0 deadlocks, op-log linearizable. Coverage per the §6 preamble.

Linearizability is checked, not assumed. Each worker records the real-time
interval [start, end] of every operation plus the op-log position the engine
reported. A history is linearizable iff some total order of the operations is
consistent with real time -- meaning if A finished before B started, A must
precede B in the op-log. That is checkable directly from the intervals without
solving the general (NP-hard) problem, because the op-log GIVES us the candidate
order: we only need to confirm no pair contradicts it.

Deadlock detection is a watchdog rather than an analysis: any operation that
exceeds the timeout with the process alive is recorded as a deadlock, because
from the user's point of view that is exactly what it is.
"""
from __future__ import annotations
import concurrent.futures as cf
import json
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import ltxrun as L  # noqa: E402

GATE = "G1.4"
SEED = 20260903
WORKSPACES = 8
OPS_PER_WORKSPACE = 10_000     # the gate says 8 workspaces x 10,000 ops
OPS_TOTAL = WORKSPACES * OPS_PER_WORKSPACE
OP_TIMEOUT_S = 120
MIN_EMISSIONS_PER_OP = 100     # §6 coverage contract

OPERATIONS = [
    ["save", "concurrent edit"], ["start", "line"], ["switch", "main"],
    ["undo"], ["assign", "."], ["split"], ["lens", "use", "clean"],
    ["internals", "thin"], ["internals", "compact"], ["sync", "--dry-run"],
]


def worker(repo: Path, ws: int, ops: int, seed: int) -> dict:
    rng = random.Random(seed)
    events, failures, deadlocks = [], [], []
    counts: dict[str, int] = {}
    for i in range(ops):
        argv = rng.choice(OPERATIONS)
        name = " ".join(argv[:2])
        counts[name] = counts.get(name, 0) + 1
        (repo / f"ws{ws}-{i}.txt").write_text(f"{ws}:{i}\n")
        start = time.monotonic()
        try:
            proc = L.run([*argv, "--json"], cwd=repo, timeout=OP_TIMEOUT_S)
        except subprocess.TimeoutExpired:
            deadlocks.append({"workspace": ws, "op": name, "index": i})
            continue
        end = time.monotonic()
        if proc.returncode != 0:
            failures.append({"workspace": ws, "op": name,
                             "stderr": proc.stderr.strip()[:160]})
            continue
        try:
            doc = json.loads(proc.stdout)
        except json.JSONDecodeError:
            failures.append({"workspace": ws, "op": name,
                             "stderr": "unparseable --json output"})
            continue
        events.append({"workspace": ws, "op": name, "start": start, "end": end,
                       "oplog_seq": doc.get("oplog_seq")})
    return {"events": events, "failures": failures, "deadlocks": deadlocks,
            "counts": counts}


def linearizability_violations(events: list[dict]) -> tuple[list[dict], list[dict]]:
    """If A finished before B started, A must precede B in the op-log.

    Returns (violations, unsequenced). A successful operation with no
    `oplog_seq` is NOT silently excluded -- excluding it would let an engine
    pass by simply omitting the field, so it is reported as its own failure.
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
                # rather than break. `break` stopped at the first overlap and
                # skipped every subsequent comparison.
                continue
            if b["oplog_seq"] <= a["oplog_seq"]:
                violations.append({
                    "earlier": {k: a[k] for k in ("workspace", "op", "oplog_seq")},
                    "later": {k: b[k] for k in ("workspace", "op", "oplog_seq")},
                    "why": "op that finished later has an earlier op-log position"})
                if len(violations) >= 50:
                    return violations, unsequenced
    return violations, unsequenced


def main() -> int:
    try:
        L.require_ltx()
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))

    work = Path(tempfile.mkdtemp(prefix="g1-4-"))
    try:
        base = work / "repo"
        base.mkdir()
        if L.run(["init"], cwd=base).returncode != 0:
            return L.not_implemented(GATE, "ltx init failed")
        (base / "seed.txt").write_text("seed\n")
        L.run(["save", "seed"], cwd=base)

        spaces = []
        for ws in range(WORKSPACES):
            path = work / f"ws{ws}"
            if L.run(["workspace", "new", str(path)], cwd=base).returncode != 0:
                return L.not_implemented(GATE, f"workspace {ws} could not be created")
            spaces.append(path)

        # Dividing 10,000 across the workspaces gave 1,250 each, which is
        # sampling that reduces a mandated iteration count.
        per = OPS_PER_WORKSPACE
        with cf.ThreadPoolExecutor(max_workers=WORKSPACES) as ex:
            results = list(ex.map(
                lambda t: worker(t[1], t[0], per, SEED + t[0]), enumerate(spaces)))

        events = [e for r in results for e in r["events"]]
        failures = [f for r in results for f in r["failures"]]
        deadlocks = [d for r in results for d in r["deadlocks"]]
        counts: dict[str, int] = {}
        for r in results:
            for k, v in r["counts"].items():
                counts[k] = counts.get(k, 0) + v

        violations, unsequenced = linearizability_violations(events)
        verify = L.run(["verify", "--complete", "--json"], cwd=base, timeout=7200)
        corrupt = verify.returncode != 0

        under = sorted(k for k, v in counts.items() if v < MIN_EMISSIONS_PER_OP)
        never = sorted({" ".join(o[:2]) for o in OPERATIONS} - set(counts))
        coverage_problems = []
        if never:
            coverage_problems.append(f"operations never emitted: {', '.join(never)}")
        if under:
            coverage_problems.append(
                f"emitted < {MIN_EMISSIONS_PER_OP} times: {', '.join(under[:6])}")
        if sum(counts.values()) < OPS_TOTAL:
            coverage_problems.append(
                f"{sum(counts.values())} of {OPS_TOTAL} operations executed")

        total = (len(failures) + len(deadlocks) + len(violations)
                 + len(unsequenced) + (1 if corrupt else 0))
        return L.emit({
            "gate": GATE, "value": total, "unit": "failures",
            "note": (f"{len(failures)} op failures, {len(deadlocks)} deadlocks, "
                     f"{len(violations)} linearizability violations, "
                     f"{len(unsequenced)} successful ops with no op-log position, "
                     f"{'corruption detected' if corrupt else 'verify clean'}"),
            "coverage": {"ok": not coverage_problems,
                         "note": "; ".join(coverage_problems)},
            "detail": {
                "workspaces": WORKSPACES, "ops_total": sum(counts.values()),
                "seed": SEED, "operation_counts": counts,
                "deadlocks": deadlocks[:20],
                "linearizability_violations": violations[:20],
                "unsequenced_operations": unsequenced[:20],
                "failures": failures[:20],
                "verify_stderr": verify.stderr.strip()[:300] if corrupt else "",
            },
        })
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
