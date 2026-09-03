#!/usr/bin/env python3
"""
The Gauntlet runner.

This module is the only thing permitted to declare a gate PASS. It does so
exclusively by executing that gate's harness and reading the number the harness
printed. There is no code path here that lets a status be asserted by hand.

Enforced invariants, mapped to the protocol:

  §0.1  A gate without a runnable harness does not exist.
        -> a gate whose harness file is absent reports N/A-yet, never PASS.
  §0.3  Harnesses are frozen before implementation of their stage.
        -> FREEZE.json pins the sha256 of every harness file; a changed file
           reverts every gate it touches to FAIL(stale) until re-measured.
  §0.4  The ratchet.
        -> ratchet.json records each gate's best passing (or waived) value;
           a regression beyond tolerance is reported as a blocking violation.
  §0.5  Waivers apply only to SOFT gates, only with the recorded evidence,
        and never more than two performance waivers per delivery.
  §6    Coverage contracts: a 0-failure run that misses coverage is a FAIL.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
GATES_FILE = REPO / "harness" / "gates.toml"
FREEZE_FILE = REPO / "harness" / "FREEZE.json"
WAIVERS_FILE = REPO / "harness" / "waivers.toml"
RATCHET_FILE = REPO / "bench" / "ratchet.json"
RESULTS_DIR = REPO / "bench" / "results"
GAUNTLET_MD = REPO / "GAUNTLET.md"

# §0.4 regression tolerances, as a fraction of the ratchet baseline.
TIMING_TOLERANCE = 0.05
NON_TIMING_TOLERANCE = 0.05
WAIVED_TIMING_TOLERANCE = 0.05
WAIVED_NON_TIMING_TOLERANCE = 0.02

# §0.5 caps.
MAX_PERF_WAIVERS = 2
MAX_WAIVER_RATIO_VS_TARGET = 2.0

HARNESS_TIMEOUT_S = int(os.environ.get("GAUNTLET_HARNESS_TIMEOUT", "3600"))

PASS = "PASS"
FAIL = "FAIL"
STALE = "FAIL(stale)"
WAIVED = "WAIVED"
PENDING_HUMAN = "PASS-PENDING-HUMAN"
NOT_YET = "N/A-yet"
ERROR = "FAIL(harness-error)"

# Statuses that let a stage be CLEAR (§0.1).
CLEARING = {PASS, WAIVED, PENDING_HUMAN}


def _sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


@dataclass
class Gate:
    gid: str
    stage: str
    title: str
    metric: str
    target: float
    cmp: str
    unit: str
    type: str          # HARD | SOFT
    klass: str         # A | S | A+S
    harness: list[str]
    perf: bool = False
    timing: bool = False

    @property
    def harness_path(self) -> Path:
        # argv[0] is the interpreter; argv[1] is the script.
        for token in self.harness:
            if token.endswith(".py") or token.endswith(".sh"):
                return REPO / token
        return REPO / self.harness[-1]

    @property
    def needs_human(self) -> bool:
        return "S" in self.klass.split("+")

    @property
    def lower_is_better(self) -> bool:
        return self.cmp in ("<", "<=")

    def satisfied_by(self, value: float) -> bool:
        ops = {
            ">=": lambda a, b: a >= b,
            "<=": lambda a, b: a <= b,
            ">": lambda a, b: a > b,
            "<": lambda a, b: a < b,
            "==": lambda a, b: a == b,
        }
        return ops[self.cmp](value, self.target)


@dataclass
class Result:
    gate: Gate
    status: str
    value: float | None = None
    detail: dict = field(default_factory=dict)
    evidence: list[str] = field(default_factory=list)
    note: str = ""
    duration_s: float = 0.0

    @property
    def clears(self) -> bool:
        return self.status in CLEARING


def load_gates() -> dict[str, Gate]:
    raw = tomllib.loads(GATES_FILE.read_text())
    gates: dict[str, Gate] = {}
    for key, body in raw.items():
        gid = key.replace("-", ".", 1)
        gates[gid] = Gate(
            gid=gid,
            stage=body["stage"],
            title=body["title"],
            metric=body["metric"],
            target=float(body["target"]),
            cmp=body["cmp"],
            unit=body["unit"],
            type=body["type"],
            klass=body["klass"],
            harness=list(body["harness"]),
            perf=bool(body.get("perf", False)),
            timing=bool(body.get("timing", False)),
        )
    return gates


def load_json(path: Path, default):
    if not path.exists():
        return default
    try:
        return json.loads(path.read_text())
    except json.JSONDecodeError:
        return default


def load_waivers() -> dict[str, dict]:
    if not WAIVERS_FILE.exists():
        return {}
    raw = tomllib.loads(WAIVERS_FILE.read_text())
    return {k.replace("-", ".", 1): v for k, v in raw.items()}


def freeze_status(gate: Gate, freeze: dict) -> str | None:
    """Return None if the harness matches its freeze record, else a reason."""
    record = freeze.get(gate.gid)
    if record is None:
        return None  # not yet frozen; freezing happens at stage entry
    path = gate.harness_path
    if not path.exists():
        return "frozen harness file is missing"
    actual = _sha256_file(path)
    if actual != record["sha256"]:
        return f"harness changed since freeze ({record['sha256'][:12]} -> {actual[:12]})"
    return None


def run_harness(gate: Gate) -> Result:
    path = gate.harness_path
    if not path.exists():
        return Result(gate, NOT_YET, note="harness not implemented yet")

    argv = list(gate.harness)
    started = time.monotonic()
    try:
        proc = subprocess.run(
            argv, cwd=REPO, capture_output=True, text=True,
            timeout=HARNESS_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired:
        return Result(gate, ERROR, note=f"harness exceeded {HARNESS_TIMEOUT_S}s")
    elapsed = time.monotonic() - started

    stdout = proc.stdout.strip()
    if not stdout:
        return Result(gate, ERROR,
                      note=f"harness printed nothing (exit {proc.returncode}); "
                           f"stderr: {proc.stderr.strip()[:300]}",
                      duration_s=elapsed)

    # The harness contract: last line of stdout is one JSON object.
    payload = None
    for line in reversed(stdout.splitlines()):
        line = line.strip()
        if line.startswith("{"):
            try:
                payload = json.loads(line)
                break
            except json.JSONDecodeError:
                continue
    if payload is None:
        return Result(gate, ERROR,
                      note=f"harness emitted no JSON object (exit {proc.returncode})",
                      duration_s=elapsed)

    if payload.get("status") == "not-implemented":
        return Result(gate, NOT_YET,
                      note=payload.get("note", "harness reports subject not built yet"),
                      detail=payload.get("detail", {}), duration_s=elapsed)

    if "value" not in payload:
        return Result(gate, ERROR, note="harness JSON lacks a 'value' field",
                      duration_s=elapsed)

    value = float(payload["value"])
    detail = payload.get("detail", {})
    evidence = payload.get("evidence", [])

    # §6 coverage contract: a clean run that misses coverage is a FAIL.
    coverage = payload.get("coverage")
    if coverage is not None and not coverage.get("ok", False):
        return Result(gate, FAIL, value=value, detail=detail, evidence=evidence,
                      note="coverage contract not satisfied: "
                           + str(coverage.get("note", "")),
                      duration_s=elapsed)

    if proc.returncode != 0 and not payload.get("allow_nonzero_exit"):
        return Result(gate, ERROR, value=value, detail=detail, evidence=evidence,
                      note=f"harness exited {proc.returncode}", duration_s=elapsed)

    ok = gate.satisfied_by(value)
    status = PASS if ok else FAIL
    note = payload.get("note", "")

    if ok and gate.needs_human:
        # [S] and [A]+[S] gates: the autonomous floor passed. The gate holds
        # PASS-PENDING-HUMAN until real human results land (§0.7).
        if not payload.get("human_results_present"):
            status = PENDING_HUMAN
            kit = payload.get("human_kit")
            note = (note + " | " if note else "") + (
                f"human kit: {kit}" if kit else "human kit not yet delivered")
            if not kit:
                status = FAIL
                note = "[S] gate floor passed but no ready-to-run kit committed (§0.7)"

    return Result(gate, status, value=value, detail=detail, evidence=evidence,
                  note=note, duration_s=elapsed)


def apply_freeze_and_waivers(result: Result, freeze: dict,
                             waivers: dict) -> Result:
    gate = result.gate

    stale_reason = freeze_status(gate, freeze)
    if stale_reason and result.status in (PASS, PENDING_HUMAN, WAIVED):
        # §0.3: the moment a harness changes, gates it touches lose PASS.
        return Result(gate, STALE, value=result.value, detail=result.detail,
                      evidence=result.evidence, note=stale_reason,
                      duration_s=result.duration_s)

    if result.status == FAIL and gate.gid in waivers:
        waiver = waivers[gate.gid]
        problems = validate_waiver(gate, waiver, result.value)
        if problems:
            return Result(gate, FAIL, value=result.value, detail=result.detail,
                          evidence=result.evidence,
                          note="INVALID WAIVER: " + "; ".join(problems),
                          duration_s=result.duration_s)
        return Result(gate, WAIVED, value=result.value, detail=result.detail,
                      evidence=result.evidence,
                      note=f"waived: {waiver.get('summary','')}",
                      duration_s=result.duration_s)

    return result


def validate_waiver(gate: Gate, waiver: dict, measured: float | None) -> list[str]:
    """§0.5 legitimacy checks that can be made mechanically.

    Includes the anti-stall rule from docs/DISAGREEMENTS.md Challenge 14: §0.5's
    improvement formula is max(0, ...) < 2%, which a REGRESSING gate satisfies —
    a negative improvement clamps to 0, and 0 < 2%. So a gate moving steadily
    away from its target qualified for a waiver on the same footing as one that
    had genuinely converged. Regression and convergence must not be scored
    identically at the moment the protocol decides whether to stop trying.
    """
    problems: list[str] = []
    if gate.type == "HARD":
        problems.append("HARD gates can never be waived (§0.5)")
    for req in ("iterations", "strategies", "analysis", "achieved"):
        if req not in waiver:
            problems.append(f"missing required field '{req}'")
    if waiver.get("iterations", 0) < 5:
        problems.append(f"only {waiver.get('iterations', 0)} targeted iterations, ≥5 required")
    strategies = waiver.get("strategies", [])
    if len(strategies) < 3:
        problems.append(f"only {len(strategies)} strategies, ≥3 materially distinct required")
    for s in strategies:
        # `is None`, not falsiness: iteration 0 is a legitimate value and
        # `not 0` would reject a correctly pre-registered strategy.
        if s.get("pre_registered_iteration") is None:
            problems.append(
                f"strategy '{s.get('name','?')}' was not pre-registered in a DIAGNOSE step")
    # Anti-stall: a regressing gate has not plateaued, it has broken.
    v_prev = waiver.get("value_3_iterations_ago")
    if v_prev is None:
        problems.append("missing 'value_3_iterations_ago' — the §0.5 improvement "
                        "formula cannot be checked without it")
    elif measured is not None:
        prev_gap = abs(v_prev - gate.target)
        cur_gap = abs(measured - gate.target)
        if cur_gap > prev_gap * 1.02:      # 2% tolerance for measurement noise
            problems.append(
                f"gate is REGRESSING: gap to target grew from {prev_gap:g} to "
                f"{cur_gap:g}. A regressing gate has not plateaued (Challenge 14)")

    # Each targeted iteration must have changed something.
    for i, it in enumerate(waiver.get("iteration_log", [])):
        if not it.get("commit"):
            problems.append(
                f"targeted iteration {i + 1} links no commit — re-measuring an "
                f"unchanged system is not a remediation iteration")
    if waiver.get("iterations", 0) >= 5 and \
            len(waiver.get("iteration_log", [])) < waiver.get("iterations", 0):
        problems.append(
            f"claims {waiver.get('iterations')} targeted iterations but logs "
            f"{len(waiver.get('iteration_log', []))}")

    if gate.perf and measured is not None and gate.target:
        ratio = (measured / gate.target) if gate.lower_is_better else (gate.target / measured)
        if ratio > MAX_WAIVER_RATIO_VS_TARGET:
            problems.append(
                f"achieved value is {ratio:.2f}× target, exceeding the 2× waiver cap (§0.5)")
    return problems


def ratchet_check(results: list[Result]) -> tuple[list[str], dict]:
    """§0.4. Returns (violations, updated baselines)."""
    baselines = load_json(RATCHET_FILE, {})
    violations: list[str] = []

    for r in results:
        gate = r.gate
        if r.value is None:
            continue
        prior = baselines.get(gate.gid)

        if prior is not None:
            tol = (WAIVED_TIMING_TOLERANCE if gate.timing else WAIVED_NON_TIMING_TOLERANCE) \
                if prior.get("waived") else \
                (TIMING_TOLERANCE if gate.timing else NON_TIMING_TOLERANCE)
            base = prior["value"]
            if gate.lower_is_better:
                limit = base * (1 + tol) if base > 0 else base + 1e-9
                worse = r.value > limit
            else:
                limit = base * (1 - tol)
                worse = r.value < limit
            if worse:
                violations.append(
                    f"{gate.gid}: {r.value:g} {gate.unit} is worse than the ratchet "
                    f"baseline {base:g} beyond the {tol:.0%} tolerance "
                    f"({'waived-value' if prior.get('waived') else 'passing-value'} ratchet, §0.4)")
            if prior.get("status") in CLEARING and r.status == FAIL:
                violations.append(
                    f"{gate.gid}: previously {prior['status']}, now FAIL — "
                    f"this blocks all other work until restored (§0.4)")

        # Record/improve the baseline only for statuses that count.
        if r.status in CLEARING:
            better = (prior is None
                      or (gate.lower_is_better and r.value <= prior["value"])
                      or (not gate.lower_is_better and r.value >= prior["value"]))
            if better:
                baselines[gate.gid] = {
                    "value": r.value, "status": r.status,
                    "waived": r.status == WAIVED, "unit": gate.unit,
                    "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                }
            elif prior is not None:
                prior["status"] = r.status

    return violations, baselines


def waiver_cap_check(results: list[Result]) -> list[str]:
    perf_waivers = [r.gate.gid for r in results
                    if r.status == WAIVED and r.gate.perf]
    if len(perf_waivers) > MAX_PERF_WAIVERS:
        return [f"§0.5 cap exceeded: {len(perf_waivers)} performance waivers "
                f"({', '.join(perf_waivers)}), maximum is {MAX_PERF_WAIVERS}"]
    return []


def stage_state(results: list[Result], stage: str) -> str:
    rs = [r for r in results if r.gate.stage == stage]
    if not rs:
        return "EMPTY"
    if all(r.clears for r in rs):
        return "CLEAR"
    if any(r.status == NOT_YET for r in rs) and not any(
            r.status in (FAIL, ERROR, STALE) for r in rs):
        return "IN PROGRESS"
    return "BLOCKED"


def delivery_state(results: list[Result]) -> tuple[str, list[str]]:
    """§0.6. Only two endings.

    Computed over the ENTIRE registry, not just the gates measured in this run.
    Measuring one stage and reporting CHAMPION would be exactly the false claim
    this runner exists to prevent: a gate that was not measured has not passed,
    and §0.6 requires "every gate in every stage" for CHAMPION.
    """
    measured = {r.gate.gid for r in results}
    unmeasured = [g for gid, g in load_gates().items() if gid not in measured]
    if unmeasured:
        by_stage: dict[str, int] = {}
        for g in unmeasured:
            by_stage[g.stage] = by_stage.get(g.stage, 0) + 1
        return "NOT DELIVERABLE", [
            f"{n} gate(s) in {stage} were not measured in this run"
            for stage, n in sorted(by_stage.items())]

    reasons = []
    hard = [r for r in results if r.gate.type == "HARD"]
    soft = [r for r in results if r.gate.type == "SOFT"]

    hard_bad = [r for r in hard if not r.clears]
    if hard_bad:
        reasons = [f"{r.gate.gid} is {r.status}" for r in hard_bad]
        return "NOT DELIVERABLE", reasons

    soft_bad = [r for r in soft if not r.clears]
    if not soft_bad:
        pending = [r for r in results if r.status == PENDING_HUMAN]
        label = "CHAMPION"
        if pending:
            label = "CHAMPION — pending human validation"
        return label, [f"{r.gate.gid} awaits human results" for r in pending]

    return "NOT DELIVERABLE", [f"{r.gate.gid} is {r.status} with no valid waiver"
                               for r in soft_bad]


def _delta(gid: str, value, prev: dict | None) -> str:
    if prev is None or value is None:
        return "—"
    p = next((r for r in prev["results"] if r["gate"] == gid), None)
    if p is None or p.get("value") is None:
        return "new"
    d = value - p["value"]
    if abs(d) < 1e-12:
        return "="
    return f"{d:+g}"


def render(payload: dict, prev: dict | None) -> str:
    """Render one iteration's scorecard. This is the ONLY renderer; GAUNTLET.md
    sections are verified by re-running it (see check_scorecard_integrity.py),
    so a hand-edited table is detectable."""
    lines: list[str] = []
    lines.append(f"### Iteration {payload['iteration']} — {payload['timestamp']}")
    lines.append("")
    stages = ", ".join(f"**{s}** {payload['stage_state'][s]}"
                       for s in payload["stages_measured"])
    lines.append(f"Stages measured: {stages}  ")
    lines.append(f"Delivery state (§0.6): **{payload['delivery_state']}**")
    lines.append("")
    lines.append("| Gate | Title | Type | Metric target | Measured | Status | Δ |")
    lines.append("|---|---|---|---|---|---|---|")
    for r in payload["results"]:
        tgt = f"`{r['cmp']} {r['target']:g} {r['unit']}`"
        val = "—" if r["value"] is None else f"{r['value']:g} {r['unit']}"
        typ = f"{r['type']} [{r['class']}]"
        note = f"<br><sub>{r['note']}</sub>" if r["note"] else ""
        lines.append(
            f"| {r['gate']} | {r['title']} | {typ} | {tgt} | {val} | "
            f"{r['status']}{note} | {_delta(r['gate'], r['value'], prev)} |")
    lines.append("")
    if payload["ratchet_violations"]:
        lines.append("**Ratchet violations (§0.4):**")
        lines.append("")
        for v in payload["ratchet_violations"]:
            lines.append(f"- ✗ {v}")
        lines.append("")
    return "\n".join(lines)
