#!/usr/bin/env python3
"""Shared markdown structure checks used by several document gates.

Document gates are still gates: they must measure structure and substance
mechanically, not trust that a file with the right name says something.
"""
from __future__ import annotations
import re
from pathlib import Path

URL = re.compile(r"https?://[^\s)\]]+")
WORD = re.compile(r"[A-Za-z0-9][A-Za-z0-9'’/-]*")
CODE_FENCE = re.compile(r"```.*?```", re.S)


def sections(text: str) -> dict[str, str]:
    """Map heading text -> body under it, for headings of any level."""
    out: dict[str, str] = {}
    lines = text.splitlines()
    current, buf = None, []
    for line in lines:
        m = re.match(r"^(#{1,6})\s+(.*?)\s*$", line)
        if m:
            if current is not None:
                out[current] = "\n".join(buf)
            current, buf = m.group(2), []
        elif current is not None:
            buf.append(line)
    if current is not None:
        out[current] = "\n".join(buf)
    return out


def substantive_words(text: str) -> int:
    """Words outside code fences — prose only, so a file cannot pass on payload."""
    return len(WORD.findall(CODE_FENCE.sub(" ", text)))


def find_section(secs: dict[str, str], *needles: str) -> tuple[str, str] | None:
    """First section whose heading contains all needles (case-insensitive)."""
    for head, body in secs.items():
        low = head.lower()
        if all(n.lower() in low for n in needles):
            return head, body
    return None


def urls(text: str) -> list[str]:
    return URL.findall(text)


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")
