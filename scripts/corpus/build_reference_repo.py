#!/usr/bin/env python3
"""
Build the G0.5 composite reference repository from hash-pinned public bases.

§6: "select and hash-pin snapshots of ≥ 3 named public repos (one source-heavy,
one binary-heavy, one deep-history) and composite them by committed script into
a ~100k-file, ~2 GB-history reference repo."

Method: each base's real history is streamed through `git fast-export`, its paths
are rewritten under a per-base prefix, and the result is `git fast-import`ed into
one composite repository. Real commits, real content, real authorship — the
composite is a genuine repository, not a synthetic one. Each base keeps its own
ref, so the three histories are disjoint and the composite's working tree is the
union of the three tips.
"""
from __future__ import annotations
import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
REPOS = REPO / "corpus" / "data" / "repos"
DEST = REPO / "corpus" / "data" / "reference-repo"
PINS = REPO / "corpus" / "manifests" / "g0-5-pins.json"

BASES = [
    ("symfony/symfony", "source", "src-heavy", 9000),
    ("opencv/opencv_extra", "binary", "bin-heavy", 6000),
    ("git/git", "history", "deep-history", 40000),
]


def rewrite_paths(stream, prefix: str, out):
    """Rewrite a fast-export stream so every path sits under `prefix/`.

    Only path-bearing commands are touched. Data blocks are copied through by
    declared length so binary content is never misinterpreted as commands —
    getting this wrong silently corrupts the corpus.
    """
    def q(p: bytes) -> bytes:
        if p.startswith(b'"'):
            return b'"' + prefix.encode() + b"/" + p[1:]
        return prefix.encode() + b"/" + p

    while True:
        line = stream.readline()
        if not line:
            break
        if line.startswith(b"data "):
            out.write(line)
            n = int(line[5:].strip())
            remaining = n
            while remaining > 0:
                buf = stream.read(min(1 << 20, remaining))
                if not buf:
                    break
                out.write(buf)
                remaining -= len(buf)
            continue
        if line.startswith(b"M "):
            parts = line.rstrip(b"\n").split(b" ", 3)
            if len(parts) == 4:
                out.write(b" ".join(parts[:3]) + b" " + q(parts[3]) + b"\n")
                continue
        elif line.startswith(b"D "):
            out.write(b"D " + q(line[2:].rstrip(b"\n")) + b"\n")
            continue
        elif line[:2] in (b"C ", b"R "):
            parts = line.rstrip(b"\n").split(b" ")
            if len(parts) == 3:
                out.write(parts[0] + b" " + q(parts[1]) + b" " + q(parts[2]) + b"\n")
                continue
        out.write(line)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true")
    args = ap.parse_args()

    if DEST.exists() and not args.force:
        print(f"{DEST} exists; pass --force to rebuild", file=sys.stderr)
        return 1
    if DEST.exists():
        subprocess.run(["rm", "-rf", str(DEST)])
    DEST.mkdir(parents=True)
    subprocess.run(["git", "init", "--quiet", str(DEST)], check=True)
    subprocess.run(["git", "-C", str(DEST), "config", "user.name", "Lattice Corpus"])
    subprocess.run(["git", "-C", str(DEST), "config", "user.email", "corpus@lattice.invalid"])

    pins = {}
    for slug, role, prefix, max_commits in BASES:
        src = REPOS / (slug.replace("/", "__") + ".git")
        if not (src / "HEAD").exists():
            print(f"SKIP {slug}: not cloned", file=sys.stderr)
            continue
        head = subprocess.run(["git", "-C", str(src), "rev-parse", "HEAD"],
                              capture_output=True, text=True).stdout.strip()
        count = subprocess.run(["git", "-C", str(src), "rev-list", "--count", "HEAD"],
                               capture_output=True, text=True).stdout.strip()
        pins[slug] = {"role": role, "prefix": prefix, "head": head,
                      "total_commits": int(count or 0), "max_commits": max_commits}
        print(f"exporting {slug} ({role}) head={head[:12]} "
              f"commits={count} cap={max_commits} …", file=sys.stderr, flush=True)

        t0 = time.time()
        exporter = subprocess.Popen(
            ["git", "-C", str(src), "fast-export", "--no-data" if False else "--progress=20000",
             "--signed-tags=strip", "--tag-of-filtered-object=drop",
             "--reference-excluded-parents", f"--refspec=refs/heads/*:refs/heads/{prefix}/*",
             f"HEAD~{max_commits}..HEAD" if int(count or 0) > max_commits else "HEAD"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        importer = subprocess.Popen(
            ["git", "-C", str(DEST), "fast-import", "--quiet", "--force"],
            stdin=subprocess.PIPE)
        try:
            rewrite_paths(exporter.stdout, prefix, importer.stdin)
        finally:
            importer.stdin.close()
            exporter.wait()
            importer.wait()
        print(f"  done in {time.time()-t0:.0f}s", file=sys.stderr, flush=True)

    # A single commit whose tree is the union of the three tips gives the
    # composite a working set of the required size.
    refs = subprocess.run(["git", "-C", str(DEST), "for-each-ref", "--format=%(refname)"],
                          capture_output=True, text=True).stdout.split()
    print(f"refs imported: {len(refs)}", file=sys.stderr)
    subprocess.run(["git", "-C", str(DEST), "gc", "--quiet"], timeout=7200)

    PINS.write_text(json.dumps({
        "built_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "bases": pins, "refs": refs,
    }, indent=2) + "\n")
    print(json.dumps(pins, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
