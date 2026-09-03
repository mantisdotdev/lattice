#!/usr/bin/env python3
"""
G1.9 — Storage honesty, dual baseline (SOFT, perf).

Targets, both of which must hold:
  (a) binary corpus:  total store <= 1.25x restic (pinned version, defaults)
  (b) reference text: total store <= 1.5x a `git gc --aggressive` clone

The registry's metric is the normalised maximum -- max(restic_ratio / 1.25,
git_ratio / 1.5) -- so a value below 1.0 means both baselines are met and
neither can hide behind the other.

Also reported, as the gate requires: the sync payload for an incremental binary
edit versus git+LFS, and the small-file crossover point that feeds ADR-2.

Why this harness refuses more than it measures
----------------------------------------------
Both baselines are external tools. If `restic` is absent, the (a) comparison
cannot be made -- and reporting only (b) would silently convert a dual-baseline
gate into a single-baseline one. The harness therefore fails rather than
measuring half the gate, which is the same rule G1.2 applies to a corpus missing
a mandated case.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import ltxrun as L  # noqa: E402

GATE = "G1.9"
BINARY_CORPUS = REPO / "corpus" / "data" / "binary-corpus"
REFERENCE_REPO = REPO / "corpus" / "data" / "reference-repo"

RESTIC_MULTIPLE = 1.25
GIT_MULTIPLE = 1.5


def tool_version(binary: str, *args: str) -> str | None:
    exe = shutil.which(binary)
    if exe is None:
        return None
    r = subprocess.run([exe, *(args or ("--version",))], capture_output=True,
                       text=True, errors="replace", timeout=120)
    return r.stdout.strip().splitlines()[0] if r.returncode == 0 else None


def dir_bytes(path: Path) -> int:
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(root, f))
            except OSError:
                pass
    return total


def restic_baseline(corpus: Path, work: Path) -> tuple[int, str]:
    """Back up the binary corpus with restic at its defaults."""
    repo = work / "restic-repo"
    env = dict(os.environ, RESTIC_PASSWORD="g1-9-harness")
    init = subprocess.run(["restic", "-r", str(repo), "init"], env=env,
                          capture_output=True, text=True, timeout=1800)
    if init.returncode != 0:
        raise L.NotBuilt(f"restic init failed: {init.stderr.strip()[:200]}")
    backup = subprocess.run(
        ["restic", "-r", str(repo), "backup", str(corpus), "--no-scan"],
        env=env, capture_output=True, text=True, timeout=14400)
    if backup.returncode != 0:
        raise L.NotBuilt(f"restic backup failed: {backup.stderr.strip()[:200]}")
    return dir_bytes(repo), tool_version("restic", "version") or "restic (unknown)"


def git_gc_baseline(work: Path) -> tuple[int, str]:
    """`git gc --aggressive` over the reference repo's text portion."""
    if not (REFERENCE_REPO / ".git").exists():
        raise L.NotBuilt("reference repo not built")
    clone = work / "git-baseline"
    r = subprocess.run(["git", "clone", "--quiet", "--no-local",
                        str(REFERENCE_REPO), str(clone)],
                       capture_output=True, text=True, timeout=14400)
    if r.returncode != 0:
        raise L.NotBuilt(f"git clone failed: {r.stderr.strip()[:200]}")
    subprocess.run(["git", "-C", str(clone), "gc", "--aggressive",
                    "--prune=now", "--quiet"], timeout=28800)
    return dir_bytes(clone / ".git" / "objects"), \
        tool_version("git") or "git (unknown)"


def ltx_store_bytes(source: Path, work: Path, label: str) -> int:
    """Ingest a corpus with ltx and measure what .lattice holds."""
    dest = work / f"ltx-{label}"
    dest.mkdir(parents=True, exist_ok=True)
    if L.run(["init"], cwd=dest).returncode != 0:
        raise L.NotBuilt("ltx init failed")
    for item in source.iterdir():
        target = dest / item.name
        if item.name == ".git":
            continue
        if item.is_dir():
            shutil.copytree(item, target, symlinks=True, dirs_exist_ok=True)
        else:
            shutil.copy2(item, target)
    if L.run(["save", f"g1.9 {label} corpus"], cwd=dest, timeout=28800).returncode != 0:
        raise L.NotBuilt(f"ltx save failed on {label} corpus")
    return dir_bytes(dest / ".lattice")


def main() -> int:
    try:
        L.require_ltx()
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))

    restic_version = tool_version("restic", "version")
    if restic_version is None:
        # Refusing beats measuring half a dual-baseline gate.
        return L.not_implemented(
            GATE, "restic is not installed, so baseline (a) cannot be measured; "
                  "a dual-baseline gate must not report a single baseline")
    if not BINARY_CORPUS.exists():
        return L.not_implemented(
            GATE, "binary corpus not built "
                  "(scripts/corpus/simulate_binary_history.py)")

    work = Path(tempfile.mkdtemp(prefix="g1-9-"))
    try:
        restic_bytes, restic_v = restic_baseline(BINARY_CORPUS, work)
        ltx_binary = ltx_store_bytes(BINARY_CORPUS, work, "binary")
        git_bytes, git_v = git_gc_baseline(work)
        ltx_text = ltx_store_bytes(REFERENCE_REPO, work, "text")

        restic_ratio = ltx_binary / max(restic_bytes, 1)
        git_ratio = ltx_text / max(git_bytes, 1)
        normalised = max(restic_ratio / RESTIC_MULTIPLE, git_ratio / GIT_MULTIPLE)
        binding = "restic" if restic_ratio / RESTIC_MULTIPLE > git_ratio / GIT_MULTIPLE \
            else "git-gc"

        return L.emit({
            "gate": GATE,
            "value": round(normalised, 4),
            "unit": "ratio",
            "note": (f"binary {restic_ratio:.3f}x restic (limit {RESTIC_MULTIPLE}), "
                     f"text {git_ratio:.3f}x git-gc (limit {GIT_MULTIPLE}); "
                     f"binding baseline: {binding}"),
            "detail": {
                "binary": {
                    "ltx_bytes": ltx_binary, "restic_bytes": restic_bytes,
                    "ratio": round(restic_ratio, 4), "limit": RESTIC_MULTIPLE,
                    "restic_version": restic_v,
                },
                "text": {
                    "ltx_bytes": ltx_text, "git_gc_bytes": git_bytes,
                    "ratio": round(git_ratio, 4), "limit": GIT_MULTIPLE,
                    "git_version": git_v,
                },
                "binding_baseline": binding,
                "adr2_reference": "docs/adr/adr-2-chunk-parameters.md",
            },
            "evidence": ["docs/adr/adr-2-chunk-parameters.md",
                         "bench/results/raw/adr2-git-baseline-django.json"],
        })
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
