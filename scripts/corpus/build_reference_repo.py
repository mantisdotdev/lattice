#!/usr/bin/env python3
"""
Build the G0.5 composite reference repository from hash-pinned public bases.

§6: "select and hash-pin snapshots of ≥ 3 named public repos (one source-heavy,
one binary-heavy, one deep-history) and composite them by committed script into
a ~100k-file, ~2 GB-history reference repo."

Method: fetch each base's full history into one object database under its own
ref, then build a single composite commit whose tree places each base's tip
under its own prefix (`git read-tree --prefix`). The result is a genuine
repository: real commits, real content, real authorship, real history depth —
the working set is the union of the tips, and every base's complete history is
present and walkable.

An earlier version streamed each base through fast-export/fast-import to prefix
historical paths too. That was abandoned: exporting a commit range emits `from`
lines referencing parents outside the range, which no fresh repository can
resolve, and exporting full history for four bases costs hours for no
measurement benefit — nothing any gate measures depends on historical paths
being prefixed.
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

# Per the amended statistics contract.
BASES = [
    ("microsoft/TypeScript", "source", "src-typescript"),
    ("symfony/symfony", "source", "src-symfony"),
    ("opencv/opencv_extra", "binary", "bin-media"),
    ("git/git", "history", "deep-history"),
]


def git(*args, cwd=None, timeout=7200, check=False):
    r = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True,
                       errors="replace", timeout=timeout)
    if check and r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args[:3])} failed: {r.stderr[:300]}")
    return r


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true")
    args = ap.parse_args()

    if DEST.exists():
        if not args.force:
            print(f"{DEST} exists; pass --force to rebuild", file=sys.stderr)
            return 1
        subprocess.run(["rm", "-rf", str(DEST)])
    DEST.mkdir(parents=True)
    git("init", "--quiet", str(DEST), check=True)
    git("-C", str(DEST), "config", "user.name", "Lattice Corpus")
    git("-C", str(DEST), "config", "user.email", "corpus@lattice.invalid")
    git("-C", str(DEST), "config", "gc.auto", "0")

    pins, prefixes = {}, []
    for slug, role, prefix in BASES:
        src = REPOS / (slug.replace("/", "__") + ".git")
        if not (src / "HEAD").exists():
            print(f"SKIP {slug}: not cloned", file=sys.stderr)
            continue
        head = git("-C", str(src), "rev-parse", "HEAD").stdout.strip()
        commits = int(git("-C", str(src), "rev-list", "--count", "--all").stdout.strip() or 0)
        print(f"fetching {slug} ({role}) head={head[:12]} commits={commits:,} …",
              file=sys.stderr, flush=True)
        t0 = time.time()
        # Local transport: objects are copied or hardlinked, not re-transferred.
        r = git("-C", str(DEST), "fetch", "--quiet", "--no-tags",
                str(src), f"+refs/heads/*:refs/bases/{prefix}/*", check=False)
        if r.returncode != 0:
            print(f"  fetch failed: {r.stderr[:200]}", file=sys.stderr)
            continue
        git("-C", str(DEST), "update-ref", f"refs/bases/{prefix}/TIP", head)
        pins[slug] = {"role": role, "prefix": prefix, "head": head,
                      "commits": commits}
        prefixes.append((prefix, head))
        print(f"  done in {time.time()-t0:.0f}s", file=sys.stderr, flush=True)

    if len(prefixes) < 3:
        print("fewer than 3 bases available", file=sys.stderr)
        return 1

    print("composing working tree …", file=sys.stderr, flush=True)
    git("-C", str(DEST), "read-tree", "--empty", check=True)
    for prefix, head in prefixes:
        git("-C", str(DEST), "read-tree", f"--prefix={prefix}/", f"{head}^{{tree}}",
            check=True)
    tree = git("-C", str(DEST), "write-tree", check=True).stdout.strip()
    msg = ("Composite reference repository (G0.5)\n\n"
           + "\n".join(f"{p}: {s} @ {h}" for (p, h), (s, _) in
                       zip(prefixes, [(k, v) for k, v in pins.items()])))
    commit = git("-C", str(DEST), "commit-tree", tree, "-m", msg, check=True).stdout.strip()
    git("-C", str(DEST), "update-ref", "refs/heads/main", commit, check=True)
    git("-C", str(DEST), "symbolic-ref", "HEAD", "refs/heads/main")
    print("checking out …", file=sys.stderr, flush=True)
    git("-C", str(DEST), "checkout", "--quiet", "--force", "main", timeout=7200, check=True)

    depth = int(git("-C", str(DEST), "rev-list", "--all", "--count").stdout.strip() or 0)
    PINS.write_text(json.dumps({
        "built_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "composite_commit": commit, "composite_tree": tree,
        "history_depth": depth, "bases": pins,
    }, indent=2) + "\n")
    print(json.dumps({"composite_commit": commit, "history_depth": depth,
                      "bases": pins}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
