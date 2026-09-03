#!/usr/bin/env python3
"""
Mine ground-truthed rename/move instances for the G0.4 corpus.

Ground truth policy — stated plainly, because "ground-truthed" is doing a lot of
work in the gate text and an automatically-derived oracle must say how it was
derived:

  Git's own rename detection is used only as a CANDIDATE GENERATOR. Every
  candidate is then re-verified with an independent similarity measure computed
  here from the blob contents (Dice coefficient over normalised line multisets),
  so the corpus does not inherit git's rename heuristic as its own ground truth.
  That matters: G4.1/G4.2 measure Lattice's matcher, and scoring a matcher
  against another matcher's output measures agreement, not correctness.

  TIER_A  identical content, different path      -> similarity == 1.0
          Unambiguous. A move with no edit is ground truth by construction:
          the bytes are the same object at a different name.
  TIER_B  moved and edited                       -> 0.90 <= similarity < 1.0
  TIER_C  weak candidate                         -> 0.50 <= similarity < 0.90
          Mined and recorded, but EXCLUDED from the ground-truth set.

Only TIER_A and TIER_B count toward the gate. A stratified sample is written for
manual spot-checking so the automatic derivation is auditable.
"""
from __future__ import annotations
import argparse
import collections
import concurrent.futures as cf
import json
import os
import random
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
REPOS = REPO / "corpus" / "data" / "repos"
MANIFEST = REPO / "corpus" / "manifests" / "mining-repos.tsv"
OUT = REPO / "corpus" / "data" / "refactorings.jsonl"
PARTIAL = REPO / "corpus" / "data" / "refactorings.partial.jsonl"
SPOT = REPO / "corpus" / "data" / "refactorings-spotcheck.json"

SEED = 20260903
MAX_COMMITS = 12000          # per repo, most recent
TIER_B_MIN = 0.90
TIER_C_MIN = 0.50
MAX_BLOB = 512 * 1024        # bounded: never read an unbounded blob

EXT_LANG = {
    ".py": "Python", ".rs": "Rust", ".go": "Go", ".java": "Java",
    ".ts": "TypeScript", ".tsx": "TypeScript", ".js": "JavaScript",
    ".jsx": "JavaScript", ".c": "C", ".h": "C", ".cc": "C++", ".cpp": "C++",
    ".rb": "Ruby", ".php": "PHP", ".scala": "Scala", ".kt": "Kotlin",
}
WS = re.compile(r"\s+")


def git(repo: Path, *args, binary=False, timeout=600):
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True,
                          timeout=timeout, text=not binary,
                          errors=None if binary else "replace")


def norm_lines(data: bytes) -> list[str] | None:
    if b"\x00" in data[:8000]:
        return None
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        return None
    return [WS.sub(" ", ln).strip() for ln in text.splitlines()
            if WS.sub(" ", ln).strip()]


def dice(a: list[str], b: list[str]) -> float:
    """Dice coefficient over line multisets. O(n): no quadratic diffing."""
    if not a and not b:
        return 1.0
    if not a or not b:
        return 0.0
    ca, cb = collections.Counter(a), collections.Counter(b)
    inter = sum((ca & cb).values())
    return 2.0 * inter / (len(a) + len(b))


def read_blob(repo: Path, rev: str, path: str) -> bytes | None:
    r = git(repo, "cat-file", "-s", f"{rev}:{path}", timeout=60)
    if r.returncode != 0:
        return None
    try:
        if int(r.stdout.strip()) > MAX_BLOB:
            return None
    except ValueError:
        return None
    b = git(repo, "show", f"{rev}:{path}", binary=True, timeout=60)
    return b.stdout if b.returncode == 0 else None


def mine(slug: str, lang_hint: str) -> list[dict]:
    path = REPOS / (slug.replace("/", "__") + ".git")
    if not (path / "HEAD").exists():
        return []
    try:
        # Candidate generation only. -M50% is deliberately permissive so that
        # our own re-verification, not git's threshold, sets the bar.
        r = git(path, "log", f"-n{MAX_COMMITS}", "--no-merges", "--all",
                "--diff-filter=R", "--find-renames=50%", "--name-status",
                "--format=%x1eC%H", "-z", timeout=1800)
    except subprocess.TimeoutExpired:
        print(f"  {slug}: TIMEOUT", file=sys.stderr)
        return []
    if r.returncode != 0:
        return []

    out: list[dict] = []
    for block in r.stdout.split("\x1e"):
        if not block.startswith("C"):
            continue
        fields = block.split("\0")
        sha = fields[0][1:].strip()
        i = 1
        while i < len(fields):
            f = fields[i]
            if f.startswith("R") and i + 2 < len(fields):
                old, new = fields[i + 1], fields[i + 2]
                i += 3
                ext_old = Path(old).suffix.lower()
                ext_new = Path(new).suffix.lower()
                lang = EXT_LANG.get(ext_new)
                if lang is None or old == new:
                    continue
                b_old = read_blob(path, f"{sha}~1", old)
                b_new = read_blob(path, sha, new)
                if b_old is None or b_new is None:
                    continue
                l_old, l_new = norm_lines(b_old), norm_lines(b_new)
                if l_old is None or l_new is None:
                    continue
                sim = dice(l_old, l_new)
                if sim < TIER_C_MIN:
                    continue
                tier = ("TIER_A" if b_old == b_new else
                        "TIER_B" if sim >= TIER_B_MIN else "TIER_C")
                out.append({
                    "repo": slug, "language": lang, "commit": sha,
                    "old_path": old, "new_path": new,
                    "similarity": round(sim, 5), "tier": tier,
                    "kind": ("rename_in_place"
                             if str(Path(old).parent) == str(Path(new).parent)
                             else "cross_directory_move"),
                    "extension_changed": ext_old != ext_new,
                    "lines_old": len(l_old), "lines_new": len(l_new),
                })
            else:
                i += 1
    print(f"  {slug:<40} {len(out):>6} candidates "
          f"(A={sum(1 for o in out if o['tier']=='TIER_A')} "
          f"B={sum(1 for o in out if o['tier']=='TIER_B')})",
          file=sys.stderr, flush=True)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--jobs", type=int, default=6)
    args = ap.parse_args()

    rows = []
    for line in MANIFEST.read_text().splitlines():
        if line.startswith("#") or not line.strip():
            continue
        p = line.split("\t")
        if len(p) >= 2:
            rows.append((p[0], p[1]))

    # Checkpoint each repository as it completes. An earlier version accumulated
    # every record in memory and wrote once at the end, so a crash or a kill an
    # hour into a run destroyed all of it and left the gate unmeasurable. The
    # partial file is valid input for the harness -- it just has fewer repos.
    #
    # This changes WHEN records are written, never WHICH records are produced:
    # mine() and its tier classification are untouched, and the final file is
    # sorted into the same deterministic order as before.
    PARTIAL.parent.mkdir(parents=True, exist_ok=True)
    all_out: list[dict] = []
    with PARTIAL.open("w") as partial:
        with cf.ThreadPoolExecutor(max_workers=args.jobs) as ex:
            for recs in ex.map(lambda r: mine(*r), rows):
                all_out.extend(recs)
                for r in recs:
                    partial.write(json.dumps(r, sort_keys=True) + "\n")
                partial.flush()
                os.fsync(partial.fileno())

    all_out.sort(key=lambda r: (r["repo"], r["commit"], r["new_path"]))
    with OUT.open("w") as fh:
        for r in all_out:
            fh.write(json.dumps(r, sort_keys=True) + "\n")
    PARTIAL.unlink(missing_ok=True)

    truth = [r for r in all_out if r["tier"] in ("TIER_A", "TIER_B")]
    rng = random.Random(SEED)
    SPOT.write_text(json.dumps({
        "seed": SEED,
        "note": "Stratified sample for manual audit of the automatic ground-truth "
                "derivation. Each entry can be re-checked with: "
                "git -C <repo> show <commit> -- <new_path>",
        "sample": rng.sample(truth, min(60, len(truth))),
    }, indent=2) + "\n")

    langs = collections.Counter(r["language"] for r in truth)
    repos = collections.Counter(r["repo"] for r in truth)
    tiers = collections.Counter(r["tier"] for r in all_out)
    print(f"\nground-truth instances: {len(truth)} "
          f"(A={tiers['TIER_A']} B={tiers['TIER_B']}; C={tiers['TIER_C']} excluded)",
          file=sys.stderr)
    print(f"repos={len(repos)} languages={len(langs)}: "
          f"{dict(langs.most_common())}", file=sys.stderr)
    print(f"kinds: {dict(collections.Counter(r['kind'] for r in truth))}",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
