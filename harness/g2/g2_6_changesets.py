#!/usr/bin/env python3
"""
G2.6 — Changeset partitioning (HARD).

§5.4's partial-save capability rests on `ltx split`: the current change is
partitioned so each top-level path group becomes a change of its own. This
harness measures that promise the way §2 words it — scripted scenarios that
must be **completable and undoable** — including splits killed mid-flight.
Measured value = scenarios that fail. Target 0, over exactly 20 scenarios.

Twelve scripted scenarios cover completion and undo:

  - partitions of 2, 5 and 10 top-level groups, nested directories, files
    at the root, and the two no-ops (one group; nothing current at all);
  - a second split after the first must move nothing (idempotence);
  - a split's grouping must come back identical after undo + split again
    (determinism of the partition, not of change ids, which are minted);
  - a change minted by split must be consumable by `save --change`
    (partitioning exists so its pieces can be checkpointed);
  - a split must not touch changes it did not partition;
  - one undo must restore the exact pre-split assignment — same paths on
    the same change id, currency included — and a second undo must then
    take the assignment itself away.

Eight scenarios SIGKILL a split of 60 top-level groups at 1, 3, 6, 10, 15,
25, 40 and 80 ms. After the kill the repository must verify clean and the
change state must be EITHER the pre-split assignment or the completed
partition — a visible half-split is an atomicity failure. From whichever
state survived, the scenario must then complete (split again if needed,
asserting the full partition) and undo back to the one-change assignment.
A kill that lands after the operation finished still runs both legs; the
verdict never depends on where the timer happened to fall, which is what
keeps a timing harness deterministic in verdict.

State is compared as the partition itself: the set of assigned-path groups
per change, plus which change id is current. Change ids minted by a re-run
split may differ; the grouping may not.

Coverage contract (§6): all 20 scenarios must run to a verdict; a scenario
whose setup fails, fails coverage rather than vanishing.

Written after the surface it measures (the reverse of §0.3's order, same as
G2.3–G2.5, said rather than hidden). Shown to FAIL before freezing: against
a wrapper binary whose `split` reports success while partitioning nothing,
every completion scenario reports the missing partition (`--ltx` exists for
exactly that check and for nothing else).
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LTX = REPO / "target" / "release" / "ltx"
GATE = "G2.6"
TIMEOUT_S = 120
KILL_POINTS_MS = [1, 3, 6, 10, 15, 25, 40, 80]
KILL_GROUPS = 60


def run(argv: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run([str(LTX), *argv, "--json"], cwd=cwd, capture_output=True,
                          text=True, errors="replace", timeout=TIMEOUT_S, check=False)


def must(argv: list[str], cwd: Path) -> dict:
    proc = run(argv, cwd)
    if proc.returncode != 0:
        raise RuntimeError(f"`ltx {' '.join(argv)}` failed: {proc.stdout[:200]}")
    return json.loads(proc.stdout)


def fresh_repo(work: Path, name: str) -> Path:
    repo = work / name
    repo.mkdir()
    must(["init"], repo)
    (repo / "base.txt").write_text("base\n")
    must(["save", "base"], repo)
    return repo


def write_groups(repo: Path, groups: dict[str, list[str]]) -> list[str]:
    """Create the files of {top-level group: paths} and return all paths."""
    paths = []
    for members in groups.values():
        for rel in members:
            p = repo / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(rel + "\n")
            paths.append(rel)
    return sorted(paths)


def change_state(repo: Path):
    """The partition as a comparable value: (canonical group sequence,
    the current change's group, {change id: group}). The group sequence is
    a sorted tuple of sorted tuples rather than a set of sets, so a
    duplicate group on an extra open change cannot collapse away — a
    harness that can pass without measuring is worse than no harness."""
    doc = must(["change", "list"], repo)
    groups = tuple(sorted(tuple(sorted(c["assigned"])) for c in doc["changes"]))
    current = next((tuple(sorted(c["assigned"])) for c in doc["changes"] if c["current"]), None)
    by_id = {c["id"]: tuple(sorted(c["assigned"])) for c in doc["changes"]}
    return groups, current, by_id


def expected_partition(groups: dict[str, list[str]]):
    return tuple(sorted(tuple(sorted(members)) for members in groups.values()))


# ------------------------------------------------------------------ scenarios
# Each returns a list of failure strings; empty means the scenario passed.

def scripted_split(groups: dict[str, list[str]]):
    """Assign every group to one change, split, and hold the result to the
    partition; then undo and hold the repository to the pre-split state."""
    def scenario(work: Path, name: str) -> list[str]:
        repo = fresh_repo(work, name)
        paths = write_groups(repo, groups)
        assigned = must(["assign", *sorted(groups)], repo)
        pre = change_state(repo)
        if pre[0] != (tuple(paths),):
            return [f"assign did not produce one change of all paths: {pre[0]}"]
        out = must(["split"], repo)
        failures = []
        post = change_state(repo)
        want = expected_partition(groups)
        if post[0] != want:
            failures.append(f"partition wrong: {sorted(map(sorted, post[0]))}")
        if assigned["change"] not in post[2]:
            failures.append("the split change's id did not survive the split")
        if post[1] is None:
            failures.append("no change is current after split")
        if len(out["into"]) != len(groups) - 1:
            failures.append(f"into named {len(out['into'])} changes for "
                            f"{len(groups)} groups")
        must(["undo"], repo)
        if change_state(repo) != pre:
            failures.append("undo did not restore the pre-split assignment "
                            "on the same change id with the same currency")
        return failures
    return scenario


def one_group_is_noop(work: Path, name: str) -> list[str]:
    repo = fresh_repo(work, name)
    write_groups(repo, {"d": ["d/x.txt", "d/y.txt"]})
    must(["assign", "d"], repo)
    pre = change_state(repo)
    out = must(["split"], repo)
    if out["into"] or out["moved"]:
        return [f"one group split into {out['into']}, moved {out['moved']}"]
    if change_state(repo) != pre:
        return ["a no-op split changed the change state"]
    return []


def nothing_current_is_noop(work: Path, name: str) -> list[str]:
    repo = fresh_repo(work, name)
    pre = change_state(repo)
    out = must(["split"], repo)
    if out["change"] is not None or out["into"] or out["moved"]:
        return [f"split with nothing current reported {out}"]
    if change_state(repo) != pre:
        return ["split with nothing current changed the change state"]
    return []


def resplit_moves_nothing(work: Path, name: str) -> list[str]:
    repo = fresh_repo(work, name)
    write_groups(repo, {"a.txt": ["a.txt"], "b.txt": ["b.txt"]})
    must(["assign", "a.txt", "b.txt"], repo)
    must(["split"], repo)
    settled = change_state(repo)
    out = must(["split"], repo)
    if out["into"] or out["moved"]:
        return [f"second split moved {out['moved']} into {out['into']}"]
    if change_state(repo) != settled:
        return ["a second split changed the change state while reporting no moves"]
    return []


def undo_then_resplit_same_partition(work: Path, name: str) -> list[str]:
    groups = {"a.txt": ["a.txt"], "d1": ["d1/x.txt", "d1/y.txt"], "d2": ["d2/z.txt"]}
    repo = fresh_repo(work, name)
    write_groups(repo, groups)
    must(["assign", *sorted(groups)], repo)
    must(["split"], repo)
    first = change_state(repo)[0]
    must(["undo"], repo)
    must(["split"], repo)
    second = change_state(repo)[0]
    if first != second:
        return [f"split after undo grouped differently: {sorted(map(sorted, second))}"]
    return []


def split_product_is_savable(work: Path, name: str) -> list[str]:
    repo = fresh_repo(work, name)
    write_groups(repo, {"a.txt": ["a.txt"], "d1": ["d1/x.txt"]})
    must(["assign", "a.txt", "d1"], repo)
    out = must(["split"], repo)
    minted = out["into"][0]
    saved = must(["save", "--change", minted, "one piece"], repo)
    failures = []
    if saved["change"] != minted:
        failures.append(f"save consumed {saved['change']}, not the minted {minted}")
    if minted in change_state(repo)[2]:
        failures.append("the saved change is still open")
    return failures


def split_leaves_other_changes_alone(work: Path, name: str) -> list[str]:
    repo = fresh_repo(work, name)
    write_groups(repo, {"a.txt": ["a.txt"], "b.txt": ["b.txt"]})
    must(["assign", "a.txt", "b.txt"], repo)
    must(["split"], repo)
    bystanders = {gid: grp for gid, grp in change_state(repo)[2].items()}
    write_groups(repo, {"c.txt": ["c.txt"], "d.txt": ["d.txt"]})
    current = next(cid for cid, _ in bystanders.items()
                   if change_state(repo)[2].get(cid) == change_state(repo)[1])
    must(["assign", "c.txt", "d.txt"], repo)
    must(["split"], repo)
    after = change_state(repo)[2]
    failures = []
    for cid, grp in bystanders.items():
        if cid == current:
            continue
        if after.get(cid) != grp:
            failures.append(f"split touched bystander change {cid[:8]}: "
                            f"{sorted(grp)} -> {sorted(after.get(cid, []))}")
    return failures


def double_undo_takes_assignment_away(work: Path, name: str) -> list[str]:
    repo = fresh_repo(work, name)
    write_groups(repo, {"a.txt": ["a.txt"], "b.txt": ["b.txt"]})
    must(["assign", "a.txt", "b.txt"], repo)
    must(["split"], repo)
    must(["undo"], repo)
    must(["undo"], repo)
    state = change_state(repo)
    if state[0] != ():
        return [f"after undoing split and assign, changes remain: {state[0]}"]
    return []


def oplog_last_seq(repo: Path) -> int:
    doc = must(["internals", "oplog"], repo)
    ops = doc.get("operations", [])
    return ops[-1]["seq"] if ops else 0


# Whether each kill demonstrably landed while the split was in flight or
# after its publish (its op-log entry exists), filled by interrupted_split
# and read by the §6 coverage contract in main(). A kill that fired before
# the process reached the operation leaves no footprint and proves nothing;
# a kill after capture but before the single publish leaves the same
# nothing and proves everything — the two are indistinguishable from
# outside, which is WHY coverage is asserted over the family of eight
# timings rather than per scenario: enough kills must be seen to intersect
# the operation for the sweep to have measured it.
KILL_FOOTPRINTS: list[bool] = []
MIN_KILLS_LANDED = 3


def interrupted_split(kill_ms: int):
    """SIGKILL a wide split mid-flight; the surviving state must be all or
    nothing, then completable, then undoable — back to the full pre-split
    state, change id and currency included."""
    def scenario(work: Path, name: str) -> list[str]:
        groups = {f"g{i:02d}": [f"g{i:02d}/f.txt"] for i in range(KILL_GROUPS)}
        repo = fresh_repo(work, name)
        write_groups(repo, groups)
        must(["assign", *sorted(groups)], repo)
        pre = change_state(repo)
        want = expected_partition(groups)
        pre_seq = oplog_last_seq(repo)

        proc = subprocess.Popen([str(LTX), "split", "--json"], cwd=repo,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(kill_ms / 1000.0)
        proc.kill()
        proc.wait(timeout=TIMEOUT_S)
        KILL_FOOTPRINTS.append(oplog_last_seq(repo) > pre_seq)

        failures = []
        verify = must(["verify"], repo)
        if not verify["ok"]:
            failures.append(f"verify not clean after kill: {verify['errors'][:2]}")
        observed = change_state(repo)
        if observed[0] not in (pre[0], want):
            failures.append(
                f"half a split is visible after a {kill_ms}ms kill: "
                f"{observed[0]}")
        if observed[0] != want:
            must(["split"], repo)
            if change_state(repo)[0] != want:
                failures.append("split after the kill did not complete the partition")
        must(["undo"], repo)
        after_undo = change_state(repo)
        if after_undo != pre:
            failures.append("undo after the interrupted split did not restore "
                            "the pre-split state, change id and currency included")
        return failures
    return scenario


SCENARIOS = [
    ("two-files", scripted_split({"a.txt": ["a.txt"], "b.txt": ["b.txt"]})),
    ("five-groups", scripted_split({
        "a.txt": ["a.txt"], "b.txt": ["b.txt"],
        "d1": ["d1/x.txt", "d1/y.txt"], "d2": ["d2/z.txt"],
        "d3": ["d3/deep/q.txt"]})),
    ("ten-files", scripted_split({f"f{i}.txt": [f"f{i}.txt"] for i in range(10)})),
    ("nested-directories", scripted_split({
        "d1": ["d1/a/b/c.txt", "d1/a/d.txt"], "d2": ["d2/x/y.txt"]})),
    ("one-group-is-noop", one_group_is_noop),
    ("nothing-current-is-noop", nothing_current_is_noop),
    ("resplit-moves-nothing", resplit_moves_nothing),
    ("undo-then-resplit-same-partition", undo_then_resplit_same_partition),
    ("split-product-is-savable", split_product_is_savable),
    ("split-leaves-other-changes-alone", split_leaves_other_changes_alone),
    ("double-undo-takes-assignment-away", double_undo_takes_assignment_away),
    ("wide-partition", scripted_split(
        {f"w{i:02d}": [f"w{i:02d}/f.txt"] for i in range(12)})),
] + [(f"killed-at-{ms}ms", interrupted_split(ms)) for ms in KILL_POINTS_MS]


def main() -> int:
    global LTX
    ap = argparse.ArgumentParser()
    ap.add_argument("--ltx", type=Path, default=LTX,
                    help="binary under test; exists so this harness can be shown to fail")
    args = ap.parse_args()
    LTX = args.ltx.resolve()
    if not LTX.exists():
        # A HARD gate with no instrument is a measurement failure, never
        # N/A-yet: exit nonzero so gauntlet records FAIL(harness-error).
        print(json.dumps({"gate": GATE,
                          "note": "ltx binary not built (target/release/ltx)"}))
        return 1

    work = Path(tempfile.mkdtemp(prefix="g2-6-"))
    rows, not_run = [], []
    try:
        for name, scenario in SCENARIOS:
            try:
                failures = scenario(work, name)
            except (RuntimeError, subprocess.TimeoutExpired, OSError,
                    json.JSONDecodeError, KeyError, IndexError,
                    StopIteration) as exc:
                not_run.append({"scenario": name, "why": f"{exc}"[:200]})
                continue
            rows.append({"scenario": name, "failures": failures})
    finally:
        import shutil
        shutil.rmtree(work, ignore_errors=True)

    failed = [r for r in rows if r["failures"]]
    kills_landed = sum(KILL_FOOTPRINTS)
    coverage_ok = (not not_run and len(rows) == len(SCENARIOS)
                   and kills_landed >= MIN_KILLS_LANDED)
    coverage_note = "; ".join(
        ([f"{len(not_run)} scenario(s) did not run: "
          + ", ".join(p["scenario"] for p in not_run)] if not_run else [])
        + ([f"only {kills_landed} of {len(KILL_FOOTPRINTS)} kills left a "
            f"footprint, {MIN_KILLS_LANDED} required — the sweep may have "
            f"missed the operation"] if kills_landed < MIN_KILLS_LANDED else []))

    print(json.dumps({
        "gate": GATE,
        "value": len(failed),
        "unit": "failures",
        "note": (f"{len(failed)} of {len(rows)} scenarios failed; "
                 f"{len(KILL_POINTS_MS)} kills, {kills_landed} with a footprint"),
        "detail": {"scenarios": rows, "not_run": not_run},
        "coverage": {"ok": coverage_ok, "note": coverage_note},
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
