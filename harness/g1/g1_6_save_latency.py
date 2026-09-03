#!/usr/bin/env python3
"""
G1.6 — `ltx save` latency (SOFT, perf, timing).

Target: p95 < 250 ms on replayed real-commit edit sets; cost proportional to
changed data, never repo size, verified by a scaling curve.

The edit sets are replays of real consecutive commits from the pinned base
repos. §6 forbids the harness inventing edits, so it fails rather than
synthesising them when the edit sets are absent.

The scaling check is the part that makes this gate mean something. A save that
is fast on the reference repo but whose cost tracks repository size rather than
changed bytes has not met the requirement, however good its p95 looks -- so the
harness measures against progressively larger prefixes of the repo and reports
the correlation. A strong positive correlation with repo size is a FAIL even if
the p95 target is met.
"""
from __future__ import annotations
import shutil
import statistics
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import ltxrun as L  # noqa: E402

GATE = "G1.6"
SCALE_FRACTIONS = [0.25, 0.5, 1.0]
MAX_SIZE_CORRELATION = 0.5   # Pearson r above this = cost tracks repo size


def apply_edit(repo: Path, edit: dict) -> None:
    """Materialise one real commit's file changes onto the working tree."""
    for change in edit.get("changes", []):
        path = repo / change["path"]
        if change["op"] == "delete":
            path.unlink(missing_ok=True)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(bytes.fromhex(change["content_hex"]))


def pearson(xs: list[float], ys: list[float]) -> float:
    if len(xs) < 2:
        return 0.0
    mx, my = statistics.mean(xs), statistics.mean(ys)
    num = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    dx = sum((x - mx) ** 2 for x in xs) ** 0.5
    dy = sum((y - my) ** 2 for y in ys) ** 0.5
    return num / (dx * dy) if dx and dy else 0.0


def main() -> int:
    try:
        L.require_ltx()
        L.require_reference_repo()
        edits = L.load_edit_sets()
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))

    if len(edits) < L.MIN_WARM_RUNS:
        return L.emit({
            "gate": GATE, "value": 1e9, "unit": "ms",
            "note": f"only {len(edits)} replayable edit sets, "
                    f"{L.MIN_WARM_RUNS} required for a p95"})

    work = Path(tempfile.mkdtemp(prefix="g1-6-"))
    try:
        repo = L.clone_reference(work / "repo")
        if L.run(["adopt"], cwd=repo).returncode != 0:
            return L.not_implemented(GATE, "ltx adopt failed")

        counter = {"i": 0}

        def before(_):
            apply_edit(repo, edits[counter["i"] % len(edits)])
            counter["i"] += 1

        both = L.measure_both(["save", "replayed edit"], cwd=repo,
                              runs=L.MIN_WARM_RUNS, before=before)
        resident = both["daemon_resident"]

        # Scaling curve: does save cost track changed data or repository size?
        scaling = []
        for frac in SCALE_FRACTIONS:
            sub = L.clone_reference(work / f"scale-{frac}")
            paths = sorted(p for p in sub.rglob("*") if p.is_file()
                           and ".git" not in p.parts)
            for victim in paths[int(len(paths) * frac):]:
                victim.unlink(missing_ok=True)
            if L.run(["adopt"], cwd=sub).returncode != 0:
                continue
            c = {"i": 0}

            def before_sub(_):
                apply_edit(sub, edits[c["i"] % len(edits)])
                c["i"] += 1

            t = L.time_command(["save", "scaling probe"], cwd=sub, runs=25,
                               warmup=3, before=before_sub)
            scaling.append({"fraction_of_repo": frac,
                            "files": int(len(paths) * frac),
                            "p95_ms": round(t.p95, 3)})
            shutil.rmtree(sub, ignore_errors=True)

        r = pearson([s["fraction_of_repo"] for s in scaling],
                    [s["p95_ms"] for s in scaling]) if len(scaling) >= 2 else 0.0
        cost_tracks_size = r > MAX_SIZE_CORRELATION

        detail = {**both, "scaling_curve": scaling,
                  "size_correlation_r": round(r, 4),
                  "max_allowed_r": MAX_SIZE_CORRELATION,
                  "cost_tracks_repo_size": cost_tracks_size}

        if resident["failures"] or resident["runs"] < L.MIN_WARM_RUNS:
            return L.emit({"gate": GATE, "value": 1e9, "unit": "ms",
                           "note": f"{resident['failures']} failed invocations",
                           "detail": detail})
        if cost_tracks_size:
            return L.emit({
                "gate": GATE, "value": 1e9, "unit": "ms",
                "note": f"p95 {resident['p95_ms']:.1f} ms meets the target, but save "
                        f"cost correlates with repo size (r={r:.3f} > "
                        f"{MAX_SIZE_CORRELATION}); the gate requires cost "
                        f"proportional to changed data",
                "detail": detail})

        return L.emit({
            "gate": GATE, "value": resident["p95_ms"], "unit": "ms",
            "note": (f"p95 {resident['p95_ms']:.1f} ms resident, "
                     f"{both['daemonless']['p95_ms']:.1f} ms daemonless; "
                     f"size correlation r={r:.3f}"),
            "detail": detail,
            "evidence": ["bench/ENVIRONMENT.md", "corpus/data/edit-sets.jsonl"],
        })
    except L.NotBuilt as exc:
        return L.not_implemented(GATE, str(exc))
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
