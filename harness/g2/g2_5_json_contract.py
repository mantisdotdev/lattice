#!/usr/bin/env python3
"""
G2.5 — JSON contract (HARD).

The brief promises agents "a stable JSON object instead of prose" for every
command. This harness makes the promise mechanical. Measured value = contract
gaps across the published normal-path surface: a command with no schema below,
a command whose scenario never ran, output that fails validation against its
schema, or a command absent from `docs/json-contract.md` (or documented with
top-level fields missing). Target 0.

Method: one scripted session in a scratch repository exercises every
normal-path command's success path — the exact sequence a user would take:
init, save, look around, assign and split a change, branch a line and merge
it back, undo, a second workspace, a lens, checkout, thin, redact, and a
dry-run sync. Each step's stdout must be exactly one JSON document that
validates against that command's schema. One provoked failure (`switch
nowhere`) must validate against the error-document schema, which is the
shape G2.4 holds every error to.

The schemas are inline and closed (`additionalProperties: false` on every
success document) for the same reason G2.3's vocabulary is inline: the §0.3
freeze pins the entry script, so a field can be added to the contract only by
refreezing this harness — a deliberate, recorded act. The error document is
closed too: it is exactly `ok, error, category, concept, recovery`, and a
sixth field is a contract change, not a garnish. Validation is a small
subset of JSON Schema (type, required, properties, additionalProperties,
items, enum, const) implemented here, because the harness must run on a bare
CI runner with the standard library alone.

`docs/json-contract.md` is the measured artifact, not part of the
instrument: it must carry a `## <command>` section per command (plus
`## error document`) naming every top-level field in backticks. Documentation
that drifts from the schema is a gap, which is the "docs coverage" leg of
the metric.

Out of scope, decided in ADR-23: field-name vocabulary (§5.1 deliberately
puts `chunks_verified` in verify's mouth — machine fields use storage words),
and the `version` field asymmetry (list documents carry `version: 1`,
act documents do not; codified as-is).

Coverage contract (§6): the walk must exercise every command the binary
itself publishes via `internals command-surface --json` (internals aside),
at least MIN_COMMANDS of them, and every scenario step must run; a setup
failure fails coverage rather than passing silently.

Written after the surface it measures (the reverse of §0.3's order, same as
G2.3 and G2.4, said here rather than hidden). Shown to FAIL before freezing:
against a shim binary that answers `{"ok": true}` to everything it reports a
validation gap for every command (`--ltx` exists for exactly that check and
for nothing else).
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LTX = REPO / "target" / "release" / "ltx"
DOCS = REPO / "docs" / "json-contract.md"
GATE = "G2.5"
TIMEOUT_S = 120
MIN_COMMANDS = 18

STR = {"type": "string"}
NSTR = {"type": ["string", "null"]}
INT = {"type": "integer"}
BOOL = {"type": "boolean"}
OK = {"const": True}
V1 = {"const": 1}


def closed(props: dict) -> dict:
    return {"type": "object", "properties": props,
            "required": sorted(props), "additionalProperties": False}


SCHEMAS = {
    "init": closed({"ok": OK, "root": STR}),
    "save": closed({"ok": OK, "checkpoint": STR, "tree": STR, "message": STR,
                    "parent": NSTR, "oplog_seq": INT, "change": NSTR,
                    "working_state": STR, "rescued_working_state": NSTR}),
    "status": closed({"ok": OK, "status": closed({
        "checkpoints": INT, "chunks": INT, "head": NSTR, "head_change": NSTR,
        "head_message": NSTR, "operations": INT, "packs": INT, "root": STR})}),
    "log": closed({"ok": OK, "forensic": BOOL,
                   "checkpoints": {"type": "array", "items": closed({
                       "at_unix_ms": INT, "id": STR, "message": STR,
                       "oplog_seq": INT, "parent": NSTR, "tree": STR})}}),
    "verify": closed({"ok": BOOL, "checkpoints": INT, "checkpoints_partial": INT,
                      "chunks_absent": INT, "chunks_redacted": INT,
                      "chunks_verified": INT, "complete": BOOL,
                      "errors": {"type": "array", "items": STR},
                      "oplog_entries": INT, "structure_verified": BOOL}),
    "checkout": closed({"ok": OK, "checkpoint": STR, "into": STR, "entries": INT,
                        "collisions": {"type": "array", "items": closed({
                            "path": STR, "reason": STR, "collided_with": STR})}}),
    "undo": closed({"ok": OK, "nothing_to_undo": BOOL, "undone_checkpoint": NSTR,
                    "now_at": NSTR, "undo_seq": INT, "oplog_seq": INT,
                    "preserved_working_state": NSTR, "rescued_working_state": NSTR,
                    "remote_effects_not_undone": {"type": "array", "items": STR}}),
    "start": closed({"ok": OK, "line": STR, "created": BOOL, "now_at": NSTR,
                     "rescued_working_state": NSTR, "oplog_seq": INT}),
    "switch": closed({"ok": OK, "line": STR, "now_at": NSTR, "oplog_seq": INT,
                      "rescued_working_state": NSTR}),
    "assign": closed({"ok": OK, "change": STR, "short": STR, "created": BOOL,
                      "line": STR, "assigned": {"type": "array", "items": STR},
                      "refused": {"type": "array",
                                  "items": closed({"path": STR, "reason": STR})},
                      "oplog_seq": INT, "rescued_working_state": NSTR}),
    "workspace new": closed({"ok": OK, "workspace": STR, "root": STR,
                             "entries": INT, "oplog_seq": INT,
                             "rescued_working_state": NSTR}),
    "workspace list": closed({"ok": OK, "version": V1,
                              "workspaces": {"type": "array", "items": closed({
                                  "id": STR, "present": BOOL, "root": STR,
                                  "short": STR})}}),
    "change list": closed({"ok": OK, "version": V1,
                           "changes": {"type": "array", "items": closed({
                               "assigned": {"type": "array", "items": STR},
                               "current": BOOL, "id": STR, "short": STR})}}),
    "line list": closed({"ok": OK, "version": V1, "current": STR,
                         "lines": {"type": "array", "items": closed({
                             "name": STR, "checkpoint": NSTR})}}),
    "redact": closed({"ok": OK, "target": STR, "chunks_destroyed": INT,
                      "places_in_history": INT, "oplog_seq": INT,
                      "rescued_working_state": NSTR}),
    "merge": closed({"ok": OK, "line": STR, "from": STR, "fast_forward": BOOL,
                     "now_at": STR, "oplog_seq": INT,
                     "rescued_working_state": NSTR}),
    "split": closed({"ok": OK, "change": NSTR,
                     "into": {"type": "array", "items": STR}, "moved": INT,
                     "oplog_seq": INT, "rescued_working_state": NSTR}),
    "lens use": closed({"ok": OK, "lens": STR, "oplog_seq": INT,
                        "rescued_working_state": NSTR}),
    "lens list": closed({"ok": OK, "version": V1,
                         "lenses": {"type": "array", "items": closed({
                             "active": BOOL, "hides": STR, "name": STR})}}),
    "sync": closed({"ok": OK, "dry_run": BOOL, "remote": NSTR,
                    "would_send": INT, "would_receive": INT, "oplog_seq": INT,
                    "rescued_working_state": NSTR}),
    "thin": closed({"ok": OK, "collected": INT, "packs_removed": INT,
                    "oplog_seq": INT, "rescued_working_state": NSTR}),
    "error document": closed({"ok": {"const": False}, "error": STR,
                              "category": {"enum": ["not-a-repository", "not-found",
                                                    "corrupt", "io", "invalid", "busy"]},
                              "concept": {"enum": ["working-state", "change",
                                                   "checkpoint", "line", "lens",
                                                   "workspace", "remote"]},
                              "recovery": STR}),
}

# One session, in order. (schema key, argv, file to write first, expected exit)
SCENARIO = [
    ("init", ["init"], None, 0),
    ("save", ["save", "first"], ("a.txt", "a\n"), 0),
    ("status", ["status"], None, 0),
    ("log", ["log"], None, 0),
    ("verify", ["verify"], None, 0),
    ("assign", ["assign", "b.txt"], ("b.txt", "b\n"), 0),
    ("change list", ["change", "list"], None, 0),
    ("split", ["split"], None, 0),
    ("start", ["start", "feat"], None, 0),
    ("save", ["save", "on feat"], ("f.txt", "f\n"), 0),
    ("switch", ["switch", "main"], None, 0),
    ("merge", ["merge", "feat"], None, 0),
    ("undo", ["undo"], None, 0),
    ("workspace new", ["workspace", "new", "../ws2"], None, 0),
    ("workspace list", ["workspace", "list"], None, 0),
    ("lens use", ["lens", "use", "clean"], None, 0),
    ("lens list", ["lens", "list"], None, 0),
    ("line list", ["line", "list"], None, 0),
    ("checkout", ["checkout", "--into", "../out"], None, 0),
    ("thin", ["thin"], None, 0),
    ("redact", ["redact", "b.txt", "--confirm-destroy"], None, 0),
    ("sync", ["sync", "--dry-run"], None, 0),
    ("error document", ["switch", "nowhere"], None, 1),
]


def validate(doc, schema, path="$") -> list[str]:
    """Violations of the schema subset this contract uses. Empty means valid."""
    errs = []
    if "const" in schema:
        if doc != schema["const"]:
            errs.append(f"{path}: expected {schema['const']!r}, got {doc!r}")
        return errs
    if "enum" in schema:
        if doc not in schema["enum"]:
            errs.append(f"{path}: {doc!r} not one of {schema['enum']}")
        return errs
    types = schema.get("type")
    if types is not None:
        names = types if isinstance(types, list) else [types]
        checks = {"object": dict, "array": list, "string": str,
                  "integer": int, "boolean": bool, "null": type(None)}
        # bool is an int in Python; an integer field must refuse True.
        matched = any(isinstance(doc, checks[n]) and not
                      (n == "integer" and isinstance(doc, bool)) for n in names)
        if not matched:
            errs.append(f"{path}: expected {'|'.join(names)}, got {type(doc).__name__}")
            return errs
    if isinstance(doc, dict) and "properties" in schema:
        for key in schema.get("required", []):
            if key not in doc:
                errs.append(f"{path}: missing required field `{key}`")
        for key, val in doc.items():
            if key in schema["properties"]:
                errs.extend(validate(val, schema["properties"][key], f"{path}.{key}"))
            elif schema.get("additionalProperties") is False:
                errs.append(f"{path}: undeclared field `{key}`")
    if isinstance(doc, list) and "items" in schema:
        for i, item in enumerate(doc):
            errs.extend(validate(item, schema["items"], f"{path}[{i}]"))
    return errs


def run(argv: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run([str(LTX), *argv, "--json"], cwd=cwd, capture_output=True,
                          text=True, errors="replace", timeout=TIMEOUT_S, check=False)


def published_normal_paths() -> set[str]:
    proc = subprocess.run([str(LTX), "internals", "command-surface", "--json"],
                          capture_output=True, text=True, errors="replace",
                          timeout=TIMEOUT_S, check=False)
    if proc.returncode != 0:
        raise RuntimeError("the binary publishes no command surface")
    names = {c["name"] for c in json.loads(proc.stdout)["commands"]}
    return {n for n in names if n.split()[0] != "internals"}


def docs_gaps() -> list[dict]:
    """Commands missing from docs/json-contract.md, or documented without
    every top-level field of their schema named in backticks."""
    gaps = []
    if not DOCS.exists():
        return [{"command": name, "gap": "docs file missing"} for name in SCHEMAS]
    sections: dict[str, str] = {}
    current = None
    for line in DOCS.read_text().splitlines():
        m = re.match(r"^## (.+?)\s*$", line)
        if m:
            current = m.group(1).strip("`")
            sections[current] = ""
        elif current is not None:
            sections[current] += line + "\n"
    for name, schema in sorted(SCHEMAS.items()):
        if name not in sections:
            gaps.append({"command": name, "gap": "no docs section"})
            continue
        fields = set(schema["properties"])
        documented = set(re.findall(r"`([a-z0-9_]+)`", sections[name]))
        missing = sorted(fields - documented)
        if missing:
            gaps.append({"command": name,
                         "gap": f"docs omit field(s): {', '.join(missing)}"})
    return gaps


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
    try:
        published = published_normal_paths()
    except (RuntimeError, json.JSONDecodeError, KeyError) as exc:
        print(json.dumps({"gate": GATE, "note": str(exc)}))
        return 1

    work = Path(tempfile.mkdtemp(prefix="g2-5-"))
    repo = work / "r"
    repo.mkdir()
    rows, setup_failures, exercised = [], [], set()
    try:
        for name, argv, write, want_exit in SCENARIO:
            if write:
                (repo / write[0]).write_text(write[1])
            try:
                proc = run(argv, repo)
            except (subprocess.TimeoutExpired, OSError) as exc:
                setup_failures.append(f"{name}: {exc}")
                continue
            exercised.add(name)
            step = {"command": name, "argv": argv, "problems": []}
            if proc.returncode != want_exit:
                step["problems"].append(
                    f"exit {proc.returncode}, expected {want_exit}: {proc.stdout[:120]!r}")
            else:
                try:
                    doc = json.loads(proc.stdout)
                except json.JSONDecodeError:
                    step["problems"] = [f"not one JSON document: {proc.stdout[:120]!r}"]
                else:
                    step["problems"] = validate(doc, SCHEMAS[name])
            rows.append(step)
    finally:
        import shutil
        shutil.rmtree(work, ignore_errors=True)

    gaps = []
    unschemaed = sorted(published - set(SCHEMAS))
    gaps += [{"command": c, "gap": "published but no schema"} for c in unschemaed]
    unexercised = sorted(set(SCHEMAS) - exercised)
    gaps += [{"command": c, "gap": "schema but never exercised"} for c in unexercised]
    invalid = {}
    for step in rows:
        if step["problems"]:
            invalid.setdefault(step["command"], []).extend(step["problems"])
    gaps += [{"command": c, "gap": f"output invalid: {'; '.join(p[:3])}"[:300]}
             for c, p in sorted(invalid.items())]
    gaps += docs_gaps()

    coverage_ok = (not setup_failures and not unexercised
                   and len(exercised) >= MIN_COMMANDS)
    coverage_note = "; ".join(
        ([f"steps that did not run: {'; '.join(setup_failures)}"] if setup_failures else [])
        + ([f"never exercised: {', '.join(unexercised)}"] if unexercised else [])
        + ([f"only {len(exercised)} commands exercised, {MIN_COMMANDS} required"]
           if len(exercised) < MIN_COMMANDS else []))

    print(json.dumps({
        "gate": GATE,
        "value": len(gaps),
        "unit": "gaps",
        "note": (f"{len(gaps)} gap(s): {len(SCHEMAS)} schemas, "
                 f"{len(exercised)} commands exercised, "
                 f"{sum(1 for s in rows if not s['problems'])} outputs valid"),
        "detail": {"gaps": gaps, "steps": rows},
        "coverage": {"ok": coverage_ok, "note": coverage_note},
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
