#!/usr/bin/env python3
"""
Build the G0.5 large-binary corpus: ≥5 GiB including a simulated edit history.

The history is simulated, and this script says so loudly, because no public
corpus provides ≥5 GB of *binary edit history* under a permissive licence. What
is NOT simulated is the seed material: real images, video, fonts and compiled
artifacts harvested from the pinned clones and the local toolchain.

Four mutation kinds, chosen because they are how binary assets actually change,
and because each stresses a different property of content-defined chunking:

  1 localized_patch   a bounded span is rewritten (an image region re-encoded).
                      CDC should store only the affected chunks. This is the
                      case CDC exists to win.
  2 prefix_insert     bytes are inserted near the front (a header/metadata block
                      grows). This is the case that defeats FIXED-size chunking
                      entirely, since every subsequent block shifts.
  3 append            bytes are added at the end (log-like or streaming assets).
  4 reencode          the whole file is re-derived; almost nothing is shared.
                      Included so the corpus cannot flatter CDC: a store must
                      not claim savings it does not have.

Selection is seeded and recorded, so the corpus is byte-reproducible.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import random
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
OUT = REPO / "corpus" / "data" / "binary-corpus"
MANIFEST = REPO / "corpus" / "manifests" / "g0-5-binary-corpus.json"
SEED = 20260903

BINARY_EXT = {".png", ".jpg", ".jpeg", ".gif", ".bmp", ".tiff", ".webp", ".ico",
              ".pdf", ".mp4", ".avi", ".mov", ".mkv", ".webm", ".mp3", ".wav",
              ".ttf", ".otf", ".woff", ".woff2", ".zip", ".gz", ".bz2", ".xz",
              ".jar", ".so", ".dylib", ".a", ".rlib", ".o", ".wasm", ".bin",
              ".dat", ".npy", ".npz", ".class", ".exe", ".dll", ".yml.gz"}
MIN_SEED_BYTES = 4096


def harvest(roots: list[Path], budget: int, rng: random.Random) -> list[Path]:
    found: list[Path] = []
    total = 0
    for root in roots:
        if not root.exists():
            continue
        for p in root.rglob("*"):
            if total >= budget:
                break
            try:
                if not p.is_file() or p.is_symlink():
                    continue
                size = p.stat().st_size
            except OSError:
                continue
            if size < MIN_SEED_BYTES or size > 256 * 1024 * 1024:
                continue
            if p.suffix.lower() not in BINARY_EXT:
                continue
            found.append(p)
            total += size
    rng.shuffle(found)
    return found


def mutate(data: bytes, kind: str, rng: random.Random) -> bytes:
    n = len(data)
    if kind == "localized_patch":
        span = min(max(n // 50, 1024), 1 << 20)
        off = rng.randrange(0, max(1, n - span))
        patch = bytes(rng.getrandbits(8) for _ in range(min(span, 65536)))
        patch = (patch * (span // len(patch) + 1))[:span]
        return data[:off] + patch + data[off + span:]
    if kind == "prefix_insert":
        ins = bytes(rng.getrandbits(8) for _ in range(rng.randrange(64, 4096)))
        off = rng.randrange(0, min(n, 8192) or 1)
        return data[:off] + ins + data[off:]
    if kind == "append":
        add = min(max(n // 20, 4096), 1 << 20)
        tail = bytes(rng.getrandbits(8) for _ in range(min(add, 65536)))
        return data + (tail * (add // len(tail) + 1))[:add]
    if kind == "reencode":
        # Re-derive the whole file deterministically from its own content: no
        # long runs survive, so a store must report ~no sharing.
        h = hashlib.blake2b(data, digest_size=64).digest()
        out = bytearray()
        while len(out) < n:
            h = hashlib.blake2b(h, digest_size=64).digest()
            out += h
        return bytes(out[:n])
    raise ValueError(kind)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--target-bytes", type=int, default=6 * (1 << 30))
    ap.add_argument("--versions", type=int, default=9)
    ap.add_argument("--seed-budget", type=int, default=700 * (1 << 20))
    ap.add_argument("--force", action="store_true")
    args = ap.parse_args()

    if OUT.exists():
        if not args.force:
            print(f"{OUT} exists; pass --force", file=sys.stderr)
            return 1
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)

    rng = random.Random(SEED)
    roots = [REPO / "corpus" / "data" / "repos",
             REPO / "corpus" / "data" / "trees",
             Path.home() / ".cargo" / "registry" / "cache",
             REPO / "prototypes"]
    seeds = harvest(roots, args.seed_budget, rng)
    if not seeds:
        print(json.dumps({"error": "no binary seed material found"}))
        return 1

    kinds = ["localized_patch", "prefix_insert", "append", "reencode"]
    kind_counts = {k: 0 for k in kinds}
    seed_bytes = written = 0
    files = 0
    records = []

    for si, src in enumerate(seeds):
        if written >= args.target_bytes:
            break
        try:
            data = src.read_bytes()
        except OSError:
            continue
        seed_bytes += len(data)
        d = OUT / f"{si % 128:03d}"
        d.mkdir(exist_ok=True)
        stem = f"{si:06d}{src.suffix.lower()}"
        cur = data
        chain = []
        for v in range(args.versions):
            if v > 0:
                # Weighted toward localized edits, which dominate real asset churn.
                kind = rng.choices(kinds, weights=[0.5, 0.2, 0.2, 0.1])[0]
                cur = mutate(cur, kind, rng)
                kind_counts[kind] += 1
                chain.append(kind)
            path = d / f"v{v:02d}-{stem}"
            path.write_bytes(cur)
            written += len(cur)
            files += 1
            if written >= args.target_bytes:
                break
        records.append({"seed": str(src.relative_to(REPO)) if str(src).startswith(str(REPO))
                        else src.name, "bytes": len(data), "chain": chain})

    manifest = {
        "seed": SEED,
        "note": "Seed material is real binary content; the edit history is "
                "simulated. See corpus/manifests/g0-5-statistics-contract.md.",
        "seed_files": len(records),
        "seed_bytes": seed_bytes,
        "versions_per_seed": args.versions,
        "total_files": files,
        "total_bytes_with_history": written,
        "mutation_kinds": kind_counts,
        "seeds": records[:200],
    }
    MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({k: v for k, v in manifest.items() if k != "seeds"}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
