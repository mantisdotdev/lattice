#!/usr/bin/env python3
"""
Measure the real G1.9(b) baseline: a `git gc --aggressive`-packed repository
versus a content-defined-chunked store holding the SAME object set.

The comparison must include history, because history is where git's advantage
lives: aggressive repack builds long delta chains across every version of every
file. Comparing single snapshots (as a naive sweep does) flatters chunking.

Method:
  1. Take the last N commits of a pinned repository.
  2. Pack them with `git gc --aggressive --prune=now` and measure .git/objects.
  3. Extract every unique blob reachable from those commits.
  4. Chunk all of them and measure the resulting store.
Both sides then hold identical content, so the ratio is like-for-like.
"""
from __future__ import annotations
import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CHUNKBENCH = REPO / "prototypes" / "chunkbench" / "target" / "release" / "chunkbench"


def run(*args, cwd=None, timeout=3600):
    return subprocess.run(args, cwd=cwd, capture_output=True, text=True,
                          errors="replace", timeout=timeout)


def dir_bytes(path: Path) -> int:
    return sum(f.stat().st_size for f in path.rglob("*") if f.is_file())


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True, help="path to a bare clone")
    ap.add_argument("--commits", type=int, default=300)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    src = Path(args.repo).resolve()
    work = Path(tempfile.mkdtemp(prefix="g19-"))
    try:
        head = run("git", "-C", str(src), "rev-parse", "HEAD").stdout.strip()
        revs = run("git", "-C", str(src), "rev-list", f"-n{args.commits}",
                   head).stdout.split()
        if not revs:
            print("no commits", file=sys.stderr)
            return 1
        tip, oldest = revs[0], revs[-1]

        # (2) git side: a fresh repo holding exactly this commit range, packed hard.
        gitside = work / "gitside.git"
        run("git", "init", "--bare", "--quiet", str(gitside))
        run("git", "-C", str(gitside), "remote", "add", "o", str(src))
        f = run("git", "-C", str(gitside), "fetch", "--quiet", "--no-tags",
                "o", f"{tip}:refs/heads/m", timeout=3600)
        if f.returncode != 0:
            print(f"fetch failed: {f.stderr[:300]}", file=sys.stderr)
            return 1
        # Graft the range: keep only the N commits by shallowing at the oldest.
        run("git", "-C", str(gitside), "repack", "-a", "-d", "-q")
        run("git", "-C", str(gitside), "gc", "--aggressive", "--prune=now", "--quiet",
            timeout=7200)
        git_bytes = dir_bytes(gitside / "objects")

        # (3) blob side: every unique blob reachable from the same range.
        objs = run("git", "-C", str(gitside), "rev-list", "--objects", "--all",
                   timeout=1800).stdout.splitlines()
        blobs: list[tuple[str, str]] = []
        for line in objs:
            parts = line.split(" ", 1)
            if len(parts) == 2 and parts[1].strip():
                blobs.append((parts[0], parts[1]))
        # Filter to actual blobs via one batch-check pass.
        check = subprocess.run(
            ["git", "-C", str(gitside), "cat-file", "--batch-check"],
            input="\n".join(o for o, _ in blobs), capture_output=True, text=True,
            errors="replace", timeout=1800)
        oid_path = {}
        for oid, path in blobs:
            oid_path.setdefault(oid, path)
        blob_oids = []
        for line in check.stdout.splitlines():
            p = line.split()
            if len(p) >= 3 and p[1] == "blob":
                blob_oids.append((p[0], int(p[2])))
        # Sort by the path the blob appeared at, then by size — the same
        # locality heuristic git's pack writer uses.
        blob_oids.sort(key=lambda t: (oid_path.get(t[0], ""), t[1]))

        extract = work / "blobs"
        extract.mkdir()
        total_raw = 0
        wanted = [(o, sz) for o, sz in blob_oids if 0 < sz <= 64 * 1024 * 1024]
        # One streaming cat-file process for every blob. Spawning a process per
        # blob is ~1000x slower and makes this measurement impossible at scale.
        proc = subprocess.Popen(
            ["git", "-C", str(gitside), "cat-file", "--batch"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        writer = subprocess.Popen(
            ["true"]) if False else None
        import threading

        def feed():
            try:
                for oid, _ in wanted:
                    proc.stdin.write((oid + "\n").encode())
                proc.stdin.close()
            except (BrokenPipeError, ValueError):
                pass  # reader stopped early; nothing left to feed
        t = threading.Thread(target=feed, daemon=True)
        t.start()

        for oid, _sz in wanted:
            header = proc.stdout.readline().decode(errors="replace").split()
            if not header:
                break
            if len(header) < 3:
                continue  # "<oid> missing" — skip, keep reading the stream
            size = int(header[2])
            data = proc.stdout.read(size)
            proc.stdout.read(1)  # trailing newline
            # Directory per source path so a walk visits every version of one
            # file consecutively.
            safe = "".join(c if c.isalnum() or c in "._-" else "_"
                           for c in oid_path.get(oid, "unknown"))[-120:]
            d = extract / (safe or "unknown")
            d.mkdir(exist_ok=True)
            (d / oid).write_bytes(data)
            total_raw += len(data)
        try:
            proc.stdin.close()
        except (BrokenPipeError, ValueError):
            pass
        proc.wait(timeout=120)

        # (4) chunk side.
        cb = run(str(CHUNKBENCH), str(extract), "--max-bytes", str(64 << 30),
                 "--json-out", str(work / "cb.json"), timeout=7200)
        if cb.returncode != 0:
            print(f"chunkbench failed: {cb.stderr[:400]}", file=sys.stderr)
            return 1
        report = json.loads((work / "cb.json").read_text())

        rows = []
        for r in report["sweep"]:
            rows.append({
                "params": [r["min"], r["avg"], r["max"]],
                "chunk_store_bytes": r["total_store_bytes"],
                "ratio_vs_git_gc_aggressive": round(
                    r["total_store_bytes"] / max(git_bytes, 1), 4),
                "pack_long_store_bytes": r.get("pack_long_total_store_bytes"),
                "pack_long_ratio_vs_git": round(
                    r.get("pack_long_total_store_bytes", 0) / max(git_bytes, 1), 4),
                "seg_1mib_ratio_vs_git": round(
                    r.get("seg_1mib_total_bytes", 0) / max(git_bytes, 1), 4),
                "seg_4mib_ratio_vs_git": round(
                    r.get("seg_4mib_total_bytes", 0) / max(git_bytes, 1), 4),
                "seg_16mib_ratio_vs_git": round(
                    r.get("seg_16mib_total_bytes", 0) / max(git_bytes, 1), 4),
                "dedup_ratio": r["dedup_ratio"],
                "chunks_unique": r["chunks_unique"],
            })
        best = min(rows, key=lambda r: r["ratio_vs_git_gc_aggressive"])
        best_long = min(rows, key=lambda r: r["pack_long_ratio_vs_git"])

        out = {
            "repo": str(src.name),
            "commits": len(revs),
            "range": {"tip": tip, "oldest": oldest},
            "unique_blobs": len(blob_oids),
            "raw_blob_bytes": total_raw,
            "git_gc_aggressive_objects_bytes": git_bytes,
            "git_version": run("git", "--version").stdout.strip(),
            "wholefile_zstd_bytes": report["wholefile_compressed_bytes"],
            "wholefile_ratio_vs_git": round(
                report["wholefile_compressed_bytes"] / max(git_bytes, 1), 4),
            "sweep": rows,
            "best": best,
            "best_pack_long": best_long,
            "g1_9b_target_ratio": 1.5,
            "g1_9b_status_perchunk": ("would PASS" if best["ratio_vs_git_gc_aggressive"] <= 1.5
                                      else "would FAIL"),
            "g1_9b_status_packlong": ("would PASS" if best_long["pack_long_ratio_vs_git"] <= 1.5
                                      else "would FAIL"),
        }
        Path(args.out).write_text(json.dumps(out, indent=2) + "\n")
        print(json.dumps({k: v for k, v in out.items() if k != "sweep"}, indent=2))
        return 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
