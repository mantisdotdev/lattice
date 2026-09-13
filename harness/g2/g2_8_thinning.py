#!/usr/bin/env python3
"""
G2.8 — Thinning safety (HARD).

`ltx thin` collects content nothing references. The gate holds it to the two
promises that make collection safe to run casually: nothing a checkpoint
holds is ever collected, and every thinning is itself an operation in the
op-log — collection that history cannot see is deletion wearing a uniform.
Measured value = violations over generated year-long traces. Target 0.

Three seeded traces simulate a year of daily work each: ~370 operations
drawn from a weighted pool (saves of new and modified files, assigns and
splits, line starts and switches, undos, the odd redaction and second
workspace), with `thin` run every 25 operations — the way a daemon or a
habit would run it. After EVERY thin:

  1. `verify --complete` must come back ok with no errors — a reachable
     chunk the collector took is exactly what this catches;
  2. four checkpoints of the current forensic log (first, last, two seeded
     picks) must `checkout` cleanly into fresh directories — content must
     not merely hash-verify, it must still materialise (a redacted file
     surfacing as a checkout collision is the redaction contract working,
     not a violation: exit 0 is the bar);
  3. the thin's reported `oplog_seq` is recorded.

At each trace's end, `internals oplog` must contain every recorded seq as
an operation of kind "thin". A thinning the op-log cannot name is a
violation even when no byte was lost.

Determinism: one seeded generator drives file contents, operation choice
and checkpoint sampling; no wall clock, no randomness outside the seed
(SEED constant below). Traces run in fresh scratch repositories.

Coverage contract (§6): all traces must complete, at least MIN_THINS
thinnings must actually have run, and every operation kind in the pool
must have been drawn at least once — a trace that never thinned measures
nothing.

Written after the surface it measures (the reverse of §0.3's order, same
as its G2 siblings, said rather than hidden). Shown to FAIL before
freezing: against a wrapper that filters thin entries out of
`internals oplog`, every thinning is reported absent (`--ltx` exists for
exactly that check and for nothing else).
"""
from __future__ import annotations

import argparse
import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LTX = REPO / "target" / "release" / "ltx"
GATE = "G2.8"
TIMEOUT_S = 300
SEED = 20260913
TRACES = 3
OPS_PER_TRACE = 370
THIN_EVERY = 25
CHECKOUT_SAMPLES = 4
MIN_THINS = 30


def run(argv: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run([str(LTX), *argv, "--json"], cwd=cwd, capture_output=True,
                          text=True, errors="replace", timeout=TIMEOUT_S, check=False)


def jdoc(proc: subprocess.CompletedProcess) -> dict:
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        return {}


class Trace:
    """One simulated year in one scratch repository."""

    def __init__(self, root: Path, rng: random.Random, name: str):
        self.repo = root / name
        self.repo.mkdir()
        self.rng = rng
        self.saved_files: list[str] = []
        self.lines = ["main"]
        self.day = 0
        self.violations: list[dict] = []
        self.kinds_drawn: set[str] = set()
        self.thin_seqs: list[int] = []
        self.workspaces = 0
        r = run(["init"], self.repo)
        if r.returncode != 0:
            raise RuntimeError(f"init failed: {r.stdout[:200]}")

    # -------------------------------------------------------------- op pool
    def op_save(self):
        name = f"f{self.rng.randrange(40):02d}.txt"
        (self.repo / name).write_text(
            f"day {self.day}: {self.rng.getrandbits(64):x}\n")
        r = run(["save", f"day {self.day}"], self.repo)
        if r.returncode == 0 and name not in self.saved_files:
            self.saved_files.append(name)

    def op_assign(self):
        name = f"a{self.rng.randrange(10)}.txt"
        (self.repo / name).write_text(f"assigned on day {self.day}\n")
        run(["assign", name], self.repo)

    def op_split(self):
        run(["split"], self.repo)

    def op_line(self):
        if self.rng.random() < 0.5 or len(self.lines) == 1:
            name = f"line-{len(self.lines)}"
            if run(["start", name], self.repo).returncode == 0:
                self.lines.append(name)
        else:
            run(["switch", self.rng.choice(self.lines)], self.repo)

    def op_undo(self):
        run(["undo"], self.repo)

    def op_workspace(self):
        if self.workspaces < 3:
            run(["workspace", "new", f"../{self.repo.name}-ws{self.workspaces}"],
                self.repo)
            self.workspaces += 1

    def op_redact(self):
        if self.saved_files:
            victim = self.rng.choice(self.saved_files)
            run(["redact", victim, "--confirm-destroy"], self.repo)

    POOL = [
        ("save", op_save, 60),
        ("assign", op_assign, 8),
        ("split", op_split, 4),
        ("line", op_line, 10),
        ("undo", op_undo, 8),
        ("workspace", op_workspace, 2),
        ("redact", op_redact, 3),
    ]

    # ------------------------------------------------------------- thinning
    def thin_and_check(self):
        out = jdoc(run(["thin"], self.repo))
        if out.get("ok") is not True:
            self.violations.append({"day": self.day, "violation": "thin failed",
                                    "detail": str(out)[:200]})
            return
        self.thin_seqs.append(out["oplog_seq"])

        v = jdoc(run(["verify", "--complete"], self.repo))
        if not (v.get("ok") is True and not v.get("errors")):
            self.violations.append({
                "day": self.day, "violation": "reachable content lost",
                "detail": json.dumps(v.get("errors", "no verify document"))[:300]})

        log = jdoc(run(["log", "--forensic"], self.repo)).get("checkpoints", [])
        if log:
            picks = {0, len(log) - 1}
            while len(picks) < min(CHECKOUT_SAMPLES, len(log)):
                picks.add(self.rng.randrange(len(log)))
            for i in sorted(picks):
                cp = log[i]["id"]
                dest = self.repo.parent / f"{self.repo.name}-co-{self.day}-{i}"
                r = run(["checkout", "--checkpoint", cp, "--into", str(dest)],
                        self.repo)
                if r.returncode != 0:
                    self.violations.append({
                        "day": self.day,
                        "violation": "checkpoint no longer materialises after thin",
                        "detail": f"{cp[:16]}: {r.stdout[:200]}"})

    # --------------------------------------------------------------- driver
    def year(self):
        names = [name for name, _, weight in self.POOL for _ in range(weight)]
        for _ in range(OPS_PER_TRACE):
            self.day += 1
            name = self.rng.choice(names)
            self.kinds_drawn.add(name)
            dict((n, f) for n, f, _ in self.POOL)[name](self)
            if self.day % THIN_EVERY == 0:
                self.thin_and_check()

    def audit_oplog(self):
        doc = jdoc(run(["internals", "oplog"], self.repo))
        kinds = {e["seq"]: e["operation"].get("kind") for e in doc.get("operations", [])}
        for seq in self.thin_seqs:
            if kinds.get(seq) != "thin":
                self.violations.append({
                    "violation": "thinning absent from op-log",
                    "detail": f"seq {seq} is {kinds.get(seq)!r}"})


def main() -> int:
    global LTX
    ap = argparse.ArgumentParser()
    ap.add_argument("--ltx", type=Path, default=LTX,
                    help="binary under test; exists so this harness can be shown to fail")
    args = ap.parse_args()
    LTX = args.ltx.resolve()
    if not LTX.exists():
        print(json.dumps({"gate": GATE, "status": "not-implemented",
                          "note": "ltx binary not built (target/release/ltx)"}))
        return 0

    rng = random.Random(SEED)
    work = Path(tempfile.mkdtemp(prefix="g2-8-"))
    violations, kinds, thins, traces_done = [], set(), 0, 0
    try:
        for t in range(TRACES):
            try:
                trace = Trace(work, rng, f"year{t}")
                trace.year()
                trace.audit_oplog()
            except (RuntimeError, subprocess.TimeoutExpired, OSError,
                    KeyError, IndexError) as exc:
                violations.append({"violation": "trace did not complete",
                                   "detail": f"year{t}: {exc}"[:200]})
                continue
            traces_done += 1
            violations.extend(trace.violations)
            kinds |= trace.kinds_drawn
            thins += len(trace.thin_seqs)
    finally:
        import shutil
        shutil.rmtree(work, ignore_errors=True)

    pool_kinds = {name for name, _, _ in Trace.POOL}
    coverage_ok = (traces_done == TRACES and thins >= MIN_THINS
                   and kinds == pool_kinds)
    coverage_note = "; ".join(
        ([f"only {traces_done} of {TRACES} traces completed"]
         if traces_done != TRACES else [])
        + ([f"only {thins} thinnings ran, {MIN_THINS} required"]
           if thins < MIN_THINS else [])
        + ([f"operation kinds never drawn: {', '.join(sorted(pool_kinds - kinds))}"]
           if kinds != pool_kinds else []))

    print(json.dumps({
        "gate": GATE,
        "value": len(violations),
        "unit": "violations",
        "note": (f"{len(violations)} violation(s) over {TRACES} year-long traces; "
                 f"{thins} thinnings, each followed by a complete verify "
                 f"and {CHECKOUT_SAMPLES} checkout probes"),
        "detail": {"violations": violations[:50]},
        "coverage": {"ok": coverage_ok, "note": coverage_note},
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
