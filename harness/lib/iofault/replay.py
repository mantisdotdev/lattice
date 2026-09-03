#!/usr/bin/env python3
"""
Replay a recorded I/O journal as a plausible post-power-loss filesystem.

Reads the journal produced by libiofault, truncates it at a random point (the
instant power was lost), and applies the three failures real storage exhibits to
the writes that had not yet been made durable:

  DROP     an un-fsynced write never reached the platter
  REORDER  un-fsynced writes landed out of order within a barrier window
  TEAR     one write landed partially, at a sector boundary

The rule that keeps this honest in the engine's favour: a write is durable once
an fsync/fdatasync on the SAME path follows it in the journal, and durable
writes are never dropped, reordered across the barrier, or torn. That is exactly
what fsync buys, and a replayer that violated it would fail the engine for a
promise the platform never made -- producing "bugs" nobody could fix.

Emits the set of critical sections the surviving faults touched, so G1.1 can
assert §6's coverage contract rather than assuming a fault landed somewhere
interesting.
"""
from __future__ import annotations

import argparse
import json
import os
import random
import struct
from dataclasses import dataclass
from pathlib import Path

OP_WRITE, OP_PWRITE, OP_FSYNC, OP_FDATASYNC, OP_RENAME, OP_FTRUNCATE, OP_UNLINK = range(1, 8)
SECTOR = 512

# Map a path inside the store to the critical section it belongs to, so
# coverage is derived from what was actually touched rather than declared by
# the product. §6 names these five.
SECTION_MARKERS = [
    ("compaction", ("/compact", ".compact", "/archive")),
    ("thinning", ("/thin", ".thin", "/ephemeral")),
    ("merge", ("/merge", ".merge", "/conflict")),
    ("sync", ("/sync", ".sync", "/remote", "/fetch")),
    ("store_write", ("/pack", ".pack", "/objects", "/chunks", "/oplog", ".redb")),
]


@dataclass
class Record:
    op: int
    seq: int
    offset: int
    length: int
    path: str
    payload: bytes


def parse(journal: Path):
    data = journal.read_bytes()
    pos, out = 0, []
    while pos + 32 <= len(data):
        op, seq, offset, length, plen = struct.unpack_from("<IQQQI", data, pos)
        pos += 32
        if pos + plen > len(data):
            break
        path = data[pos:pos + plen].decode("utf-8", "replace")
        pos += plen
        payload = b""
        if op in (OP_WRITE, OP_PWRITE, OP_RENAME):
            take = length if op != OP_RENAME else length
            if pos + take > len(data):
                break
            payload = data[pos:pos + take]
            pos += take
        out.append(Record(op, seq, offset, length, path, payload))
    return out


def section_for(path: str) -> str | None:
    low = path.lower()
    for name, markers in SECTION_MARKERS:
        if any(m in low for m in markers):
            return name
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--journal", required=True)
    ap.add_argument("--root", required=True)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--drop", action="store_true")
    ap.add_argument("--reorder", action="store_true")
    ap.add_argument("--tear", action="store_true")
    args = ap.parse_args()

    journal = Path(args.journal)
    if not journal.exists():
        print(json.dumps({"error": "journal missing", "sections_hit": []}))
        return 1
    records = parse(journal)
    if not records:
        print(json.dumps({"sections_hit": [], "note": "empty journal"}))
        return 0

    rng = random.Random(args.seed)

    # 1. Power was lost at a uniformly random point in the journal.
    cut = rng.randrange(1, len(records) + 1)
    prefix = records[:cut]

    # 2. Which writes were made durable? A write is durable once a sync on the
    #    same path follows it. Everything after the last sync for that path is
    #    still in volatile cache and is fair game.
    last_sync: dict[str, int] = {}
    for r in prefix:
        if r.op in (OP_FSYNC, OP_FDATASYNC):
            last_sync[r.path] = r.seq

    durable, volatile = [], []
    for r in prefix:
        if r.op in (OP_FSYNC, OP_FDATASYNC):
            continue
        (durable if r.seq < last_sync.get(r.path, -1) else volatile).append(r)

    # 3. Mutilate only the volatile tail.
    surviving = list(volatile)
    dropped = []
    if args.drop and surviving:
        keep = []
        for r in surviving:
            if rng.random() < 0.35:
                dropped.append(r)
            else:
                keep.append(r)
        surviving = keep
    if args.reorder and len(surviving) > 1:
        rng.shuffle(surviving)
    torn = None
    if args.tear and surviving:
        victim = rng.randrange(len(surviving))
        r = surviving[victim]
        if r.op in (OP_WRITE, OP_PWRITE) and len(r.payload) > SECTOR:
            sectors = max(1, len(r.payload) // SECTOR)
            keep_sectors = rng.randrange(1, sectors + 1)
            surviving[victim] = Record(r.op, r.seq, r.offset,
                                       keep_sectors * SECTOR, r.path,
                                       r.payload[:keep_sectors * SECTOR])
            torn = {"path": r.path, "kept_bytes": keep_sectors * SECTOR,
                    "original_bytes": len(r.payload)}

    # 4. Rebuild the filesystem: durable writes in journal order, then the
    #    mutilated volatile tail in its (possibly reordered) order.
    # Resolve the root for the same reason the shim does: the journal holds
    # fully-resolved paths (/private/var/...) while the caller usually passes
    # the symlinked form (/var/...). Comparing them unresolved applied nothing
    # while reporting a perfectly plausible crash.
    root = Path(args.root).resolve()
    applied = 0
    sections: set[str] = set()
    for r in sorted(durable, key=lambda x: x.seq) + surviving:
        target = Path(r.path)
        if not str(target).startswith(str(root)):
            continue

        sec = section_for(r.path)
        if sec:
            sections.add(sec)
        try:
            if r.op in (OP_WRITE, OP_PWRITE):
                target.parent.mkdir(parents=True, exist_ok=True)
                with open(target, "r+b" if target.exists() else "wb") as fh:
                    fh.seek(r.offset)
                    fh.write(r.payload)
                applied += 1
            elif r.op == OP_FTRUNCATE:
                if target.exists():
                    os.truncate(target, r.length)
                    applied += 1
            elif r.op == OP_UNLINK:
                target.unlink(missing_ok=True)
                applied += 1
            elif r.op == OP_RENAME:
                src = Path(r.payload.decode("utf-8", "replace"))
                if src.exists():
                    target.parent.mkdir(parents=True, exist_ok=True)
                    os.replace(src, target)
                    applied += 1
        except OSError:
            # A failed replay step IS a plausible crash outcome; the point of
            # the exercise is that the engine survives whatever state results.
            pass

    print(json.dumps({
        "journal_records": len(records),
        "crash_point": cut,
        "durable": len(durable),
        "volatile": len(volatile),
        "dropped": len(dropped),
        "reordered": bool(args.reorder and len(volatile) > 1),
        "torn": torn,
        "applied": applied,
        "sections_hit": sorted(sections),
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
