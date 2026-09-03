#!/usr/bin/env python3
"""
Mine real merge commits for the G0.3 corpus.

Emits one JSON record per two-parent merge commit found in the pinned bare
clones. Metadata only at this stage; replay and baseline measurement happen in
replay_merges.py, so that mining and measuring are separable and the mined set
can be hash-pinned before any baseline is computed.
"""
from __future__ import annotations
import argparse
import concurrent.futures as cf
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
REPOS = REPO / "corpus" / "data" / "repos"
MANIFEST = REPO / "corpus" / "manifests" / "mining-repos.tsv"
OUT = REPO / "corpus" / "data" / "merges.jsonl"


def load_manifest() -> list[tuple[str, str]]:
    rows = []
    for line in MANIFEST.read_text().splitlines():
        if line.startswith("#") or not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 2:
            rows.append((parts[0], parts[1]))
    return rows


def mine(slug: str, lang: str) -> list[dict]:
    path = REPOS / (slug.replace("/", "__") + ".git")
    if not (path / "HEAD").exists():
        return []
    try:
        # --all so we catch merges on every ref, not just the default branch.
        out = subprocess.run(
            ["git", "-C", str(path), "rev-list", "--merges", "--parents",
             "--all", "--format=%x1f%H%x1f%ct%x1f%an%x1e"],
            capture_output=True, text=True, timeout=1800, errors="replace")
    except subprocess.TimeoutExpired:
        print(f"  {slug}: TIMEOUT", file=sys.stderr)
        return []
    if out.returncode != 0:
        print(f"  {slug}: rev-list failed", file=sys.stderr)
        return []

    # rev-list --parents --format interleaves: "<sha> <parents...>" then the
    # formatted record. Parse pairwise.
    records: list[dict] = []
    lines = out.stdout.split("\x1e")
    for chunk in lines:
        chunk = chunk.strip("\n")
        if not chunk.strip():
            continue
        head, _, tail = chunk.partition("\x1f")
        # head looks like: "commit <sha> <p1> <p2> ..."  (possibly preceded by \n)
        toks = head.replace("commit ", " ").split()
        if len(toks) < 3:
            continue
        sha, parents = toks[0], toks[1:]
        if len(parents) != 2:
            continue  # octopus merges are out of scope for three-way replay
        fields = tail.split("\x1f")
        ts = int(fields[1]) if len(fields) > 1 and fields[1].isdigit() else 0
        records.append({
            "repo": slug, "language": lang, "merge": sha,
            "p1": parents[0], "p2": parents[1], "committed_at": ts,
        })
    print(f"  {slug:<40} {len(records):>7} two-parent merges", file=sys.stderr)
    return records


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--jobs", type=int, default=6)
    args = ap.parse_args()

    rows = load_manifest()
    print(f"mining {len(rows)} repositories", file=sys.stderr)
    all_records: list[dict] = []
    with cf.ThreadPoolExecutor(max_workers=args.jobs) as ex:
        for recs in ex.map(lambda r: mine(*r), rows):
            all_records.extend(recs)

    # Deterministic order so the corpus hash is stable across runs.
    all_records.sort(key=lambda r: (r["repo"], r["merge"]))
    OUT.parent.mkdir(parents=True, exist_ok=True)
    with OUT.open("w") as fh:
        for r in all_records:
            fh.write(json.dumps(r, sort_keys=True) + "\n")

    langs = sorted({r["language"] for r in all_records})
    repos = sorted({r["repo"] for r in all_records})
    print(f"\ntotal: {len(all_records)} merges | {len(repos)} repos | "
          f"{len(langs)} languages: {', '.join(langs)}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
