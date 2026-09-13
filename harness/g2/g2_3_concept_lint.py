#!/usr/bin/env python3
"""
G2.3 — Concept lint (HARD).

§4.2 caps the user-facing surface at seven nouns and the README promises "no
index, no HEAD, no detached anything." This harness makes that promise
mechanical. Measured value = vocabulary violations across every normal-path
`--help` screen the shipped binary prints: a word of Git or graph vocabulary
(banned outright), or any word that is neither one of the seven concepts nor
on the closed list of vetted plain English below. Target 0.

The rule is closed-world on purpose. A lint that only checks a ban list can
never catch an eighth noun being coined ("changeset", "snapshot"); a surface
whose every word must be vetted can. The cost is that ANY new help text word
— however innocent — fails the gate until it is added here, and this file is
inside the freeze (§0.3), so adding it is a deliberate, recorded act:
`gauntlet freeze --refreeze G2.3` plus an ADR note, never a quiet edit.

What is linted: the help text of the root command and of every subcommand
reachable from it, except the `internals` subtree. `internals` is "plumbing,
never required on a normal path" — its own screens may say chunk, pack and
Merkle, because the person who typed `ltx internals` asked for the machinery.
Its one-line row in the root help IS linted, because the root screen is the
first thing every user reads. Command names and flag names are linted for
free: they appear in the help text they introduce.

Vocabulary rulings that are easy to mis-read as violations, decided here and
frozen (the naming record is docs/adr/adr-22-concept-vocabulary.md):

  - "repository", "file", "path", "directory" — physical containers, not
    working-model concepts. Vetted plain English.
  - "history" — the collective of checkpoints, the way "weather" is the
    collective of days. Vetted.
  - "conflict" — §4.2 (revised): a state a change or checkpoint HAS, never an
    object. The word is vetted; a `conflict` noun-phrase command would still
    fail because its help would need unvetted words to describe itself.
  - "address" — how a checkpoint is named from outside (ADR-8), an attribute
    like a size. Vetted.
  - "working-tree" — Git's name for the first concept. Banned; the surface
    says "working state".

Coverage contract (§6): the walk must reach at least MIN_COMMANDS normal-path
commands, every `--help` must exit 0, and every normal-path command the binary
itself publishes via `internals command-surface --json` must have been walked.
A help screen this harness cannot read is a surface it cannot vouch for, so it
fails coverage rather than passing silently.

Written after the product surface it measures (the reverse of §0.3's order,
same as G2.4, and said here rather than hidden). It was therefore shown to
FAIL before being frozen: against a shim binary whose help advertises
`commit` and `stash` it reports both words, and against one that coins a
"snapshot" noun it reports the coinage (`--ltx` exists for exactly that check
and for nothing else).
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LTX = REPO / "target" / "release" / "ltx"
GATE = "G2.3"
TIMEOUT_S = 60
MIN_COMMANDS = 15

# The seven §4.2 concepts, in every spelling the surface uses.
CONCEPTS = frozenset({
    "working", "state", "working-state",
    "change", "changes",
    "checkpoint", "checkpoints", "checkpointed",
    "line", "lines",
    "lens", "lenses",
    "workspace", "workspaces",
    "remote", "remotes",
})

# Git and graph vocabulary. One occurrence on a normal path is one violation.
BANNED = frozenset({
    # Git's own surface
    "commit", "commits", "committed", "committing",
    "branch", "branches", "branching",
    "head", "heads", "index", "indexes",
    "stage", "staged", "staging", "stash", "stashed",
    "rebase", "rebased", "rebasing", "reflog",
    "submodule", "submodules", "cherry-pick", "cherry-picked",
    "worktree", "worktrees", "working-tree", "detached",
    # Graph and storage machinery
    "dag", "graph", "graphs", "node", "nodes", "edge", "edges",
    "vertex", "vertices", "ancestor", "ancestors",
    "descendant", "descendants", "parent", "parents",
    "merkle", "hash", "hashes", "hashed", "hashing",
    "sha", "sha1", "sha256", "oid", "oids",
    "blob", "blobs", "object", "objects", "tree", "trees", "ref", "refs",
    # Nouns §4.2 struck or folded into the seven
    "changeset", "changesets", "snapshot", "snapshots", "snapshotted",
})

# Every other word the surface may use. Closed: a word absent from all three
# sets is a violation, which is how a new noun gets caught.
ALLOWED = frozenset({
    "a", "accident", "add", "address", "against", "agent", "already", "an",
    "and", "another", "anything", "are", "arguments", "as", "assign", "assigned",
    "assigns", "authorship", "be", "becomes", "begin", "bring", "by", "call",
    "cannot", "check", "checkout", "collect", "command", "commands",
    "complete", "confirm-destroy", "conflict", "conflicts", "consume",
    "content", "contents", "continue", "control", "covering", "create",
    "creates", "current", "default", "defaults", "destroy", "differs",
    "directory", "does", "dry-run", "each", "emit", "empty", "entries", "every",
    "everything", "everywhere", "exchange", "exist", "fetch", "file", "for",
    "forensic", "form", "from", "go", "h", "happened", "has", "help", "here",
    "hidden", "hide", "history", "holds", "human", "id", "implicitly",
    "including", "init", "instead", "intact", "internals", "into",
    "irreversibility", "is", "it", "its", "json", "lattice", "limit", "list",
    "log", "look", "looks", "ltx", "may", "merge", "message", "missing",
    "mistyped", "most", "move", "must", "n", "name", "never", "new", "none",
    "normal", "nothing", "of", "on", "one", "only", "onto", "open",
    "options", "or", "other", "over", "own", "partial", "path", "paths",
    "plain", "plumbing", "preserving", "previous", "print", "prose", "put",
    "read", "received", "recent", "redact", "references", "report",
    "reports", "repository", "required", "return", "s", "save", "saved",
    "see", "sent", "show", "shows", "so", "split", "stable", "start", "starts",
    "status", "switch", "sync", "take", "than", "that", "the", "them", "there", "thin",
    "this", "through", "to", "top-level", "under", "undo", "undone",
    "unqualified", "until", "usage", "use", "v", "verified", "verify",
    "version", "view", "what", "where", "which", "whole", "whose", "with",
    "without", "work", "would", "write",
})

WORD = re.compile(r"[A-Za-z][A-Za-z-]*")


def run_help(path: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run([str(LTX), *path, "--help"], capture_output=True,
                          text=True, errors="replace", timeout=TIMEOUT_S,
                          check=False)


def subcommands(help_text: str) -> list[str]:
    """The names clap lists under "Commands:", in order."""
    names, in_block = [], False
    for raw in help_text.splitlines():
        if raw.strip() == "Commands:":
            in_block = True
            continue
        if in_block:
            if not raw.startswith("  ") or not raw.strip():
                break
            names.append(raw.split()[0])
    return names


def walk(path: list[str], screens: dict[str, str], failures: list[str]) -> None:
    proc = run_help(path)
    name = " ".join(path) or "(root)"
    if proc.returncode != 0:
        failures.append(name)
        return
    screens[name] = proc.stdout
    for sub in subcommands(proc.stdout):
        if sub == "internals":
            continue
        walk(path + [sub], screens, failures)


def judge(screens: dict[str, str]) -> list[dict]:
    """One row per distinct (screen, word) violation."""
    rows = []
    for name in sorted(screens):
        seen: set[str] = set()
        for token in WORD.findall(screens[name]):
            word = token.lower().rstrip("-")
            if not word or word in seen:
                continue
            seen.add(word)
            if word in BANNED:
                rows.append({"screen": name, "word": word, "kind": "banned-jargon"})
            elif word not in ALLOWED and word not in CONCEPTS:
                rows.append({"screen": name, "word": word, "kind": "unvetted-word"})
    return rows


def published_normal_paths() -> set[str]:
    proc = subprocess.run([str(LTX), "internals", "command-surface", "--json"],
                          capture_output=True, text=True, errors="replace",
                          timeout=TIMEOUT_S, check=False)
    if proc.returncode != 0:
        raise RuntimeError("the binary publishes no command surface")
    names = {c["name"] for c in json.loads(proc.stdout)["commands"]}
    return {n for n in names if n.split()[0] != "internals"}


def main() -> int:
    global LTX
    ap = argparse.ArgumentParser()
    ap.add_argument("--ltx", type=Path, default=LTX,
                    help="binary under test; exists so this harness can be shown to fail")
    args = ap.parse_args()
    LTX = args.ltx.resolve()
    if not LTX.exists():
        print(json.dumps({"gate": GATE, "status": "not-implemented",
                          "note": "ltx binary not built (target/release/ltx)"}))
        return 0

    screens: dict[str, str] = {}
    help_failures: list[str] = []
    walk([], screens, help_failures)
    rows = judge(screens)

    try:
        published = published_normal_paths()
    except (RuntimeError, json.JSONDecodeError, KeyError) as exc:
        print(json.dumps({"gate": GATE, "status": "not-implemented", "note": str(exc)}))
        return 0
    unwalked = sorted(published - set(screens))

    coverage_ok = (not help_failures and not unwalked
                   and len(screens) >= MIN_COMMANDS)
    coverage_note = "; ".join(
        ([f"--help failed for: {', '.join(help_failures)}"] if help_failures else [])
        + ([f"published but never walked: {', '.join(unwalked)}"] if unwalked else [])
        + ([f"only {len(screens)} screens walked, {MIN_COMMANDS} required"]
           if len(screens) < MIN_COMMANDS else []))

    print(json.dumps({
        "gate": GATE,
        "value": len(rows),
        "unit": "violations",
        "note": (f"{len(rows)} violation(s) across {len(screens)} help screens; "
                 f"vocabulary: {len(CONCEPTS)} concept forms, "
                 f"{len(ALLOWED)} vetted words, {len(BANNED)} banned"),
        "detail": {"violations": rows, "screens_walked": sorted(screens)},
        "coverage": {"ok": coverage_ok, "note": coverage_note},
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
