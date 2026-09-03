#!/usr/bin/env python3
"""
G0.7 — Provenance demand probe (HARD).

Standalone by mandate: reads plain `git log` output only. It must not require
the Lattice bridge, store, or CLI, because a probe that needs the product cannot
inform whether to build the product.

The pre-registered criteria live in corpus/preregistration/g0-7-provenance-demand.md
and are hash-pinned. This harness refuses to report a verdict if that file's
hash differs from the pin recorded beside it — criteria cannot be edited after
seeing data.
"""
from __future__ import annotations
import hashlib
import json
import re
import statistics
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PREREG = REPO / "corpus" / "preregistration" / "g0-7-provenance-demand.md"
PIN = REPO / "corpus" / "preregistration" / "g0-7-provenance-demand.sha256"
PROBE_DIR = REPO / "corpus" / "data" / "probe-repos"
MINE_DIR = REPO / "corpus" / "data" / "repos"
OUT = REPO / "corpus" / "data" / "g0-7-probe.json"

WINDOW = 20_000
RECENT_WINDOW = 2_000
AGENT_HEAVY = ["home-assistant/core", "microsoft/vscode", "nodejs/node",
               "kubernetes/kubernetes"]
CONTRAST = ["git/git", "postgres/postgres", "django/django", "rails/rails"]

# Pre-registered thresholds (§ this file's pre-registration; do not edit).
C1_MIN = 5.0
C2_MIN = 20.0
C3_MIN_PP = 3.0
WEAK_GO_BAND_PP = 1.0

CI_AUTOMATION = re.compile(
    r"dependabot|renovate|github[- ]actions|greenkeeper|snyk[- ]bot|mergify|"
    r"codecov|allcontributors|pre-commit-ci|semantic-release|imgbot|whitesource|"
    r"restyled|stale\[bot\]|netlify|vercel\[bot\]|azure-pipelines|travis|appveyor|"
    r"circleci|jenkins|buildkite|release-please|changeset-bot|weblate|transifex|"
    r"crowdin|pyup|scala-steward|bors|autoroller|gitbot|update-bot|k8s-ci-robot|"
    r"openshift-|prow", re.I)
AUTHORING_AGENT = re.compile(
    r"\bclaude\b|copilot|\bcursor\b|\bdevin\b|\bcodex\b|\baider\b|\bsweep\b|"
    r"chatgpt|openai|anthropic|\bgpt-?[0-9]|codeium|tabnine|sourcegraph[- ]cody|"
    r"\bcody\b|gemini[- ]code|jules\b|\bdroid\b|factory[- ]ai", re.I)
BOT_MARK = re.compile(r"\[bot\]|\bbot@|^bot$|-bot\b|\bbot-", re.I)

TRAILERS = {
    "Co-Authored-By": re.compile(r"^co-authored-by:\s*(.+)$", re.I | re.M),
    "Signed-off-by": re.compile(r"^signed-off-by:\s*(.+)$", re.I | re.M),
    "Reviewed-by": re.compile(r"^reviewed-by:\s*(.+)$", re.I | re.M),
    "Generated-by": re.compile(r"^generated-by:\s*(.+)$", re.I | re.M),
    "Assisted-by": re.compile(r"^assisted-by:\s*(.+)$", re.I | re.M),
    "Change-Id": re.compile(r"^change-id:\s*(.+)$", re.I | re.M),
    "PR-URL": re.compile(r"^pr-url:\s*(.+)$", re.I | re.M),
    "Reviewed-On": re.compile(r"^reviewed-on:\s*(.+)$", re.I | re.M),
}

REC_SEP, FIELD_SEP = "\x1e", "\x1f"


def repo_dir(slug: str) -> Path | None:
    name = slug.replace("/", "__") + ".git"
    for base in (PROBE_DIR, MINE_DIR):
        p = base / name
        if (p / "HEAD").exists():
            return p
    return None


def classify_identity(name: str, email: str) -> str | None:
    blob = f"{name} {email}"
    if AUTHORING_AGENT.search(blob):
        return "authoring-agent"
    if CI_AUTOMATION.search(blob) or BOT_MARK.search(blob):
        return "ci-automation"
    return None


def probe(slug: str) -> dict | None:
    path = repo_dir(slug)
    if path is None:
        return None
    try:
        raw = subprocess.run(
            ["git", "-C", str(path), "log", f"-n{WINDOW}", "--no-merges",
             f"--format=%H{FIELD_SEP}%an{FIELD_SEP}%ae{FIELD_SEP}%cn"
             f"{FIELD_SEP}%ce{FIELD_SEP}%B{REC_SEP}"],
            capture_output=True, text=True, timeout=900, errors="replace")
    except subprocess.TimeoutExpired:
        return None
    if raw.returncode != 0:
        return None

    counts = {k: 0 for k in ("commits", "s1", "s1_agent", "s1_ci",
                             "s2", "s2_agent", "s3", "any", "agent_route")}
    per_trailer: dict[str, int] = {k: 0 for k in TRAILERS}
    recent = {"commits": 0, "agent_route": 0, "any": 0}

    for idx, rec in enumerate(raw.stdout.split(REC_SEP)):
        rec = rec.strip("\n")
        if not rec.strip():
            continue
        parts = rec.split(FIELD_SEP)
        if len(parts) < 6:
            continue
        _, an, ae, cn, ce, body = parts[0], parts[1], parts[2], parts[3], parts[4], parts[5]
        counts["commits"] += 1
        is_recent = idx < RECENT_WINDOW
        if is_recent:
            recent["commits"] += 1

        # S1: the commit's own author/committer identity is non-human.
        klass = classify_identity(an, ae) or classify_identity(cn, ce)
        s1 = klass is not None
        if s1:
            counts["s1"] += 1
            counts["s1_agent" if klass == "authoring-agent" else "s1_ci"] += 1

        # S2: co-authorship trailers, and the agent subset.
        s2 = s2_agent = False
        for m in TRAILERS["Co-Authored-By"].finditer(body):
            s2 = True
            if classify_identity(m.group(1), m.group(1)):
                s2_agent = True
        if s2:
            counts["s2"] += 1
        if s2_agent:
            counts["s2_agent"] += 1

        # S3: any structured provenance trailer.
        s3 = False
        for name, pat in TRAILERS.items():
            if pat.search(body):
                per_trailer[name] += 1
                s3 = True
        if s3:
            counts["s3"] += 1

        agent_route = s1 or s2_agent
        if agent_route:
            counts["agent_route"] += 1
            if is_recent:
                recent["agent_route"] += 1
        if s1 or s2 or s3:
            counts["any"] += 1
            if is_recent:
                recent["any"] += 1

    n = counts["commits"] or 1
    return {
        "repo": slug, "path": str(path.relative_to(REPO)),
        "counts": counts, "per_trailer": per_trailer,
        "pct_agent_route": 100.0 * counts["agent_route"] / n,
        "pct_any_convention": 100.0 * counts["any"] / n,
        "pct_s1_agent": 100.0 * counts["s1_agent"] / n,
        "pct_s1_ci": 100.0 * counts["s1_ci"] / n,
        "recent": {
            "window": recent["commits"],
            "pct_agent_route": 100.0 * recent["agent_route"] / (recent["commits"] or 1),
            "pct_any_convention": 100.0 * recent["any"] / (recent["commits"] or 1),
        },
    }


def main() -> int:
    # The pin gate: criteria may not move after data is seen.
    if not PREREG.exists() or not PIN.exists():
        print(json.dumps({"gate": "G0.7", "value": 0, "unit": "repos",
                          "note": "pre-registration or its hash pin is missing"}))
        return 0
    actual = hashlib.sha256(PREREG.read_bytes()).hexdigest()
    pinned = PIN.read_text().split()[0]
    if actual != pinned:
        print(json.dumps({
            "gate": "G0.7", "value": 0, "unit": "repos",
            "note": f"PRE-REGISTRATION HASH MISMATCH — criteria were edited "
                    f"(pinned {pinned[:12]}, actual {actual[:12]}). Verdict refused."}))
        return 0

    heavy = [r for r in (probe(s) for s in AGENT_HEAVY) if r]
    contrast = [r for r in (probe(s) for s in CONTRAST) if r]

    if len(heavy) < 3:
        print(json.dumps({
            "gate": "G0.7", "value": len(heavy), "unit": "repos",
            "note": f"only {len(heavy)} of the pre-registered agent-heavy repos "
                    f"are available; ≥3 required",
            "detail": {"agent_heavy": heavy, "contrast": contrast}}))
        return 0

    med = lambda rows, key: statistics.median(r[key] for r in rows)  # noqa: E731
    c1_val = med(heavy, "pct_agent_route")
    c2_val = med(heavy, "pct_any_convention")
    c3_val = c1_val - (med(contrast, "pct_agent_route") if contrast else 0.0)

    c1, c2 = c1_val >= C1_MIN, c2_val >= C2_MIN
    c3 = c3_val >= C3_MIN_PP
    if c1 and c2 and c3:
        verdict = "GO"
    elif c1 and c2 and (C3_MIN_PP - c3_val) < WEAK_GO_BAND_PP:
        verdict = "WEAK-GO"
    else:
        verdict = "NO-GO"

    payload = {
        "prereg_sha256": actual,
        "verdict": verdict,
        "criteria": {
            "C1": {"metric": "median % commits attributable to a non-human author",
                   "value": round(c1_val, 3), "threshold": C1_MIN, "pass": c1},
            "C2": {"metric": "median % commits carrying any ad-hoc provenance convention",
                   "value": round(c2_val, 3), "threshold": C2_MIN, "pass": c2},
            "C3": {"metric": "agent-heavy minus contrast, percentage points",
                   "value": round(c3_val, 3), "threshold": C3_MIN_PP, "pass": c3},
        },
        "agent_heavy": heavy,
        "contrast": contrast,
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(payload, indent=2) + "\n")

    verdict_doc = REPO / "docs" / "G0-7-VERDICT.md"
    print(json.dumps({
        "gate": "G0.7",
        "value": len(heavy),
        "unit": "repos",
        "note": f"verdict {verdict}: C1={c1_val:.2f}% (≥{C1_MIN}), "
                f"C2={c2_val:.2f}% (≥{C2_MIN}), C3={c3_val:+.2f}pp (≥{C3_MIN_PP})"
                + ("" if verdict_doc.exists() else " | written verdict doc missing"),
        "detail": payload,
        "evidence": [str(OUT.relative_to(REPO)), str(PREREG.relative_to(REPO))],
    }))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
