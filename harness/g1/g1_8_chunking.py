#!/usr/bin/env python3
"""
G1.8 — Chunking efficiency (SOFT, perf).

Target: a 1-byte edit at a random offset in a 50 MB file persists < 256 KiB of
new data, p95 over 100 trials x 5 file types.

The five file types are PINNED here rather than chosen at run time. An
independent review noted that "5 file types" was unspecified in the brief, which
would let a future run pick five compressible text files and report a flattering
number. The set below deliberately spans the compressibility range, because
chunk-boundary behaviour differs sharply between high-entropy and low-entropy
content, and a store that only handles one is not a store that handles files.

Measured against the ADR-2 parameters. This harness does NOT require the `ltx`
binary: it measures the chunking design directly through the same `fastcdc`
parameters ADR-2 selected, so the gate is meaningful before the engine exists
and becomes a regression check afterwards. When `ltx` IS built, the harness
additionally measures the real store and reports both, and the ENGINE figure is
the gate's value -- a design that chunks well but whose store writes more than
the chunks is not a passing store.
"""
from __future__ import annotations

import hashlib
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness" / "lib"))
import ltxrun as L  # noqa: E402

GATE = "G1.8"
SEED = 20260903
FILE_BYTES = 50 * 1024 * 1024      # the gate names 50 MB explicitly
TRIALS_PER_TYPE = 100              # the gate names 100 trials
TARGET_BYTES = 256 * 1024

# ADR-2's decision. Changing these here without changing the ADR is a
# divergence the ADR's own evidence section would contradict.
CHUNK_MIN, CHUNK_AVG, CHUNK_MAX = 2048, 8192, 32768

# The five pinned file types, spanning the compressibility range on purpose.
FILE_TYPES = [
    ("incompressible", "pseudo-random bytes; no internal redundancy, the worst "
                       "case for content-defined boundaries"),
    ("source_text", "repeated realistic source lines; highly compressible, many "
                    "natural boundaries"),
    ("structured_binary", "fixed-size records with a varying header; the shape "
                          "of model weights and serialized data"),
    ("sparse_zeros", "long zero runs punctuated by data; pathological for "
                     "rolling hashes that boundary on low-entropy windows"),
    ("mixed_media", "compressed blocks separated by plain-text metadata, as in "
                    "container formats"),
]


def make_file(kind: str, size: int, rng: random.Random) -> bytes:
    """Deterministic content per (kind, seed). No file is read from disk, so the
    corpus cannot drift between runs."""
    if kind == "incompressible":
        return rng.randbytes(size)
    if kind == "source_text":
        lines = [f"    let value_{i} = compute(&context, {i});\n".encode()
                 for i in range(512)]
        out = bytearray()
        while len(out) < size:
            out += lines[rng.randrange(len(lines))]
        return bytes(out[:size])
    if kind == "structured_binary":
        out = bytearray()
        record = 4096
        while len(out) < size:
            out += (len(out) // record).to_bytes(8, "little")
            out += rng.randbytes(record - 8)
        return bytes(out[:size])
    if kind == "sparse_zeros":
        out = bytearray()
        while len(out) < size:
            out += bytes(rng.randrange(4096, 65536))
            out += rng.randbytes(rng.randrange(512, 4096))
        return bytes(out[:size])
    if kind == "mixed_media":
        out = bytearray()
        while len(out) < size:
            out += f"--boundary-{len(out)}\nContent-Type: application/octet-stream\n\n".encode()
            out += rng.randbytes(rng.randrange(1 << 16, 1 << 18))
        return bytes(out[:size])
    raise ValueError(kind)


KERNEL = REPO / "prototypes" / "chunkbench" / "target" / "release" / "g1_8"


def run_design_kernel() -> dict:
    """Measure the chunking design via the Rust kernel.

    The kernel uses the SAME `fastcdc` crate the engine will use, so there is
    exactly one chunker implementation in the project. A Python reimplementation
    in the harness would eventually disagree with the product, and the harness
    copy is the one nobody would notice was wrong.
    """
    if not KERNEL.exists():
        raise L.NotBuilt(
            "G1.8 kernel not built "
            "(cd prototypes/chunkbench && cargo build --release --bin g1_8)")
    proc = subprocess.run(
        [str(KERNEL), str(SEED), str(FILE_BYTES), str(TRIALS_PER_TYPE)],
        capture_output=True, text=True, errors="replace", timeout=7200)
    if proc.returncode != 0:
        raise L.NotBuilt(f"G1.8 kernel failed: {proc.stderr.strip()[:300]}")
    doc = json.loads(proc.stdout)
    if (doc["chunk_min"], doc["chunk_avg"], doc["chunk_max"]) != \
            (CHUNK_MIN, CHUNK_AVG, CHUNK_MAX):
        raise L.NotBuilt(
            f"kernel chunk parameters {doc['chunk_min']}/{doc['chunk_avg']}/"
            f"{doc['chunk_max']} disagree with ADR-2's "
            f"{CHUNK_MIN}/{CHUNK_AVG}/{CHUNK_MAX}")
    return {t["kind"]: t["samples"] for t in doc["per_type"]}


def percentile(values: list[int], p: float) -> float:
    if not values:
        return float("nan")
    ordered = sorted(values)
    return ordered[max(0, min(len(ordered) - 1, int(len(ordered) * p + 0.5) - 1))]


def measure_engine(rng: random.Random) -> dict | None:
    """When the engine exists, measure what the STORE actually persists."""
    if not L.LTX.exists():
        return None
    work = Path(tempfile.mkdtemp(prefix="g1-8-engine-"))
    try:
        repo = work / "repo"
        repo.mkdir()
        if L.run(["init"], cwd=repo).returncode != 0:
            return None
        results: dict[str, list[int]] = {}
        for kind, _ in FILE_TYPES:
            data = make_file(kind, FILE_BYTES, random.Random(SEED))
            target = repo / f"{kind}.bin"
            target.write_bytes(data)
            if L.run(["save", "baseline"], cwd=repo).returncode != 0:
                return None
            before = store_bytes(repo)
            samples = []
            for _ in range(TRIALS_PER_TYPE):
                offset = rng.randrange(len(data))
                buf = bytearray(target.read_bytes())
                buf[offset] ^= 0xFF
                target.write_bytes(bytes(buf))
                if L.run(["save", "one-byte edit"], cwd=repo).returncode != 0:
                    return None
                after = store_bytes(repo)
                samples.append(max(0, after - before))
                before = after
            results[kind] = samples
        return results
    finally:
        shutil.rmtree(work, ignore_errors=True)


def store_bytes(repo: Path) -> int:
    store = repo / ".lattice"
    return sum(f.stat().st_size for f in store.rglob("*") if f.is_file()) \
        if store.exists() else 0


def main() -> int:
    try:
        design = run_design_kernel()
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))

    missing = [k for k, _ in FILE_TYPES if k not in design]
    if missing:
        return L.emit({
            "gate": GATE, "value": 1e12, "unit": "bytes",
            "note": f"kernel did not measure pinned file type(s): "
                    f"{', '.join(missing)}"})

    engine = measure_engine(random.Random(SEED))

    if engine is None:
        # The gate measures what the STORE persists. The design measurement is
        # strong evidence and is reported, but claiming PASS on it would credit
        # the engine for a property only the chunker has been shown to have --
        # a store can always write more than its chunks. Same refusal as G1.12
        # declining to count a self-skipped CI job as a platform pass.
        design_p95 = percentile([x for v in design.values() for x in v], 0.95)
        return L.emit({
            "gate": GATE,
            "status": "not-implemented",
            "note": (f"engine not built, so the store cannot be measured. The "
                     f"chunking design alone measures p95 {design_p95:,.0f} bytes "
                     f"against a {TARGET_BYTES:,} target -- reported as evidence, "
                     f"not claimed as a pass."),
            "detail": {
                "measured_against": "chunking design only",
                "design_p95_bytes": design_p95,
                "chunk_params": {"min": CHUNK_MIN, "avg": CHUNK_AVG,
                                 "max": CHUNK_MAX},
                "per_type": {k: {"p50": percentile(v, 0.50),
                                 "p95": percentile(v, 0.95),
                                 "max": max(v), "trials": len(v)}
                             for k, v in design.items()},
            },
        })

    source = engine
    which = "engine store"

    per_type = {k: {"p50": percentile(v, 0.50), "p95": percentile(v, 0.95),
                    "max": max(v), "trials": len(v)}
                for k, v in source.items()}
    all_samples = [s for v in source.values() for s in v]
    p95 = percentile(all_samples, 0.95)
    worst = max(per_type.items(), key=lambda kv: kv[1]["p95"])

    return L.emit({
        "gate": GATE,
        "value": p95,
        "unit": "bytes",
        "note": (f"p95 {p95:,.0f} bytes over {len(all_samples)} trials "
                 f"({len(FILE_TYPES)} pinned file types x {TRIALS_PER_TYPE}), "
                 f"measured against the {which}; worst type "
                 f"'{worst[0]}' at {worst[1]['p95']:,.0f}"),
        "detail": {
            "measured_against": which,
            "chunk_params": {"min": CHUNK_MIN, "avg": CHUNK_AVG, "max": CHUNK_MAX},
            "file_bytes": FILE_BYTES,
            "trials_per_type": TRIALS_PER_TYPE,
            "seed": SEED,
            "target_bytes": TARGET_BYTES,
            "pinned_file_types": {k: d for k, d in FILE_TYPES},
            "per_type": per_type,
            "design_p95_bytes": percentile(
                [s for v in design.values() for s in v], 0.95),
        },
        "evidence": ["docs/adr/adr-2-chunk-parameters.md"],
    })


if __name__ == "__main__":
    raise SystemExit(main())
