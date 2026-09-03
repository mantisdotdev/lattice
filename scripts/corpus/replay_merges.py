#!/usr/bin/env python3
"""
Replay mined merges and measure the G0.3 line-based baselines.

Implements corpus/manifests/g0-3-oracle.md exactly. The oracle's hash is
recorded in the output so a result can never be read against a different oracle
than the one it was produced under.
"""
from __future__ import annotations
import argparse
import collections
import concurrent.futures as cf
import hashlib
import json
import math
import random
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
REPOS = REPO / "corpus" / "data" / "repos"
MERGES = REPO / "corpus" / "data" / "merges.jsonl"
ORACLE = REPO / "corpus" / "manifests" / "g0-3-oracle.md"
OUT = REPO / "corpus" / "data" / "merge-baselines.json"
DETAIL = REPO / "corpus" / "data" / "merge-replay.jsonl"

SEED = 20260903          # pinned in the oracle document
GIT_TIMEOUT = 120

IMPORT_LINE = re.compile(
    r"^\s*(?:import\s|from\s+\S+\s+import\s|use\s+\S+;|#include\s|require\s|"
    r"require_once\s|using\s)", re.I)
TRAILING_COMMA = re.compile(r",(\s*[)\]}])")
BLANK_RUN = re.compile(r"\n{4,}")


def git(repo: Path, *args: str, binary: bool = False):
    return subprocess.run(["git", "-C", str(repo), *args],
                          capture_output=True, timeout=GIT_TIMEOUT,
                          text=not binary, errors=None if binary else "replace")


# ---------------------------------------------------------------- normalization
def normalize(data: bytes) -> bytes:
    """Oracle rules N1-N5. Binary blobs pass through untouched."""
    if b"\x00" in data[:8000]:
        return data
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        return data

    text = text.replace("\r\n", "\n").replace("\r", "\n")              # N1
    lines = [ln.rstrip() for ln in text.split("\n")]                   # N2

    out, i = [], 0                                                     # N5
    while i < len(lines):
        if IMPORT_LINE.match(lines[i]):
            j = i
            while j < len(lines) and IMPORT_LINE.match(lines[j]):
                j += 1
            if j - i >= 2:
                out.extend(sorted(lines[i:j]))
                i = j
                continue
        out.append(lines[i])
        i += 1

    text = "\n".join(out)
    text = TRAILING_COMMA.sub(r"\1", text)                             # N4
    text = BLANK_RUN.sub("\n\n\n", text).rstrip("\n") + "\n"           # N3
    return text.encode("utf-8")


def blob(repo: Path, tree: str, path: str) -> bytes | None:
    r = git(repo, "show", f"{tree}:{path}", binary=True)
    return r.stdout if r.returncode == 0 else None


# ------------------------------------------------------------------- classify
def classify(repo: Path, rec: dict) -> dict:
    sha, p1, p2 = rec["merge"], rec["p1"], rec["p2"]
    res = {"repo": rec["repo"], "language": rec["language"], "merge": sha}

    base = git(repo, "merge-base", p1, p2)
    if base.returncode != 0 or not base.stdout.strip():
        res["bucket"] = "ERROR"
        res["why"] = "no merge base (X3)"
        return res
    merge_base = base.stdout.split()[0]

    recorded = git(repo, "rev-parse", f"{sha}^{{tree}}")
    if recorded.returncode != 0:
        res["bucket"] = "ERROR"
        res["why"] = "recorded tree unreadable"
        return res
    recorded_tree = recorded.stdout.strip()

    mt = git(repo, "merge-tree", "--write-tree", p1, p2)
    if mt.returncode > 1:
        res["bucket"] = "ERROR"
        res["why"] = f"merge-tree exit {mt.returncode}"
        return res
    replayed_tree = mt.stdout.splitlines()[0].strip() if mt.stdout.strip() else ""
    if not replayed_tree:
        res["bucket"] = "ERROR"
        res["why"] = "merge-tree produced no tree"
        return res

    if mt.returncode == 1:
        conflicted = [ln for ln in mt.stdout.splitlines()[1:] if ln.strip()]
        res["bucket"] = "REPLAY_CONFLICT"
        res["conflicted_entries"] = len(conflicted)
        res["replayed_tree"] = replayed_tree
        res["recorded_tree"] = recorded_tree
        return res

    if replayed_tree == recorded_tree:
        res["bucket"] = "CLEAN_MATCH"
        return res

    # Clean replay, different tree. Normalize, then apply exclusions.
    d = git(repo, "diff", "--name-only", "-z", replayed_tree, recorded_tree)
    if d.returncode != 0:
        res["bucket"] = "ERROR"
        res["why"] = "diff failed"
        return res
    paths = [p for p in d.stdout.split("\0") if p]
    res["diff_paths"] = len(paths)

    only_normalization, evil_paths = True, []
    for path in paths[:80]:                     # bounded: never unbounded work
        b_rep = blob(repo, replayed_tree, path)
        b_rec = blob(repo, recorded_tree, path)
        if b_rep is None or b_rec is None:
            only_normalization = False          # add/delete divergence is real
            continue
        if normalize(b_rep) == normalize(b_rec):
            continue
        only_normalization = False
        b_p1 = blob(repo, f"{p1}^{{tree}}", path)
        b_p2 = blob(repo, f"{p2}^{{tree}}", path)
        b_base = blob(repo, f"{merge_base}^{{tree}}", path)
        n_rec = normalize(b_rec)
        sides = {normalize(b) for b in (b_rep, b_p1, b_p2, b_base) if b is not None}
        if n_rec not in sides:
            evil_paths.append(path)             # X1: authored during the merge

    if only_normalization:
        res["bucket"] = "CLEAN_DIVERGE_NORMALIZED"
    elif evil_paths:
        res["bucket"] = "EXCLUDED_EVIL"
        res["evil_paths"] = evil_paths[:5]
    else:
        res["bucket"] = "CLEAN_DIVERGE"
    res["replayed_tree"] = replayed_tree
    res["recorded_tree"] = recorded_tree
    return res


def do_repo(slug: str, records: list[dict]) -> list[dict]:
    path = REPOS / (slug.replace("/", "__") + ".git")
    if not (path / "HEAD").exists():
        return []
    out = []
    for rec in records:
        try:
            out.append(classify(path, rec))
        except subprocess.TimeoutExpired:
            out.append({"repo": slug, "merge": rec["merge"], "bucket": "ERROR",
                        "why": "timeout"})
    print(f"  {slug:<40} replayed {len(out)}", file=sys.stderr, flush=True)
    return out


def wilson(k: int, n: int) -> tuple[float, float]:
    """95% Wilson interval, as percentages. Reported so a sampled rate is never
    mistaken for an exact one."""
    if n == 0:
        return (0.0, 0.0)
    z, p = 1.959964, k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (100 * max(0.0, c - h), 100 * min(1.0, c + h))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--sample", type=int, default=15000)
    ap.add_argument("--per-repo-cap", type=int, default=2000)
    ap.add_argument("--jobs", type=int, default=7)
    args = ap.parse_args()

    records = [json.loads(l) for l in MERGES.read_text().splitlines() if l.strip()]
    by_repo: dict[str, list[dict]] = collections.defaultdict(list)
    for r in records:
        by_repo[r["repo"]].append(r)

    rng = random.Random(SEED)
    sampled: dict[str, list[dict]] = {}
    for slug, rows in sorted(by_repo.items()):
        rows = sorted(rows, key=lambda r: r["merge"])
        take = min(len(rows), args.per_repo_cap)
        sampled[slug] = rng.sample(rows, take)
    total = sum(len(v) for v in sampled.values())
    if total > args.sample:                       # trim proportionally, deterministically
        scale = args.sample / total
        for slug in sampled:
            keep = max(1, int(len(sampled[slug]) * scale))
            sampled[slug] = sampled[slug][:keep]

    print(f"mined={len(records)} repos={len(by_repo)} "
          f"replaying={sum(len(v) for v in sampled.values())}", file=sys.stderr)

    results: list[dict] = []
    with cf.ThreadPoolExecutor(max_workers=args.jobs) as ex:
        futs = [ex.submit(do_repo, slug, rows) for slug, rows in sorted(sampled.items())]
        for f in cf.as_completed(futs):
            results.extend(f.result())

    with DETAIL.open("w") as fh:
        for r in sorted(results, key=lambda r: (r["repo"], r["merge"])):
            fh.write(json.dumps(r, sort_keys=True) + "\n")

    buckets = collections.Counter(r["bucket"] for r in results)
    n_ok = sum(v for k, v in buckets.items() if k != "ERROR")
    clean = (buckets["CLEAN_MATCH"] + buckets["CLEAN_DIVERGE"]
             + buckets["CLEAN_DIVERGE_NORMALIZED"])
    conflict = buckets["REPLAY_CONFLICT"]
    diverge = buckets["CLEAN_DIVERGE"]

    by_lang = collections.defaultdict(collections.Counter)
    for r in results:
        by_lang[r.get("language", "?")][r["bucket"]] += 1

    payload = {
        "oracle_sha256": hashlib.sha256(ORACLE.read_bytes()).hexdigest(),
        "seed": SEED,
        "mined_total": len(records),
        "mined_repos": sorted(by_repo),
        "mined_languages": sorted({r["language"] for r in records}),
        "replayed": len(results),
        "buckets": dict(buckets),
        "baselines": {
            "naive_line_mismerge_rate_pct": 0.0,
            "naive_note": "zero by construction: for a clean auto-merge the "
                          "recorded tree IS the line-merge result "
                          "(docs/DISAGREEMENTS.md Challenge 2)",
            "divergence_baseline_pct": round(100.0 * diverge / clean, 4) if clean else None,
            "divergence_baseline_ci95": [round(x, 4) for x in wilson(diverge, clean)],
            "divergence_n": clean,
            "line_conflict_rate_pct": round(100.0 * conflict / n_ok, 4) if n_ok else None,
            "line_conflict_rate_ci95": [round(x, 4) for x in wilson(conflict, n_ok)],
            "line_conflict_n": n_ok,
        },
        "by_language": {k: dict(v) for k, v in sorted(by_lang.items())},
        "by_repo": {slug: dict(collections.Counter(
            r["bucket"] for r in results if r["repo"] == slug))
            for slug in sorted(sampled)},
    }
    OUT.write_text(json.dumps(payload, indent=2) + "\n")
    print(json.dumps(payload["baselines"], indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
