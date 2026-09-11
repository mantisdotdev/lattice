#!/usr/bin/env python3
"""
G1.3 — Universal undo (HARD).

Target: 100% of state-changing commands undoable, with the exclusions
DISAGREEMENTS Challenge 12 enumerated and ADR-20 made this harness carry: the
commands the binary publishes as `undoable: false` must REFUSE undo — no undo
entry may ever name one — and what a redaction destroyed is not required to
return, because it must not.

Amended under ADR-20 (§0.3): reads `undoable` from the discovered surface;
asserts refusal from the op-log; excludes redacted paths from the working-tree
comparison; and reports an in-process run that exceeds its budget as a failed
measurement rather than crashing or calling the subject not built. >= 100,000 property-generated
command sequences in-process against `ltx-core`, plus >= 1,000 end-to-end through
the CLI binary; undo-all restores initial state: 0 failures.

Equality domain, exactly as the gate specifies: user-visible state -- working-tree
bytes, checkpoint graph, lines, changesets -- explicitly EXCLUDING the op-log and
ephemeral autosnapshots. Comparing the op-log would make the gate unsatisfiable by
construction, since undo is itself an operation that the op-log must record.

§6's coverage contract is the part of this harness that does the real work:

    "the operation generator auto-enumerates the full state-changing command/API
     surface (so new commands cannot silently dodge) and must emit each >= 100
     times, including sync, redaction, thinning, lens edits, and undo-of-undo"

So the command list is DISCOVERED from the built binary's own machine-readable
command surface, never hand-written here. A hand-written list is a list that
stops being complete the moment someone adds a command, and the whole point of
the coverage contract is that the gate notices.
"""
from __future__ import annotations

import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LTX = REPO / "target" / "release" / "ltx"
CORE_HARNESS = REPO / "target" / "release" / "undo-property"

SEED = 20260903
IN_PROCESS_SEQUENCES = 100_000
CLI_SEQUENCES = 1_000
MIN_EMISSIONS_PER_COMMAND = 100
# The in-process half's wall-clock budget. It was inline; a timeout is now a
# reported outcome, so the number has a name.
IN_PROCESS_BUDGET_S = 7200

# §6 names these explicitly; if the discovered surface lacks any of them the
# coverage contract is not satisfied, regardless of the emission counts.
REQUIRED_OPERATIONS = {
    "save", "undo", "start", "switch", "assign", "split",
    "sync", "redact", "thin", "lens", "merge", "workspace",
}


def not_implemented(note: str) -> int:
    print(json.dumps({"gate": "G1.3", "status": "not-implemented", "note": note}))
    return 0


def discover_command_surface() -> tuple[list[dict], str] | None:
    """Enumerate state-changing operations from the binary itself.

    Preference order:
      1. `ltx internals command-surface --json` -- the machine-readable contract
         G2.5 requires the CLI to publish. This is authoritative.
      2. `ltx --help --json`, as a fallback during early development.

    A hand-maintained list is deliberately NOT a fallback: if discovery fails the
    harness reports not-implemented rather than measuring against a list that
    could silently omit a command.
    """
    for argv, source in (
        ([str(LTX), "internals", "command-surface", "--json"], "command-surface"),
        ([str(LTX), "--help", "--json"], "help --json"),
    ):
        try:
            r = subprocess.run(argv, capture_output=True, text=True,
                               errors="replace", timeout=60)
        except (OSError, subprocess.TimeoutExpired):
            continue
        if r.returncode != 0 or not r.stdout.strip():
            continue
        try:
            doc = json.loads(r.stdout)
        except json.JSONDecodeError:
            continue
        commands = doc.get("commands") if isinstance(doc, dict) else doc
        if not isinstance(commands, list) or not commands:
            continue
        mutating = [c for c in commands
                    if isinstance(c, dict) and c.get("state_changing")]
        if mutating:
            return mutating, source
    return None


def snapshot_user_visible_state(root: Path) -> dict:
    """Capture exactly the equality domain the gate names, and nothing else.

    Working-tree bytes are hashed per path; the checkpoint graph, lines and
    changesets are read through the JSON contract. The op-log and ephemeral
    autosnapshots are deliberately absent -- including them would compare state
    the gate explicitly excludes.
    """
    import hashlib

    tree: dict[str, str] = {}
    rootb = os.fsencode(root)
    for dirpath, dirnames, filenames in os.walk(rootb):
        if b".lattice" in Path(os.fsdecode(dirpath)).parts[0:1] or \
                os.path.basename(dirpath) == b".lattice":
            dirnames[:] = []
            continue
        dirnames[:] = [d for d in dirnames if d != b".lattice"]
        for name in filenames:
            full = os.path.join(dirpath, name)
            rel = os.fsdecode(os.path.relpath(full, rootb))
            try:
                if os.path.islink(full):
                    tree[rel] = "link:" + os.readlink(full)
                else:
                    with open(full, "rb") as fh:
                        h = hashlib.sha256()
                        for block in iter(lambda: fh.read(1 << 20), b""):
                            h.update(block)
                    tree[rel] = h.hexdigest()
            except OSError:
                tree[rel] = "unreadable"

    def query(*args: str):
        r = subprocess.run([str(LTX), *args, "--json"], cwd=root,
                           capture_output=True, text=True, errors="replace",
                           timeout=300)
        if r.returncode != 0:
            return {"error": r.stderr.strip()[:200]}
        try:
            return json.loads(r.stdout)
        except json.JSONDecodeError:
            return {"error": "unparseable"}

    return {
        "working_tree": tree,
        "checkpoints": query("log", "--forensic"),
        "lines": query("line", "list"),
        "changes": query("change", "list"),
    }


def run_cli_sequences(count: int, commands: list[dict], rng: random.Random) -> dict:
    """End-to-end sequences through the CLI binary, each fully undone."""
    failures: list[dict] = []
    emitted: dict[str, int] = {c["name"]: 0 for c in commands}
    completed = 0
    # ADR-20: the non-undoable set is what the binary PUBLISHES, not a list
    # kept here. A command that claims to be undoable and is not fails the
    # comparison below; one that claims not to be and is reversed anyway fails
    # the refusal check.
    non_undoable = {c["name"] for c in commands if not c.get("undoable", True)}

    for i in range(count):
        work = Path(tempfile.mkdtemp(prefix="g1-3-cli-"))
        try:
            subprocess.run([str(LTX), "init"], cwd=work, capture_output=True,
                           timeout=120)
            (work / "seed.txt").write_bytes(b"seed\n")
            subprocess.run([str(LTX), "save", "seed"], cwd=work,
                           capture_output=True, timeout=120)
            initial = snapshot_user_visible_state(work)

            length = rng.randint(1, 12)
            applied = 0
            # Op-log positions of the non-undoable operations this sequence
            # performed, and the paths its redactions destroyed.
            pinned: dict[int, str] = {}
            redacted: set[str] = set()
            for _ in range(length):
                cmd = rng.choice(commands)
                argv = [str(LTX), *cmd["name"].split(), *cmd.get("sample_args", [])]
                r = subprocess.run([*argv, "--json"], cwd=work, capture_output=True,
                                   text=True, errors="replace", timeout=300)
                emitted[cmd["name"]] += 1
                if r.returncode == 0:
                    applied += 1
                    try:
                        doc = json.loads(r.stdout)
                    except json.JSONDecodeError:
                        doc = {}
                    if cmd["name"] in non_undoable and doc.get("oplog_seq") is not None:
                        pinned[int(doc["oplog_seq"])] = cmd["name"]
                    if cmd["name"].split()[0] == "redact" and doc.get("target"):
                        redacted.add(os.path.normpath(str(doc["target"])))

            # Undo everything, then compare. Undo must also undo undo (redo),
            # so the loop runs until undo reports nothing left rather than a
            # fixed count.
            for _ in range(applied * 3 + 8):
                r = subprocess.run([str(LTX), "undo", "--json"], cwd=work,
                                   capture_output=True, text=True,
                                   errors="replace", timeout=300)
                if r.returncode != 0:
                    break
                try:
                    if json.loads(r.stdout).get("nothing_to_undo"):
                        break
                except json.JSONDecodeError:
                    break

            # ADR-20 (2): nothing published as non-undoable may have been
            # reversed. The op-log is the authority on what undo did.
            reversed_pins = []
            if pinned:
                r = subprocess.run([str(LTX), "internals", "oplog", "--json"], cwd=work,
                                   capture_output=True, text=True, errors="replace",
                                   timeout=300)
                try:
                    ops = json.loads(r.stdout).get("operations", []) if r.returncode == 0 else []
                except json.JSONDecodeError:
                    ops = []
                for op in ops:
                    kind = (op.get("operation") or {}).get("kind")
                    target = (op.get("operation") or {}).get("undone_seq")
                    if kind == "undo" and target in pinned:
                        reversed_pins.append(f"{pinned[target]} at {target}")
            if reversed_pins:
                failures.append({"sequence": i,
                                 "differing_domains": ["non-undoable operation reversed: "
                                                       + ", ".join(reversed_pins)]})

            final = snapshot_user_visible_state(work)
            # ADR-20 (3): what a redaction destroyed is not required to
            # return. Dropped from BOTH sides, and only those paths.
            for snap in (initial, final):
                tree = snap.get("working_tree")
                if isinstance(tree, dict):
                    for path in redacted:
                        tree.pop(path, None)
            if final != initial:
                differing = sorted(
                    k for k in set(initial) | set(final)
                    if initial.get(k) != final.get(k))
                failures.append({"sequence": i, "differing_domains": differing})
            completed += 1
        except subprocess.TimeoutExpired:
            failures.append({"sequence": i, "differing_domains": ["timeout"]})
        finally:
            shutil.rmtree(work, ignore_errors=True)

    return {"completed": completed, "failures": failures, "emitted": emitted}


def main() -> int:
    if not LTX.exists():
        return not_implemented("ltx binary not built yet (Stage G0 forbids product code)")

    surface = discover_command_surface()
    if surface is None:
        return not_implemented(
            "could not auto-enumerate the state-changing command surface; "
            "§6 forbids substituting a hand-written command list")
    commands, source = surface
    names = {c["name"].split()[0] for c in commands}

    missing_required = sorted(REQUIRED_OPERATIONS - names)

    rng = random.Random(SEED)
    core_result = None
    if CORE_HARNESS.exists():
        try:
            r = subprocess.run([str(CORE_HARNESS), "--sequences",
                                str(IN_PROCESS_SEQUENCES), "--seed", str(SEED),
                                "--json"], capture_output=True, text=True,
                               errors="replace", timeout=IN_PROCESS_BUDGET_S)
        except subprocess.TimeoutExpired:
            # ADR-20 addendum: a run that exceeds its budget is a measurement
            # that failed coverage, not a subject that was never built. Zero
            # sequences completed is what the coverage contract below sees.
            r = None
            core_result = {"sequences": 0, "failures": 0,
                           "note": f"in-process harness exceeded {IN_PROCESS_BUDGET_S} s"}
        if r is not None and r.returncode == 0 and r.stdout.strip():
            try:
                core_result = json.loads(r.stdout.splitlines()[-1])
            except json.JSONDecodeError:
                core_result = None
    if core_result is None:
        return not_implemented(
            "in-process property harness (target/release/undo-property) not built; "
            f"G1.3 requires >= {IN_PROCESS_SEQUENCES:,} in-process sequences")

    cli_result = run_cli_sequences(CLI_SEQUENCES, commands, rng)

    emitted = {k: core_result.get("emitted", {}).get(k, 0) + v
               for k, v in cli_result["emitted"].items()}
    under_emitted = sorted(k for k, v in emitted.items()
                           if v < MIN_EMISSIONS_PER_COMMAND)

    coverage_problems = []
    if missing_required:
        coverage_problems.append(
            f"discovered surface omits required operations: {', '.join(missing_required)}")
    if under_emitted:
        coverage_problems.append(
            f"{len(under_emitted)} commands emitted < {MIN_EMISSIONS_PER_COMMAND} "
            f"times: {', '.join(under_emitted[:8])}")
    if core_result.get("sequences", 0) < IN_PROCESS_SEQUENCES:
        coverage_problems.append(
            f"only {core_result.get('sequences', 0):,} in-process sequences, "
            f"{IN_PROCESS_SEQUENCES:,} required")
    if cli_result["completed"] < CLI_SEQUENCES:
        coverage_problems.append(
            f"only {cli_result['completed']:,} CLI sequences completed, "
            f"{CLI_SEQUENCES:,} required")

    failures = core_result.get("failures", 0) + len(cli_result["failures"])

    print(json.dumps({
        "gate": "G1.3",
        "value": failures,
        "unit": "failures",
        "note": (f"{failures} failures over "
                 f"{core_result.get('sequences', 0):,} in-process + "
                 f"{cli_result['completed']:,} CLI sequences"),
        "coverage": {
            "ok": not coverage_problems,
            "note": "; ".join(coverage_problems),
        },
        "detail": {
            "surface_source": source,
            "state_changing_commands": sorted(names),
            "emissions_per_command": emitted,
            "in_process": {k: core_result.get(k)
                           for k in ("sequences", "failures", "seed")},
            "cli_failures": cli_result["failures"][:20],
        },
        "evidence": ["harness/g1/g1_3_universal_undo.py"],
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
