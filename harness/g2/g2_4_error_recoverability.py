#!/usr/bin/env python3
"""
G2.4 — Error recoverability (HARD).

§4.3: "every error names a way back." Measured value = the number of error
paths whose JSON document is deficient — missing a recovery action, a causal
category, or a §4.2 concept — out of every error path this harness can provoke
through the shipped CLI. Target 0.

Each provocation is a fresh repository put into one specific state and one
command that must fail there. The failure's `--json` document is then held to
five requirements, and an error that misses any one of them is deficient:

  1. `error` says what happened;
  2. `category` is one of the causal categories the contract defines;
  3. `concept` is one of the SEVEN §4.2 nouns. `none` is a deficiency, not a
     category of its own: an error that cannot say which concept it is about
     leaves the user without orientation, which is the state this gate exists
     to eliminate;
  4. `recovery` is present and non-empty;
  5. every command the recovery names (`ltx <word>`) exists in the binary's
     own published surface — advice that leads to "unrecognized subcommand"
     is a dead end dressed as a way back.

Coverage contract (§6): a provocation that does NOT produce an error — exit
zero, or output that is not one JSON object with `ok: false` — is not skipped
and not counted as clean. It fails coverage, because a harness whose
provocations silently succeed is measuring nothing there. Every causal category
must be provoked at least once, so the count cannot pass by exercising only the
easy ones.

The Busy provocation holds the repository lock from this process for longer
than the engine is willing to wait, so it costs about a minute. It is kept
because Busy is the one category whose recovery must NOT say "inspect the
repository" — nothing is wrong — and that is precisely the kind of advice this
gate checks.

Written after the product surface it measures, which is the reverse of §0.3's
order, and said here rather than hidden. It was therefore shown to FAIL before
being frozen: against a wrapper binary that blanks `recovery`, and one that
reports `concept: none`, it reports every provoked error deficient (`--ltx`
exists for exactly that check and for nothing else).
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LTX = REPO / "target" / "release" / "ltx"
GATE = "G2.4"
TIMEOUT_S = 300

CATEGORIES = {"not-a-repository", "not-found", "corrupt", "io", "invalid", "busy"}
# §4.2's seven, as the CLI spells them.
CONCEPTS = {"working-state", "change", "checkpoint", "line", "lens", "workspace", "remote"}
# The engine waits this long for the lock before reporting Busy; the hold must
# outlast it. Read from the source would be nicer, but a number quoted from a
# constant is a number that drifts — so it is generous instead.
BUSY_HOLD_S = 70
ZERO_ADDRESS = "0" * 64


def run(argv: list[str], cwd: Path, timeout: int = TIMEOUT_S) -> subprocess.CompletedProcess:
    return subprocess.run([str(LTX), *argv, "--json"], cwd=cwd, capture_output=True,
                          text=True, errors="replace", timeout=timeout, check=False)


def fresh_repo(work: Path, name: str) -> Path:
    repo = work / name
    repo.mkdir()
    for step in (["init"], ["save", "base"]):
        if step[0] == "save":
            (repo / "a.txt").write_text("a\n")
        r = run(step, repo)
        if r.returncode != 0:
            raise RuntimeError(f"setup `ltx {' '.join(step)}` failed: {r.stdout[:200]}")
    return repo


# ---------------------------------------------------------------- provocations
# Each returns (cwd, argv). Setup that fails raises, which surfaces as a
# harness error rather than a silently missing provocation.

def not_a_repository(work: Path):
    empty = work / "empty"
    empty.mkdir()
    return empty, ["status"]


def no_such_line(work: Path):
    return fresh_repo(work, "no-such-line"), ["switch", "nowhere"]


def invalid_line_name(work: Path):
    return fresh_repo(work, "invalid-line"), ["start", ""]


def no_such_change(work: Path):
    return fresh_repo(work, "no-such-change"), ["save", "--change", "zzzz", "msg"]


def change_holds_nothing(work: Path):
    repo = work / "empty-change"
    repo.mkdir()
    for step in (["init"], ["assign", "."]):
        r = run(step, repo)
        if r.returncode != 0:
            raise RuntimeError(f"setup `ltx {' '.join(step)}` failed: {r.stdout[:200]}")
    listed = json.loads(run(["change", "list"], repo).stdout)["changes"]
    if not listed:
        raise RuntimeError("setup: a bare assign in an empty repository made no change")
    return repo, ["save", "--change", listed[0]["short"], "msg"]


def no_such_lens(work: Path):
    return fresh_repo(work, "no-such-lens"), ["lens", "use", "nope"]


def no_remote(work: Path):
    return fresh_repo(work, "no-remote"), ["sync"]


def checkout_into_repository(work: Path):
    return fresh_repo(work, "checkout-inside"), ["checkout", "--into", "."]


def checkout_unknown_checkpoint(work: Path):
    repo = fresh_repo(work, "checkout-unknown")
    return repo, ["checkout", "--checkpoint", ZERO_ADDRESS, "--into", str(work / "out-unknown")]


def checkout_bad_address(work: Path):
    repo = fresh_repo(work, "checkout-bad")
    return repo, ["checkout", "--checkpoint", "zzz", "--into", str(work / "out-bad")]


def workspace_over_non_empty(work: Path):
    repo = fresh_repo(work, "workspace-nonempty")
    return repo, ["workspace", "new", str(repo)]


def merge_no_such_line(work: Path):
    return fresh_repo(work, "merge-no-line"), ["merge", "nowhere"]


def merge_diverged(work: Path):
    repo = fresh_repo(work, "diverged")
    steps = [
        (["start", "feat"], None),
        (["save", "on feat"], ("f.txt", "f\n")),
        (["switch", "main"], None),
        (["save", "on main"], ("m.txt", "m\n")),
    ]
    for argv, write in steps:
        if write:
            (repo / write[0]).write_text(write[1])
        r = run(argv, repo)
        if r.returncode != 0:
            raise RuntimeError(f"setup `ltx {' '.join(argv)}` failed: {r.stdout[:200]}")
    return repo, ["merge", "feat"]


def corrupt_missing_pack(work: Path):
    repo = fresh_repo(work, "corrupt")
    packs = repo / ".lattice" / "packs"
    victims = sorted(packs.glob("000000000000.*"))
    if not victims:
        raise RuntimeError("setup: the first pack is not where the store keeps packs")
    for v in victims:
        v.unlink()
    return repo, ["log"]


def io_refused(work: Path):
    repo = fresh_repo(work, "io")
    (repo / "z.txt").write_text("z\n")
    packs = repo / ".lattice" / "packs"
    packs.chmod(0)
    return repo, ["save", "z"]


def busy(work: Path):
    return fresh_repo(work, "busy"), ["status"]


PROVOCATIONS = [
    ("not-a-repository", not_a_repository),
    ("no-such-line", no_such_line),
    ("invalid-line-name", invalid_line_name),
    ("no-such-change", no_such_change),
    ("change-holds-nothing", change_holds_nothing),
    ("no-such-lens", no_such_lens),
    ("no-remote", no_remote),
    ("checkout-into-repository", checkout_into_repository),
    ("checkout-unknown-checkpoint", checkout_unknown_checkpoint),
    ("checkout-bad-address", checkout_bad_address),
    ("workspace-over-non-empty", workspace_over_non_empty),
    ("merge-no-such-line", merge_no_such_line),
    ("merge-diverged", merge_diverged),
    ("corrupt-missing-pack", corrupt_missing_pack),
    ("io-refused", io_refused),
    ("busy", busy),
]


def provoke(name: str, cwd: Path, argv: list[str]) -> subprocess.CompletedProcess:
    if name != "busy":
        return run(argv, cwd)
    if os.name != "posix":
        raise RuntimeError("the busy provocation holds a POSIX lock; not available here")
    import fcntl
    # Hold the engine's own lock file for longer than it will wait, from a
    # process that never releases early. The child sleeps; this process runs
    # the command and then kills the child, so the hold cannot outlive the
    # harness whatever happens in between.
    holder = subprocess.Popen(
        [sys.executable, "-c",
         "import fcntl,sys,time\n"
         "f=open(sys.argv[1],'w'); fcntl.flock(f, fcntl.LOCK_EX)\n"
         "sys.stdout.write('held\\n'); sys.stdout.flush(); time.sleep(float(sys.argv[2]))",
         str(cwd / ".lattice" / "lock"), str(BUSY_HOLD_S)],
        stdout=subprocess.PIPE, text=True)
    try:
        if holder.stdout.readline().strip() != "held":
            raise RuntimeError("setup: could not take the lock")
        return run(argv, cwd, timeout=BUSY_HOLD_S + TIMEOUT_S)
    finally:
        holder.kill()
        holder.wait()


def surface_first_words() -> set[str]:
    r = subprocess.run([str(LTX), "internals", "command-surface", "--json"],
                       capture_output=True, text=True, errors="replace",
                       timeout=TIMEOUT_S, check=False)
    if r.returncode != 0:
        raise RuntimeError("the binary publishes no command surface")
    names = {c["name"].split()[0] for c in json.loads(r.stdout)["commands"]}
    # `internals` is a real prefix a recovery may name, published as its
    # subcommands rather than as itself.
    return names | {"internals"}


def commands_named(recovery: str) -> list[str]:
    out, rest = [], recovery
    while True:
        pos = rest.find("ltx ")
        if pos < 0:
            return out
        rest = rest[pos + 4:]
        word = ""
        for ch in rest:
            if ch.isalnum() or ch == "-":
                word += ch
            else:
                break
        if word:
            out.append(word)


def judge(doc: dict, known_commands: set[str]) -> list[str]:
    """Every requirement the document misses, by name. Empty means clean."""
    missing = []
    if not str(doc.get("error") or "").strip():
        missing.append("error text")
    if doc.get("category") not in CATEGORIES:
        missing.append(f"category ({doc.get('category')!r} is not a causal category)")
    if doc.get("concept") not in CONCEPTS:
        missing.append(f"concept ({doc.get('concept')!r} is not one of the seven)")
    recovery = str(doc.get("recovery") or "").strip()
    if not recovery:
        missing.append("recovery")
    else:
        for word in commands_named(recovery):
            if word not in known_commands:
                missing.append(f"recovery names `ltx {word}`, which does not exist")
    return missing


def main() -> int:
    global LTX
    ap = argparse.ArgumentParser()
    ap.add_argument("--ltx", type=Path, default=LTX,
                    help="binary under test; exists so this harness can be shown to fail")
    args = ap.parse_args()
    # Absolute, because every provocation runs the binary from inside a
    # scratch repository, where a relative path would resolve to nothing.
    LTX = args.ltx.resolve()
    if not LTX.exists():
        print(json.dumps({"gate": GATE, "status": "not-implemented",
                          "note": "ltx binary not built (target/release/ltx)"}))
        return 0

    try:
        known = surface_first_words()
    except (RuntimeError, json.JSONDecodeError, KeyError) as exc:
        print(json.dumps({"gate": GATE, "status": "not-implemented", "note": str(exc)}))
        return 0

    work = Path(tempfile.mkdtemp(prefix="g2-4-"))
    rows, not_provoked, categories_seen = [], [], set()
    try:
        for name, setup in PROVOCATIONS:
            try:
                cwd, argv = setup(work)
                proc = provoke(name, cwd, argv)
            except (RuntimeError, subprocess.TimeoutExpired, OSError) as exc:
                not_provoked.append({"provocation": name, "why": f"setup: {exc}"[:200]})
                continue
            finally:
                # The io provocation locks a directory; give it back so the
                # scratch tree can be removed whatever else happened.
                packs = work / "io" / ".lattice" / "packs"
                if packs.exists():
                    packs.chmod(stat.S_IRWXU)
            if proc.returncode == 0:
                not_provoked.append({"provocation": name, "why": "command succeeded"})
                continue
            try:
                doc = json.loads(proc.stdout)
            except json.JSONDecodeError:
                not_provoked.append({"provocation": name,
                                     "why": f"not a JSON error document: {proc.stdout[:80]!r}"})
                continue
            if doc.get("ok") is not False:
                not_provoked.append({"provocation": name, "why": "document does not say ok: false"})
                continue
            missing = judge(doc, known)
            categories_seen.add(doc.get("category"))
            rows.append({"provocation": name, "category": doc.get("category"),
                         "concept": doc.get("concept"), "exit": proc.returncode,
                         "missing": missing})
    finally:
        shutil.rmtree(work, ignore_errors=True)

    deficient = [r for r in rows if r["missing"]]
    unseen = sorted(CATEGORIES - categories_seen)
    coverage_ok = not not_provoked and not unseen
    coverage_note = "; ".join(
        ([f"{len(not_provoked)} provocation(s) produced no error: "
          + ", ".join(p["provocation"] for p in not_provoked)] if not_provoked else [])
        + ([f"categories never provoked: {', '.join(unseen)}"] if unseen else []))

    print(json.dumps({
        "gate": GATE,
        "value": len(deficient),
        "unit": "deficient errors",
        "note": (f"{len(deficient)} of {len(rows)} provoked errors deficient; "
                 f"{len(categories_seen)} of {len(CATEGORIES)} categories exercised"),
        "detail": {"errors": rows, "not_provoked": not_provoked,
                   "known_commands": sorted(known)},
        "coverage": {"ok": coverage_ok, "note": coverage_note},
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
