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
    """Map heading text -> the body beneath it, INCLUDING nested subsections.

    A section runs until the next heading of the same or higher level, which is
    how a reader understands document structure. An earlier version ended a
    section at the next heading of ANY level, so a `## Question` whose content
    lives under `### Sub-points` measured as empty -- it reported 0 words for
    sections containing thousands. That measured heading adjacency, not content,
    which is a different thing from the criterion the consuming gates state.
    See docs/adr/adr-12-mdcheck-section-nesting.md.
    """
    lines = text.splitlines()
    heads: list[tuple[int, str, int]] = []  # (level, title, line index)
    for i, line in enumerate(lines):
        m = re.match(r"^(#{1,6})\s+(.*?)\s*$", line)
        if m:
            heads.append((len(m.group(1)), m.group(2), i))

    out: dict[str, str] = {}
    for idx, (level, title, start) in enumerate(heads):
        end = len(lines)
        for later_level, _, later_start in heads[idx + 1:]:
            if later_level <= level:
                end = later_start
                break
        out[title] = "\n".join(lines[start + 1:end])
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
