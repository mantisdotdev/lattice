#!/usr/bin/env python3
"""
G1.2 — Byte fidelity (HARD).

Target: round-trip (save -> checkout) byte-identical on the adversarial corpus
(CRLF, BOM, invalid UTF-8, 0-byte, >= 1 GB file, symlinks, unicode/case-colliding
names): 0 mismatches.

Frozen before any `ltx` code exists. Until the binary is built this harness
reports `not-implemented`, which the runner renders as N/A-yet -- a gate whose
subject does not exist is not a gate that passes.

Two design points worth stating, because they are what make this harness hard to
satisfy accidentally:

  1. Expectations are PINNED. Every file's SHA-256 was recorded when the corpus
     was generated (corpus/manifests/g1-2-adversarial.json). The harness compares
     checkout output against those pinned digests, never against the source tree
     it happens to find on disk -- otherwise a bug that corrupts both save and
     the comparison would pass.

  2. Filename identity is compared as BYTES, not as decoded strings. The corpus
     deliberately contains an NFC/NFD filename pair that renders identically.
     Comparing decoded names would silently treat them as one file on macOS,
     which is precisely the failure this gate exists to catch.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CORPUS = REPO / "corpus" / "data" / "adversarial"
MANIFEST = REPO / "corpus" / "manifests" / "g1-2-adversarial.json"
LTX = REPO / "target" / "release" / "ltx"

TIMEOUT = 1800


def not_implemented(note: str) -> int:
    print(json.dumps({"gate": "G1.2", "status": "not-implemented", "note": note}))
    return 0


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def ltx(*args: str, cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run([str(LTX), *args], cwd=cwd, capture_output=True,
                          text=True, errors="replace", timeout=TIMEOUT)


def collect_entries(root: Path) -> dict[bytes, dict]:
    """Map raw-bytes relative path -> entry description.

    Paths are kept as bytes throughout. os.walk on bytes avoids the filesystem's
    own Unicode normalisation deciding what counts as a distinct file.
    """
    out: dict[bytes, dict] = {}
    rootb = os.fsencode(root)
    for dirpath, dirnames, filenames in os.walk(rootb):
        # Prune BEFORE recording. An earlier version pruned on the next
        # iteration, by which time `.lattice` had already been recorded as a
        # directory entry of the root -- producing a permanent false mismatch
        # ("present in source, absent after checkout") on every single run.
        dirnames[:] = [d for d in dirnames if d != b".lattice"]
        if b".lattice" in Path(os.fsdecode(dirpath)).parts:
            continue
        for name in filenames + dirnames:
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, rootb)
            try:
                st = os.lstat(full)
            except OSError:
                continue
            if os.path.islink(full):
                out[rel] = {"kind": "symlink", "target": os.readlink(full)}
            elif os.path.isdir(full):
                out[rel] = {"kind": "directory"}
            else:
                out[rel] = {"kind": "file", "size": st.st_size,
                            "mode": oct(st.st_mode & 0o777)}
    return out


def main() -> int:
    if not LTX.exists():
        return not_implemented("ltx binary not built yet (Stage G0 forbids product code)")
    if not CORPUS.exists() or not MANIFEST.exists():
        return not_implemented(
            "adversarial corpus not built "
            "(scripts/corpus/build_adversarial_corpus.py)")

    manifest = json.loads(MANIFEST.read_text())
    expected = {f["path"]: f for f in manifest["files"]}
    expected_special = {s["path"]: s for s in manifest["special_entries"]}

    # G1.2 names its mandated cases explicitly. Deriving the expectation set
    # from the manifest alone let a corpus builder omit one -- the >=1 GB file,
    # via --skip-huge -- and still produce a manifest that looked complete, so
    # the gate would report 0 mismatches while never measuring a mandated case.
    # The mandate lives here, in the harness, where the corpus cannot edit it.
    mandated = {
        "crlf": "eol/crlf.txt",
        "bom": "encoding/utf8-bom.txt",
        "invalid utf-8": "encoding/invalid-utf8.bin",
        "0-byte": "size/empty.txt",
        ">=1 GB file": "size/huge-1gb.bin",
    }
    absent = {name: path for name, path in mandated.items() if path not in expected}
    declared_skips = manifest.get("skipped_mandated_cases", [])
    if absent:
        print(json.dumps({
            "gate": "G1.2", "value": len(absent), "unit": "mismatches",
            "note": "corpus omits mandated cases named by G1.2: "
                    + ", ".join(f"{n} ({p})" for n, p in absent.items())
                    + (f" | builder declared skips: {declared_skips}"
                       if declared_skips else ""),
            "detail": {"missing_mandated_cases": absent,
                       "declared_skips": declared_skips}}))
        return 0
    if not any(s["kind"] == "symlink" for s in expected_special.values()):
        print(json.dumps({
            "gate": "G1.2", "value": 1, "unit": "mismatches",
            "note": "corpus contains no symlink entries; G1.2 names symlinks "
                    "explicitly, so symlink round-trip would go unmeasured"}))
        return 0

    work = Path(tempfile.mkdtemp(prefix="g1-2-"))
    mismatches: list[dict] = []
    checked = 0
    try:
        source = work / "source"
        shutil.copytree(CORPUS, source, symlinks=True)

        r = ltx("init", cwd=source)
        if r.returncode != 0:
            # A built binary that cannot initialise is a failure, not an absent
            # subject. Reporting not-implemented here would map to N/A-yet and
            # the gate would never evaluate its value.
            print(json.dumps({
                "gate": "G1.2", "value": len(expected) + len(expected_special),
                "unit": "mismatches",
                "note": f"ltx init failed: {r.stderr[:200]}"}))
            return 0
        r = ltx("save", "adversarial corpus", cwd=source)
        if r.returncode != 0:
            print(json.dumps({
                "gate": "G1.2", "value": len(expected) + len(expected_special),
                "unit": "mismatches",
                "note": f"ltx save failed: {r.stderr[:200]}"}))
            return 0

        # Check out into a fresh directory so nothing from the source tree can
        # be mistaken for a faithful round-trip.
        dest = work / "checkout"
        r = ltx("workspace", "new", str(dest), cwd=source)
        if r.returncode != 0:
            r = ltx("checkout", "--into", str(dest), cwd=source)
        if r.returncode != 0 or not dest.exists():
            print(json.dumps({
                "gate": "G1.2", "value": len(expected) + len(expected_special),
                "unit": "mismatches",
                "note": f"checkout failed: {r.stderr[:200]}"}))
            return 0

        # 1. Every pinned file must reappear with its pinned digest.
        for rel, info in sorted(expected.items()):
            checked += 1
            path = dest / rel
            if not path.exists() or path.is_dir():
                mismatches.append({"path": rel, "why": "absent after checkout"})
                continue
            actual = sha256_file(path)
            if actual != info["sha256"]:
                mismatches.append({
                    "path": rel, "why": "content differs",
                    "expected_sha256": info["sha256"], "actual_sha256": actual,
                    "expected_bytes": info["bytes"],
                    "actual_bytes": path.stat().st_size})
            if "mode" in info:
                # Normalise both sides: oct() yields '0o755', the manifest
                # records '0755'. Comparing them raw made this branch fire on
                # every correct round-trip, so the gate could never reach 0.
                actual_mode = f"{path.stat().st_mode & 0o777:04o}"
                if actual_mode != f"{int(str(info['mode']), 8):04o}":
                    mismatches.append({"path": rel, "why": "mode differs",
                                       "expected": f"{int(str(info['mode']), 8):04o}",
                                       "actual": actual_mode})

        # 2. Symlinks must round-trip as symlinks, targets intact, without being
        #    followed. Empty directories must survive.
        for rel, info in sorted(expected_special.items()):
            checked += 1
            path = dest / rel
            if info["kind"] == "symlink":
                if not path.is_symlink():
                    mismatches.append({"path": rel,
                                       "why": "symlink did not round-trip as a symlink"})
                elif os.readlink(path) != info["target"]:
                    mismatches.append({"path": rel, "why": "symlink target differs",
                                       "expected": info["target"],
                                       "actual": os.readlink(path)})
            elif info["kind"] == "directory":
                if not path.is_dir():
                    mismatches.append({"path": rel, "why": "directory absent"})

        # 3. Path-set equality, compared as raw bytes. Catches NFC/NFD collapse
        #    and case-folding, which a digest check alone would miss.
        src_entries = collect_entries(source)
        dst_entries = collect_entries(dest)
        only_src = set(src_entries) - set(dst_entries)
        only_dst = set(dst_entries) - set(src_entries)
        for rel in sorted(only_src):
            mismatches.append({"path": os.fsdecode(rel),
                               "why": "present in source, absent after checkout "
                                      "(byte-exact path comparison)"})
        for rel in sorted(only_dst):
            mismatches.append({"path": os.fsdecode(rel),
                               "why": "appeared after checkout, absent in source"})
        checked += len(src_entries)

        print(json.dumps({
            "gate": "G1.2",
            "value": len(mismatches),
            "unit": "mismatches",
            "note": (f"0 mismatches over {checked} checked entries"
                     if not mismatches else
                     f"{len(mismatches)} mismatches over {checked} checked entries"),
            "detail": {
                "checked_entries": checked,
                "pinned_files": len(expected),
                "special_entries": len(expected_special),
                "source_paths": len(src_entries),
                "checkout_paths": len(dst_entries),
                "mismatches": mismatches[:50],
                "mismatches_truncated": max(0, len(mismatches) - 50),
            },
            "evidence": [str(MANIFEST.relative_to(REPO))],
        }))
        return 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
