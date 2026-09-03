#!/usr/bin/env python3
"""
G1.1 — Crash and power-loss safety (HARD).

Target: >= 2,000 randomized SIGKILL injections PLUS >= 1,000 simulated
power-loss cases via an I/O fault-injection shim that drops, reorders and tears
un-fsynced writes. After each: verify passes, zero checkpointed data lost, zero
corruption -- under BOTH harnesses. Critical-section coverage per the §6
preamble.

How the power-loss half works without the product's cooperation
---------------------------------------------------------------
§0.3 forbids product code from detecting harness execution, so the engine must
never know it is being fault-injected. The shim therefore sits BELOW the
product, at the libc boundary:

  1. RECORD. `ltx` runs under a DYLD/LD interposition library that appends every
     write, pwrite, fsync, fdatasync, rename, ftruncate and unlink to a journal
     -- offsets, lengths, payload digests, and which fd they targeted. The
     product is unmodified and unaware.
  2. REPLAY. For each trial the harness reconstructs a plausible post-crash
     filesystem from a random PREFIX of that journal, applying the three
     failures real hardware exhibits:
       - DROP     un-fsynced writes vanish
       - REORDER  un-fsynced writes land out of order within a barrier window
       - TEAR     one write lands partially, at a sector boundary
     Writes before an fsync on the same fd are durable and are never dropped;
     that is precisely the guarantee fsync buys, and a shim that violated it
     would fail the engine for a promise the platform never made.
  3. VERIFY. `ltx verify --complete` must pass, and every checkpoint that was
     durable before the crash must still resolve with identical content.

This is the "dm-flakey-class or block-level replay" the brief names, built at
the syscall layer because macOS has no dm-flakey and because a block-level shim
would not know where fsync barriers fall.

Coverage
--------
§6 requires the fault injector to demonstrate hits in EVERY declared critical
section, with counters emitted alongside the scorecard. A run that injects 3,000
faults which all land in the same code path is not a 3,000-fault run, and the
coverage block fails it.
"""
from __future__ import annotations

import json
import os
import random
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import ltxrun as L  # noqa: E402

GATE = "G1.1"
SEED = 20260903
SIGKILL_TRIALS = 2000
POWERLOSS_TRIALS = 1000
SHIM = REPO / "harness" / "lib" / "iofault" / "libiofault.dylib"
SHIM_SO = REPO / "harness" / "lib" / "iofault" / "libiofault.so"

# §6: the fault injector must demonstrate hits in every declared critical
# section. These names are emitted by the shim's journal as region markers
# derived from the path being written, so the product never declares them.
CRITICAL_SECTIONS = ["store_write", "compaction", "thinning", "merge", "sync"]
MIN_HITS_PER_SECTION = 1

# Operations the trials interleave, so crashes land across the surface rather
# than all inside `save`.
OPERATION_POOL = [
    ["save", "trial checkpoint"],
    ["start", "crash-line"],
    ["switch", "main"],
    ["undo"],
    ["sync", "--dry-run"],
    ["internals", "compact"],
    ["internals", "thin"],
]


def shim_path() -> Path | None:
    for candidate in (SHIM, SHIM_SO):
        if candidate.exists():
            return candidate
    return None


def seeded_bytes(rng: random.Random, n: int) -> bytes:
    """Trial content from the SEEDED generator.

    os.urandom() bypassed `rng` entirely, so the emitted SEED could not
    reproduce a trial's inputs -- and a crash harness that cannot reproduce its
    own failures is not much use when one fires.
    """
    return rng.randbytes(n)


def durable_checkpoints(repo: Path) -> list[dict]:
    """Checkpoints the engine reports as durable, with their content digests."""
    proc = L.run(["log", "--forensic", "--json"], cwd=repo)
    if proc.returncode != 0:
        return []
    try:
        doc = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return []
    return doc.get("checkpoints", [])


def verify(repo: Path) -> tuple[bool, str]:
    proc = L.run(["verify", "--complete", "--json"], cwd=repo, timeout=3600)
    if proc.returncode != 0:
        return False, proc.stderr.strip()[:300]
    try:
        doc = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return False, "verify emitted unparseable JSON"
    return bool(doc.get("complete")) and not doc.get("errors"), \
        json.dumps(doc.get("errors", []))[:300]


def calibrate(repo: Path, argv: list[str], shim: Path, work: Path,
              trial: int) -> int | None:
    """Total journal bytes this operation emits when it runs to completion.

    Measured on a throwaway copy so the real trial starts from the same state,
    and cached per operation so 2,000 trials do not pay for 2,000 calibrations.
    """
    key = " ".join(argv)
    if key in _CALIBRATION:
        return _CALIBRATION[key]
    probe = work / f"cal-{trial}"
    shutil.rmtree(probe, ignore_errors=True)
    shutil.copytree(repo, probe, symlinks=True)
    journal = work / f"cal-journal-{trial}.bin"
    env = dict(os.environ, IOFAULT_JOURNAL=str(journal), IOFAULT_ROOT=str(probe),
               DYLD_INSERT_LIBRARIES=str(shim), LD_PRELOAD=str(shim))
    L.run(argv, cwd=probe, env=env)
    total = journal.stat().st_size if journal.exists() else 0
    shutil.rmtree(probe, ignore_errors=True)
    journal.unlink(missing_ok=True)
    _CALIBRATION[key] = total
    return total


_CALIBRATION: dict[str, int] = {}


def sigkill_trial(work: Path, rng: random.Random, trial: int,
                  shim: Path) -> dict:
    """Kill `ltx` at a seeded point in its I/O SEQUENCE, then verify.

    The injection point is chosen by how much the operation has written, not by
    elapsed wall-clock time. An earlier version slept for a seeded duration,
    which is not reproducible: the delay was seeded but the scheduler and the
    machine's load were not, so the same seed killed different operations at
    different internal states and a failure could not be re-run.

    Running the trial under the recording shim gives a monotonic progress
    signal -- journal bytes written -- that is a property of the operation
    rather than of the machine. The same seed now targets the same point in the
    same I/O sequence.
    """
    repo = work / f"sk-{trial}"
    shutil.rmtree(repo, ignore_errors=True)
    repo.mkdir(parents=True)
    if L.run(["init"], cwd=repo).returncode != 0:
        return {"trial": trial, "ok": False, "why": "init failed"}

    (repo / "seed.txt").write_bytes(seeded_bytes(rng, 4096))
    save = L.run(["save", "seed"], cwd=repo)
    if save.returncode != 0:
        return {"trial": trial, "ok": False, "injected": False,
                "why": f"baseline save failed: {save.stderr[:160]}"}
    before = durable_checkpoints(repo)
    if not before:
        return {"trial": trial, "ok": False, "injected": False,
                "why": "no baseline checkpoints readable; loss assertion "
                       "would be vacuous"}

    (repo / f"edit-{trial}.bin").write_bytes(
        seeded_bytes(rng, rng.randrange(1024, 1 << 20)))
    argv = rng.choice(OPERATION_POOL)

    journal = work / f"sk-journal-{trial}.bin"
    env = dict(os.environ, IOFAULT_JOURNAL=str(journal), IOFAULT_ROOT=str(repo),
               DYLD_INSERT_LIBRARIES=str(shim), LD_PRELOAD=str(shim))

    # An ABSOLUTE seeded byte target, derived from a calibration run of this
    # operation. The previous attempt compared `written` against a fraction of
    # `observed_peak`, which is itself set to `written` immediately above --
    # so the test reduced to `written >= fraction * written` and fired the
    # instant 4096 bytes appeared, identically for every seed. The seed had no
    # effect at all.
    total = calibrate(repo, argv, shim, work, trial)
    if total is None or total < 512:
        return {"trial": trial, "ok": True, "injected": False,
                "why": "operation produced too little I/O to place a milestone"}
    target_bytes = int(rng.uniform(0.05, 0.95) * total)

    proc = subprocess.Popen([str(L.LTX), *argv], cwd=repo, env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    injected = False
    reached = False
    deadline = time.monotonic() + 120
    written = 0
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            break
        try:
            written = journal.stat().st_size
        except OSError:
            written = 0
        if written >= target_bytes:
            reached = True
            proc.send_signal(signal.SIGKILL)
            injected = True
            break
        time.sleep(0.002)
    if proc.poll() is None:
        proc.send_signal(signal.SIGKILL)
        injected = True
    proc.wait(timeout=60)
    if not reached:
        # The run never reached its milestone, so the kill landed somewhere
        # unseeded. Not a valid injection; the caller retries.
        return {"trial": trial, "ok": True, "injected": False,
                "why": f"milestone {target_bytes}B not reached (got {written}B)"}
    observed_peak = written

    ok, why = verify(repo)
    after = durable_checkpoints(repo)
    lost = [c for c in before if c not in after]
    return {"trial": trial, "operation": argv[0], "injected": injected,
            "ok": ok and not lost,
            "why": why if not ok else (f"{len(lost)} checkpoints lost" if lost else ""),
            "checkpoints_lost": len(lost),
            "io_bytes_at_kill": observed_peak,
            "target_bytes": target_bytes,
            "calibrated_total_bytes": total}


def powerloss_trial(work: Path, rng: random.Random, trial: int,
                    shim: Path) -> dict:
    """Record the I/O journal, then rebuild a genuine post-crash state.

    The ordering matters and an earlier version got it wrong: it ran the
    operation to COMPLETION and then replayed selected records on top. That is
    not a crash -- it is a consistent post-operation store with some writes
    re-applied, which the engine would obviously survive. The trial measured
    nothing.

    Correct sequence: snapshot the store BEFORE the operation, run the
    operation under the recording shim, RESTORE the snapshot (undoing the
    operation entirely), then replay the mutilated journal onto that restored
    state. What results is a store that saw part of an operation and then lost
    power, which is the thing the gate is about.
    """
    repo = work / f"pl-{trial}"
    shutil.rmtree(repo, ignore_errors=True)
    repo.mkdir(parents=True)
    journal = work / f"journal-{trial}.bin"
    snapshot = work / f"snap-{trial}"

    env = {
        "DYLD_INSERT_LIBRARIES": str(shim),
        "LD_PRELOAD": str(shim),
        "IOFAULT_JOURNAL": str(journal),
        "IOFAULT_ROOT": str(repo),
    }
    if L.run(["init"], cwd=repo, env=env).returncode != 0:
        return {"trial": trial, "ok": False, "why": "init failed under shim"}
    (repo / "seed.txt").write_bytes(seeded_bytes(rng, 4096))
    save = L.run(["save", "seed"], cwd=repo, env=env)
    if save.returncode != 0:
        return {"trial": trial, "ok": False,
                "why": f"baseline save failed: {save.stderr[:160]}"}

    before = durable_checkpoints(repo)
    if not before:
        # Without baseline evidence the "no checkpoints lost" assertion is
        # vacuous -- an empty before-set can never show a loss.
        return {"trial": trial, "ok": False,
                "why": "no baseline checkpoints readable; loss assertion "
                       "would be vacuous"}

    # Snapshot the pre-operation store so the operation can be undone.
    shutil.rmtree(snapshot, ignore_errors=True)
    shutil.copytree(repo / ".lattice", snapshot, symlinks=True)

    (repo / f"edit-{trial}.bin").write_bytes(
        seeded_bytes(rng, rng.randrange(1024, 1 << 20)))
    argv = rng.choice(OPERATION_POOL)
    op = L.run(argv, cwd=repo, env=env)
    if op.returncode != 0:
        # An operation that failed may have produced no relevant I/O, so replay
        # would restore the pristine baseline and the trial would "survive" a
        # crash that never happened.
        return {"trial": trial, "ok": False, "operation": argv[0],
                "why": f"operation failed under shim ({op.returncode}): "
                       f"{op.stderr[:160]}"}

    # Undo the operation completely, then let the replayer rebuild whatever
    # partial state a crash would have left.
    shutil.rmtree(repo / ".lattice", ignore_errors=True)
    shutil.copytree(snapshot, repo / ".lattice", symlinks=True)

    replay = subprocess.run(
        [sys.executable, str(REPO / "harness" / "lib" / "iofault" / "replay.py"),
         "--journal", str(journal), "--root", str(repo),
         "--seed", str(rng.randrange(1 << 30)),
         "--drop", "--reorder", "--tear"],
        capture_output=True, text=True, timeout=600)
    if replay.returncode != 0:
        return {"trial": trial, "ok": False,
                "why": f"replay failed: {replay.stderr.strip()[:200]}"}
    sections = json.loads(replay.stdout or "{}").get("sections_hit", [])

    ok, why = verify(repo)
    after = durable_checkpoints(repo)
    lost = [c for c in before if c not in after]
    return {"trial": trial, "operation": argv[0], "ok": ok and not lost,
            "why": why if not ok else (f"{len(lost)} checkpoints lost" if lost else ""),
            "checkpoints_lost": len(lost), "sections_hit": sections}


def main() -> int:
    try:
        L.require_ltx()
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))

    shim = shim_path()
    if shim is None:
        # Half of this gate is the power-loss half. Reporting only the SIGKILL
        # result would convert a two-harness HARD gate into a one-harness gate,
        # which is the failure mode §6's "under BOTH harnesses" forbids.
        return L.not_implemented(
            GATE, "I/O fault-injection shim not built "
                  "(harness/lib/iofault/); G1.1 requires BOTH the SIGKILL and "
                  "the power-loss harness, and will not report one alone")

    rng = random.Random(SEED)
    work = Path(tempfile.mkdtemp(prefix="g1-1-"))
    try:
        # Over-run: attempts where the process had already exited delivered no
        # signal, so keep going until the mandated number of REAL injections is
        # reached (bounded, so a pathologically fast engine cannot loop forever).
        sk = []
        attempts = 0
        while (sum(1 for r in sk if r.get("injected")) < SIGKILL_TRIALS
               and attempts < SIGKILL_TRIALS * 3):
            sk.append(sigkill_trial(work, rng, attempts, shim))
            attempts += 1
        pl = [powerloss_trial(work, rng, i, shim) for i in range(POWERLOSS_TRIALS)]

        failures = [r for r in sk + pl if not r["ok"]]
        hits: dict[str, int] = {s: 0 for s in CRITICAL_SECTIONS}
        for r in pl:
            for s in r.get("sections_hit", []):
                if s in hits:
                    hits[s] += 1
        uncovered = [s for s, n in hits.items() if n < MIN_HITS_PER_SECTION]

        injected_sk = [r for r in sk if r.get("injected")]
        coverage_ok = (not uncovered
                       and len(injected_sk) >= SIGKILL_TRIALS
                       and len(pl) >= POWERLOSS_TRIALS)
        coverage_note = ""
        if uncovered:
            coverage_note = ("fault injector never hit critical section(s): "
                             + ", ".join(uncovered))
        elif len(injected_sk) < SIGKILL_TRIALS or len(pl) < POWERLOSS_TRIALS:
            coverage_note = (
                f"{len(injected_sk)}/{SIGKILL_TRIALS} trials actually delivered a "
                f"SIGKILL ({len(sk) - len(injected_sk)} processes had already "
                f"exited) and {len(pl)}/{POWERLOSS_TRIALS} power-loss trials")

        return L.emit({
            "gate": GATE,
            "value": len(failures),
            "unit": "failures",
            "note": (f"{len(failures)} failures over {len(sk)} SIGKILL + "
                     f"{len(pl)} power-loss injections"),
            "coverage": {"ok": coverage_ok, "note": coverage_note},
            "detail": {
                "seed": SEED,
                "sigkill_trials_attempted": len(sk),
                "sigkill_trials_injected": len(injected_sk),
                "powerloss_trials": len(pl),
                "critical_section_hits": hits,
                "checkpoints_lost_total": sum(r.get("checkpoints_lost", 0)
                                              for r in sk + pl),
                "failures": failures[:25],
                "failures_truncated": max(0, len(failures) - 25),
            },
            "evidence": ["harness/lib/iofault/"],
        })
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
