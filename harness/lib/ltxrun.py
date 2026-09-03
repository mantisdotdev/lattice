#!/usr/bin/env python3
"""
Shared plumbing for gate harnesses that drive the `ltx` binary.

Everything here is deliberately dumb: locate the binary, run it, time it,
compute percentiles. No gate logic lives in this file, because a shared helper
that decides pass/fail is a single place to accidentally weaken every gate at
once.

Two things this module enforces on its callers, both from
docs/DISAGREEMENTS.md:

  Challenge 5 -- every timing gate reports BOTH `p95_daemon_resident` and
  `p95_daemonless`. `measure_both()` makes producing only one awkward: it
  returns a dict with both keys or raises.

  §0.3 -- the harness never asks the product to behave differently. There is no
  "test mode" flag here, and `harness/lib/check_no_harness_leak.py` fails CI if
  product code ever learns to recognise one.
"""
from __future__ import annotations

import json
import math
import os
import shutil
import statistics
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LTX = REPO / "target" / "release" / "ltx"
LTX_DAEMON = REPO / "target" / "release" / "ltx-daemon"
REFERENCE_REPO = REPO / "corpus" / "data" / "reference-repo"

# §0.3: "All timing targets are p95 over >= 100 warm runs."
MIN_WARM_RUNS = 100
DEFAULT_TIMEOUT = 900


class NotBuilt(Exception):
    """The subject of measurement does not exist yet."""


def require_ltx() -> Path:
    if not LTX.exists():
        raise NotBuilt("ltx binary not built (target/release/ltx)")
    return LTX


def require_reference_repo() -> Path:
    if not (REFERENCE_REPO / ".git").exists():
        raise NotBuilt("reference repo not built "
                       "(scripts/corpus/build_reference_repo.py)")
    return REFERENCE_REPO


def not_implemented(gate: str, note: str) -> int:
    print(json.dumps({"gate": gate, "status": "not-implemented", "note": note}))
    return 0


def emit(payload: dict) -> int:
    print(json.dumps(payload))
    return 0


def run(args: list[str], cwd: Path, timeout: int = DEFAULT_TIMEOUT,
        env: dict | None = None) -> subprocess.CompletedProcess:
    full_env = dict(os.environ)
    if env:
        full_env.update(env)
    return subprocess.run([str(LTX), *args], cwd=cwd, capture_output=True,
                          text=True, errors="replace", timeout=timeout,
                          env=full_env)


def percentile(values: list[float], p: float) -> float:
    """Nearest-rank percentile: the smallest value at or below which at least
    p of the samples fall, i.e. the ceil(p*n)-th ordered sample (1-indexed).

    An earlier version used `int(n*p + 0.5) - 1`, which rounds rather than
    ceilings: for n=111 at p95 it selected the 105th sample where nearest-rank
    requires the 106th. That UNDER-reports latency, so a gate could pass on a
    percentile it had not actually met.
    """
    if not values:
        return float("nan")
    ordered = sorted(values)
    rank = math.ceil(p * len(ordered))
    return ordered[max(0, min(len(ordered) - 1, rank - 1))]


@dataclass
class DaemonHandle:
    proc: subprocess.Popen | None = None

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.proc.kill()


def start_daemon(cwd: Path, warmup_s: float = 2.0) -> DaemonHandle:
    """Start the daemon and let its watcher settle.

    Raises NotBuilt if the daemon binary is absent, so a timing gate cannot
    silently report its daemonless number as if the daemon had been running.
    """
    if not LTX_DAEMON.exists():
        raise NotBuilt("ltx-daemon binary not built")
    proc = subprocess.Popen([str(LTX_DAEMON), "--repo", str(cwd)],
                            cwd=cwd, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    deadline = time.monotonic() + warmup_s
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise NotBuilt(f"ltx-daemon exited immediately (code {proc.returncode})")
        time.sleep(0.05)
    return DaemonHandle(proc)


def stop_any_daemon(cwd: Path) -> None:
    if LTX.exists():
        subprocess.run([str(LTX), "daemon", "stop"], cwd=cwd,
                       capture_output=True, timeout=60)


@dataclass
class TimingResult:
    samples_ms: list[float] = field(default_factory=list)
    failures: int = 0
    stderr_sample: str = ""

    @property
    def p50(self) -> float:
        return percentile(self.samples_ms, 0.50)

    @property
    def p95(self) -> float:
        return percentile(self.samples_ms, 0.95)

    @property
    def p99(self) -> float:
        return percentile(self.samples_ms, 0.99)

    @property
    def mean(self) -> float:
        return statistics.mean(self.samples_ms) if self.samples_ms else float("nan")


def time_command(argv: list[str], cwd: Path, runs: int,
                 warmup: int = 5, before=None,
                 timeout: int = DEFAULT_TIMEOUT) -> TimingResult:
    """Time `ltx <argv>` over `runs` warm invocations.

    `before` runs before each timed invocation and is NOT timed -- use it to
    set up per-iteration state (apply an edit, switch a line) without charging
    the setup to the measurement.
    """
    result = TimingResult()
    for i in range(warmup + runs):
        if before is not None:
            before(i)
        started = time.perf_counter()
        proc = run(argv, cwd=cwd, timeout=timeout)
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        if proc.returncode != 0:
            result.failures += 1
            if not result.stderr_sample:
                result.stderr_sample = proc.stderr.strip()[:300]
            continue
        if i >= warmup:
            result.samples_ms.append(elapsed_ms)
    return result


def measure_both(argv: list[str], cwd: Path, runs: int = MIN_WARM_RUNS,
                 warmup: int = 5, before=None) -> dict:
    """Measure a command with the daemon resident AND without it.

    Challenge 5: every latency gate must publish both numbers. The gate's
    pass/fail remains governed by the spec's stated target applied to the
    daemon-resident figure -- re-scoping a gate upward on our own authority is
    as forbidden as downward -- but the daemonless number appears in every
    scorecard, so a catastrophic degradation is a finding the stakeholder sees
    rather than one the scorecard hides.
    """
    out: dict = {}

    stop_any_daemon(cwd)
    daemonless = time_command(argv, cwd, runs=runs, warmup=warmup, before=before)
    out["daemonless"] = _summarise(daemonless)

    handle = None
    try:
        handle = start_daemon(cwd)
        resident = time_command(argv, cwd, runs=runs, warmup=warmup, before=before)
        out["daemon_resident"] = _summarise(resident)
    except NotBuilt as exc:
        out["daemon_resident"] = None
        out["daemon_unavailable"] = str(exc)
    finally:
        if handle:
            handle.stop()

    if out.get("daemon_resident") is None:
        raise NotBuilt(out.get("daemon_unavailable", "daemon unavailable"))

    d = out["daemon_resident"]["p95_ms"]
    n = out["daemonless"]["p95_ms"]
    out["daemonless_slowdown"] = round(n / d, 3) if d and d > 0 else None
    # ADR-4's product constraint: >5x slower daemonless must be self-reported by
    # the command. The harness surfaces the ratio so G2.4 can enforce the rest.
    out["exceeds_adr4_5x_threshold"] = bool(
        out["daemonless_slowdown"] and out["daemonless_slowdown"] > 5.0)
    return out


def _summarise(t: TimingResult) -> dict:
    return {
        "runs": len(t.samples_ms),
        "failures": t.failures,
        "p50_ms": round(t.p50, 3),
        "p95_ms": round(t.p95, 3),
        "p99_ms": round(t.p99, 3),
        "mean_ms": round(t.mean, 3),
        "stderr_sample": t.stderr_sample,
    }


def clone_reference(dest: Path) -> Path:
    """A disposable copy of the reference repo, so a gate never mutates the
    corpus every other gate measures against."""
    require_reference_repo()
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(REFERENCE_REPO, dest, symlinks=True)
    return dest


def load_edit_sets() -> list[dict]:
    """Replayable edit sets built from real consecutive commits.

    §6: "'Edit sets' for latency gates are replayed sequences of real
    consecutive commits from the pinned base repos -- never synthetic edits you
    design." A harness that cannot find them must fail rather than invent any.
    """
    path = REPO / "corpus" / "data" / "edit-sets.jsonl"
    if not path.exists():
        raise NotBuilt("edit sets not built "
                       "(scripts/corpus/build_edit_sets.py)")
    return [json.loads(line) for line in path.read_text().splitlines()
            if line.strip()]
