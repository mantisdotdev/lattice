#!/usr/bin/env python3
"""Bounded power-loss probe. NOT A GATE — a diagnostic beside G1.1.

Nothing here measures anything of record. G1.1 owns that, is frozen, and must
not be replaced by this. What this exists for is the case where G1.1 cannot be
RUN: it retains a repository and a ~1.7 MB journal for every one of ~4,500
trials, so it needs roughly 22-24 GB of scratch space and dies with ENOSPC below
that. This probe runs the same power-loss sequence for a bounded number of
trials and deletes each trial's working set immediately, so disk stays flat and
a crash-safety question can be answered with a number rather than an argument.

Two deliberate differences from G1.1, both of which make this WEAKER, and
neither of which may be read back into the gate:

  * only the four implemented operations are drawn, because sync, internals
    compact and internals thin fail on "unrecognized subcommand" and say
    nothing about crash safety;
  * the SIGKILL half is not run at all.

It was written to answer one question: whether teaching the injector about
fcntl(F_FULLFSYNC) (ADR-18) removed the failures. Over 60 trials on one seed it
answered 37 failures before, 0 after; two further seeds of 80 trials each also
returned 0.


NOT a gate and not a substitute for one. G1.1 is frozen and retains a repository
and a journal for every one of ~4,500 trials, which needs ~22-24 GB of scratch;
this machine has 17 GB. This probe runs the SAME sequence for a bounded number
of trials and deletes each trial's working set immediately, so disk stays flat
and the question "did the fcntl fix remove the real failures?" can be answered
with a number instead of an argument.

It deliberately omits the three operations that do not exist yet (sync,
internals compact, internals thin), because those fail on "unrecognized
subcommand" and say nothing about crash safety.

Usage: probe_powerloss.py <shim.dylib> <trials> [seed]
"""
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# Derived, never hardcoded: a recorded diagnostic that only runs from one
# person's checkout is not a recorded diagnostic. LTX_BIN overrides the binary.
REPO = Path(__file__).resolve().parents[1]
LTX = Path(os.environ.get("LTX_BIN", REPO / "target" / "release" / "ltx"))
REPLAY = REPO / "harness" / "lib" / "iofault" / "replay.py"

# G1.1's operation pool, verbatim. Three of these do not exist yet; they are
# INCLUDED so the failure-reason distribution can be measured rather than
# assumed, because G1.1 itself retains only `failures[:25]` and cannot show
# what the other 402 were.
FULL_POOL = [
    ["save", "trial checkpoint"],
    ["start", "crash-line"],
    ["switch", "main"],
    ["undo"],
    ["sync", "--dry-run"],
    ["internals", "compact"],
    ["internals", "thin"],
]

# The implemented subset, for isolating crash-safety behaviour from
# "this command does not exist".
IMPLEMENTED_ONLY = FULL_POOL[:4]


def run(argv, cwd, env=None, timeout=120):
    e = dict(os.environ)
    if env:
        e.update(env)
    return subprocess.run([str(LTX), *argv], cwd=cwd, env=e,
                          capture_output=True, text=True, timeout=timeout)


def checkpoints(repo):
    p = run(["log", "--forensic", "--json"], cwd=repo)
    if p.returncode != 0:
        return None
    try:
        return json.loads(p.stdout).get("checkpoints", [])
    except json.JSONDecodeError:
        return None


def verify(repo):
    p = run(["verify", "--complete", "--json"], cwd=repo)
    if p.returncode != 0:
        return False, p.stderr.strip()[:200]
    try:
        d = json.loads(p.stdout)
    except json.JSONDecodeError:
        return False, "unparseable"
    return bool(d.get("complete")) and not d.get("errors"), \
        json.dumps(d.get("errors", []))[:200]


def trial(work, rng, i, shim, pool):
    repo = work / f"pl-{i}"
    snap = work / f"snap-{i}"
    journal = work / f"j-{i}.bin"
    repo.mkdir(parents=True)
    env = {"DYLD_INSERT_LIBRARIES": str(shim), "LD_PRELOAD": str(shim),
           "IOFAULT_JOURNAL": str(journal), "IOFAULT_ROOT": str(repo)}
    try:
        if run(["init"], cwd=repo, env=env).returncode != 0:
            return {"skip": "init failed"}
        (repo / "seed.txt").write_bytes(rng.randbytes(4096))
        if run(["save", "seed"], cwd=repo, env=env).returncode != 0:
            return {"skip": "seed save failed"}
        before = checkpoints(repo)
        if not before:
            return {"skip": "no baseline"}

        shutil.copytree(repo / ".lattice", snap, symlinks=True)
        journal.unlink(missing_ok=True)
        (repo / f"edit-{i}.bin").write_bytes(rng.randbytes(rng.randrange(1024, 1 << 19)))
        argv = rng.choice(pool)
        op = run(argv, cwd=repo, env=env)
        if op.returncode != 0:
            # G1.1 counts this as a FAILURE, not a skip, so mirror it — the
            # whole point is to reproduce its accounting.
            why = op.stderr.strip()[:200]
            kind = ("unimplemented command"
                    if "unrecognized subcommand" in why else "operation failed")
            return {"op": argv[0], "ok": False, "why": why, "kind": kind,
                    "lost": 0}

        shutil.rmtree(repo / ".lattice", ignore_errors=True)
        shutil.copytree(snap, repo / ".lattice", symlinks=True)

        rp = subprocess.run(
            [sys.executable, str(REPLAY), "--journal", str(journal),
             "--root", str(repo), "--seed", str(rng.randrange(1 << 30)),
             "--drop", "--reorder", "--tear"],
            capture_output=True, text=True, timeout=300)
        if rp.returncode != 0:
            return {"skip": f"replay failed: {rp.stderr[:80]}"}
        info = json.loads(rp.stdout or "{}")

        ok, why = verify(repo)
        after = checkpoints(repo)
        lost = len(before) if after is None else len([c for c in before if c not in after])
        return {"op": argv[0], "ok": bool(ok) and lost == 0, "why": why,
                "kind": "clean" if (bool(ok) and lost == 0) else
                        ("checkpoint lost" if lost else "verify failed"),
                "lost": lost, "durable": info.get("durable"),
                "volatile": info.get("volatile")}
    finally:
        # The whole point: reclaim immediately so disk stays flat.
        shutil.rmtree(repo, ignore_errors=True)
        shutil.rmtree(snap, ignore_errors=True)
        journal.unlink(missing_ok=True)


def main():
    shim = Path(sys.argv[1]).resolve()
    if not shim.is_file():
        raise SystemExit(f"shim not found: {shim}")
    if not LTX.is_file():
        raise SystemExit(f"ltx not built: {LTX} (cargo build --release)")
    # Without the replayer EVERY trial becomes a skip, and the summary would
    # read "0 failures" over zero completed trials — silence indistinguishable
    # from success, which is the one result a crash-safety probe must never
    # produce.
    if not REPLAY.is_file():
        raise SystemExit(f"replayer missing: {REPLAY}")
    trials = int(sys.argv[2])
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 20260904
    pool = FULL_POOL if os.environ.get("PROBE_FULL_POOL") else IMPLEMENTED_ONLY
    rng = random.Random(seed)
    work = Path(tempfile.mkdtemp(prefix="plprobe-"))
    results, skipped = [], []
    try:
        for i in range(trials):
            r = trial(work, rng, i, shim, pool)
            (skipped if "skip" in r else results).append(r)
    finally:
        shutil.rmtree(work, ignore_errors=True)

    if not results:
        raise SystemExit(
            f"no trial completed ({len(skipped)} skipped): "
            + "; ".join(sorted({r["skip"][:80] for r in skipped})))

    from collections import Counter
    failures = [r for r in results if not r["ok"]]
    durable_zero = sum(1 for r in results if r.get("durable") == 0)
    print(json.dumps({
        "shim": str(shim),
        "pool": "full (G1.1 verbatim)" if pool is FULL_POOL else "implemented only",
        "trials_run": len(results),
        "skipped": len(skipped),
        "skip_reasons": Counter(r["skip"][:60] for r in skipped).most_common(5),
        "failures": len(failures),
        # EVERY failure classified, not a sample — this is the field G1.1
        # cannot provide, since it stores failures[:25].
        "failure_kinds": dict(Counter(r["kind"] for r in failures)),
        "failures_by_operation": dict(Counter(r["op"] for r in failures)),
        "checkpoints_lost_total": sum(r["lost"] for r in results),
        "trials_where_replayer_saw_no_barrier": durable_zero,
        "failure_sample": failures[:3],
    }, indent=1))


if __name__ == "__main__":
    main()
