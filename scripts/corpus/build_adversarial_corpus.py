#!/usr/bin/env python3
"""
Build the G1.2 adversarial byte-fidelity corpus.

G1.2 is HARD: save->checkout must be byte-identical, 0 mismatches, on a corpus of
"CRLF, BOM, invalid UTF-8, 0-byte, >= 1 GB file, symlinks, unicode/case-colliding
names". This script builds exactly that, plus every neighbouring case that has
historically broken a version control system.

The corpus is generated rather than mined because these cases are, by their
nature, the ones real repositories avoid -- a repo containing a file whose name
differs from another only by Unicode normalisation does not survive contact with
macOS, which is precisely why it must be tested.

Every file's expected SHA-256 is recorded in a manifest, so the gate compares
against a pinned expectation rather than against whatever the filesystem
happened to return.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
OUT = REPO / "corpus" / "data" / "adversarial"
MANIFEST = REPO / "corpus" / "manifests" / "g1-2-adversarial.json"

CR = b"\x0d"
LF = b"\x0a"
CRLF = CR + LF
NUL = b"\x00"


def write(rel: str, data: bytes, records: list, note: str) -> None:
    path = OUT / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    records.append({
        "path": rel,
        "bytes": len(data),
        "note": note,
        "sha256": hashlib.sha256(data).hexdigest(),
    })


def build_line_endings(r: list) -> None:
    write("eol/crlf.txt", b"line one" + CRLF + b"line two" + CRLF, r,
          "CRLF throughout; must survive without translation (byte fidelity)")
    write("eol/lf.txt", b"line one" + LF + b"line two" + LF, r,
          "LF baseline")
    write("eol/cr-only.txt", b"classic mac" + CR + b"line endings" + CR, r,
          "bare CR line endings")
    write("eol/mixed.txt", b"lf" + LF + b"crlf" + CRLF + b"cr" + CR + b"end" + LF, r,
          "all three in one file -- the case naive EOL normalisation corrupts")
    write("eol/no-trailing-newline.txt", b"no newline at end", r,
          "missing final newline must not be added")
    write("eol/crlf-no-final.txt", b"a" + CRLF + b"b", r,
          "CRLF with no final terminator")


def build_encodings(r: list) -> None:
    write("encoding/utf8-bom.txt", b"\xef\xbb\xbfwith a BOM" + LF, r,
          "UTF-8 BOM must be preserved, not stripped")
    write("encoding/utf16le-bom.txt",
          b"\xff\xfe" + "utf-16 text\n".encode("utf-16-le"), r, "UTF-16 LE BOM")
    write("encoding/utf16be-bom.txt",
          b"\xfe\xff" + "utf-16 text\n".encode("utf-16-be"), r, "UTF-16 BE BOM")
    write("encoding/invalid-utf8.bin",
          bytes([0x41, 0xC3, 0x28, 0xA0, 0xA1, 0xE2, 0x28, 0xA1]), r,
          "invalid UTF-8 sequences; stored as bytes, never repaired")
    write("encoding/latin1.txt", "café naïve\n".encode("latin-1"), r,
          "Latin-1 high bytes")
    write("encoding/nul-bytes.bin", b"before" + NUL + b"after" + NUL + NUL + b"end", r,
          "embedded NUL -- the usual binary/text sniffing boundary")
    write("encoding/lone-surrogate.bin", b"\xed\xa0\x80", r,
          "CESU-8 lone surrogate: valid bytes, invalid Unicode")
    write("encoding/overlong.bin", b"\xc0\xaf", r,
          "overlong UTF-8 encoding of '/' -- a classic path-traversal payload")


def build_sizes(r: list) -> None:
    write("size/empty.txt", b"", r, "0-byte file")
    write("size/one-byte.txt", b"x", r, "1 byte")
    write("size/exactly-4096.bin", bytes(range(256)) * 16, r, "exactly one page")
    write("size/exactly-8192.bin", bytes(range(256)) * 32, r, "exactly two pages")
    # Straddle plausible chunk boundaries so an off-by-one in chunking shows up.
    for n in (1023, 1024, 1025, 2047, 2048, 2049, 8191, 8192, 8193, 65535, 65536, 65537):
        write(f"size/boundary-{n}.bin", b"\xa5" * n, r,
              f"{n} bytes -- straddles a chunk-size boundary")
    # Highly compressible and wholly incompressible, same size.
    write("size/zeros-1mib.bin", bytes(1 << 20), r,
          "1 MiB of zeros -- pathological for delta and dedup accounting")
    write("size/random-1mib.bin",
          bytes((i * 2654435761 >> 13) & 0xFF for i in range(1 << 20)), r,
          "1 MiB incompressible-ish, deterministic")


def build_names(r: list) -> None:
    write("names/UPPER.txt", b"upper\n", r, "case-colliding pair, member 1")
    write("names/upper.txt", b"lower\n", r,
          "case-colliding pair, member 2 -- distinct on Linux, colliding on "
          "macOS/Windows (Windows is tier-1)")
    write("names/é-decomposed.txt", b"NFD\n", r,
          "e + U+0301 combining acute (NFD)")
    write("names/é-precomposed.txt", b"NFC\n", r,
          "U+00E9 precomposed (NFC) -- same glyph as the NFD file, different "
          "bytes; macOS normalises filenames and Linux does not")
    write("names/中文文件.txt", "CJK\n".encode(), r, "CJK filename")
    write("names/emoji-\U0001f680.txt", b"astral plane\n", r,
          "astral-plane codepoint in a filename (surrogate pairs on Windows)")
    write("names/spaces and 'quotes' and \"double\".txt", b"quoting\n", r,
          "spaces and both quote kinds -- shell and pathspec parsers")
    write("names/trailing.space .txt", b"trailing space\n", r,
          "trailing space before the extension (Windows strips it)")
    write("names/--looks-like-a-flag.txt", b"flag-like\n", r,
          "leading dashes: argument-parsing hazard")
    write("names/percent%20encoded.txt", b"percent\n", r,
          "literal percent-encoding in a real name")
    write("names/dot.in.the.middle.tar.gz", b"multi-extension\n", r,
          "multiple extensions")
    write("names/.hidden", b"dotfile\n", r, "leading dot")
    deep = "/".join(f"d{i:02d}" for i in range(24))
    write(f"names/{deep}/deep.txt", b"deep path\n", r,
          "24 levels deep -- approaches Windows MAX_PATH")
    write("names/" + ("n" * 200) + ".txt", b"long component\n", r,
          "200-character path component")


def build_special(r: list, special: list) -> None:
    (OUT / "special").mkdir(parents=True, exist_ok=True)
    links = [
        ("relative-symlink", "../size/one-byte.txt",
         "relative symlink; target must not be followed on save"),
        ("dangling-symlink", "/nonexistent/dangling",
         "dangling symlink must round-trip as a symlink, not an error"),
        ("loop-a", "loop-b", "symlink cycle: traversal must terminate"),
        ("loop-b", "loop-a", "symlink cycle, other half"),
    ]
    for name, target, note in links:
        try:
            os.symlink(target, OUT / "special" / name)
            special.append({"path": f"special/{name}", "kind": "symlink",
                            "target": target, "note": note})
        except OSError as exc:
            # Recorded, not swallowed. Silently dropping the entry produced a
            # manifest that looked complete while G1.2 stopped testing symlink
            # round-trip entirely -- the realistic trigger being Windows without
            # developer mode, and Windows is tier-1.
            print(f"symlink {name} unavailable: {exc}", file=sys.stderr)
            special.append({"path": f"special/{name}", "kind": "symlink",
                            "target": target, "note": note,
                            "creation_failed": str(exc)})

    payload = b"#!/bin/sh\nexit 0\n"
    exec_path = OUT / "special" / "executable.sh"
    exec_path.write_bytes(payload)
    os.chmod(exec_path, 0o755)
    r.append({"path": "special/executable.sh", "bytes": len(payload), "mode": "0755",
              "note": "executable bit must round-trip",
              "sha256": hashlib.sha256(payload).hexdigest()})

    (OUT / "special" / "empty-dir").mkdir(exist_ok=True)
    special.append({"path": "special/empty-dir", "kind": "directory",
                    "note": "empty directory -- git cannot represent this, so the "
                            "lossy-edge ledger (G3.4) must record it explicitly"})


def build_huge(r: list, target_bytes: int) -> None:
    big = OUT / "size" / "huge-1gb.bin"
    big.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256()
    block = bytes((i * 37 + 11) % 256 for i in range(1 << 20))  # 1 MiB, deterministic
    written = 0
    with big.open("wb") as fh:
        while written < target_bytes:
            n = min(len(block), target_bytes - written)
            fh.write(block[:n])
            digest.update(block[:n])
            written += n
    r.append({"path": "size/huge-1gb.bin", "bytes": written,
              "sha256": digest.hexdigest(),
              "note": "the >=1 GB file G1.2 names explicitly; exercises chunk trees"})


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--huge-file-bytes", type=int, default=1_100_000_000)
    ap.add_argument("--skip-huge", action="store_true")
    ap.add_argument("--force", action="store_true")
    args = ap.parse_args()

    if OUT.exists():
        if not args.force:
            print(f"{OUT} exists; pass --force", file=sys.stderr)
            return 1
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)

    records: list = []
    special: list = []
    build_line_endings(records)
    build_encodings(records)
    build_sizes(records)
    build_names(records)
    build_special(records, special)
    skipped_mandated: list[str] = []
    if args.skip_huge:
        # Declared in the manifest so a consumer can reject an incomplete
        # corpus instead of trusting a manifest that appears whole.
        skipped_mandated.append("size/huge-1gb.bin")
    else:
        build_huge(records, args.huge_file_bytes)

    manifest = {
        "note": "Expected SHA-256 per file. G1.2 compares checkout output against "
                "these pinned values, not against whatever the filesystem replies.",
        "files": sorted(records, key=lambda x: x["path"]),
        "special_entries": sorted(special, key=lambda x: x["path"]),
        "total_files": len(records),
        "total_bytes": sum(x["bytes"] for x in records),
        "skipped_mandated_cases": skipped_mandated,
    }
    MANIFEST.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"files": len(records), "special_entries": len(special),
                      "total_bytes": manifest["total_bytes"]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
