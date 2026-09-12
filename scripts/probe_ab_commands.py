#!/usr/bin/env python3
"""Time the same commands with two `ltx` binaries, alternately, on one repository.

A diagnostic beside the gates, never a gate: it exists so a measurement an ADR
quotes can be rerun from the command line rather than reconstructed from a
transcript. Given a repository (typically a copy-on-write clone of a gate run),
it creates a fresh workspace with the OLD binary, then for each command runs
old and new alternately N times, writing a new file before every run so a save
or a capture always has something to do, and prints the medians as JSON.

    python3 scripts/probe_ab_commands.py OLD NEW REPO [--runs 3] [-- CMD ...]

The binaries never share a process, so nothing carries over between runs but
the repository itself, which both change in the same ways.
"""
from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import time
from pathlib import Path

DEFAULT_COMMANDS = [
    "workspace list", "save probe", "switch main", "start line", "undo",
    "internals thin", "assign .", "status",
]


def run(binary: Path, argv: list[str], cwd: Path) -> float:
    start = time.monotonic()
    proc = subprocess.run([str(binary), *argv, "--json"], cwd=cwd,
                          capture_output=True, text=True, timeout=600)
    elapsed = (time.monotonic() - start) * 1000
    if proc.returncode != 0:
        print(f"  {binary.name} {' '.join(argv)}: exit {proc.returncode}: "
              f"{proc.stdout[:120]}", file=sys.stderr)
    return elapsed


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("old", type=Path)
    parser.add_argument("new", type=Path)
    parser.add_argument("repo", type=Path, help="a repository root; a workspace is created beside it")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("commands", nargs="*", default=DEFAULT_COMMANDS)
    args = parser.parse_args()

    old, new = args.old.resolve(), args.new.resolve()
    workspace = args.repo.resolve().parent / f"probe-ab-{int(time.time())}"
    made = subprocess.run([str(old), "workspace", "new", str(workspace), "--json"],
                          cwd=args.repo, capture_output=True, text=True)
    if made.returncode != 0:
        print(f"workspace new failed: {made.stdout[:200]}", file=sys.stderr)
        return 1

    results: dict[str, dict[str, list[float]]] = {"old": {}, "new": {}}
    for command in args.commands:
        argv = command.split()
        for _ in range(args.runs):
            for label, binary in (("old", old), ("new", new)):
                (workspace / f"probe-{time.time_ns()}.txt").write_text("x")
                results[label].setdefault(command, []).append(run(binary, argv, workspace))
    medians = {label: {command: round(statistics.median(times))
                       for command, times in per.items()}
               for label, per in results.items()}
    print(json.dumps({"runs": args.runs, "workspace": str(workspace),
                      "medians_ms": medians, "samples_ms": results}, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
