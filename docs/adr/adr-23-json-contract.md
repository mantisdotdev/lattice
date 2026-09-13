# ADR-23 — The JSON contract is closed, and documented under a gate

**Status:** Accepted · **Required by:** §5 (stable JSON for agents), gate G2.5 · **Creates:** `harness/g2/g2_5_json_contract.py` (frozen), `docs/json-contract.md`
**Gates touched:** G2.5 (JSON contract, HARD) — first harness

## Context

Every command already answers `--json` with one JSON document, and four
frozen harnesses (G1.1, G1.3, G1.4, G2.4) already parse those documents by
field name — the contract existed, unwritten and unenforced. Nothing stopped
a field from being renamed, which is exactly how G1.1 once broke: `verify`
nested its report under a key and every crash trial failed identically.

## Decision

**Success documents are closed.** Each command has a schema with
`additionalProperties: false`; an undeclared field is a gap, a missing one
too. A contract you can silently extend is a contract consumers cannot pin.

**The schemas live inline in the frozen harness.** Same reasoning as
ADR-22's vocabulary: the §0.3 freeze closure pins the entry script and its
`harness/lib` imports; a schema directory elsewhere would be quietly
editable. Adding a field is therefore `gauntlet freeze --refreeze G2.5`
plus a note here.

**`docs/json-contract.md` is the measured artifact.** The harness requires a
`## <command>` section per schema naming every top-level field in backticks;
documentation that drifts from the shape is a gap. The docs explain, the
schema enforces, the gate ties them together.

**The error document is closed at five fields** — `ok`, `error`, `category`,
`concept`, `recovery` — the shape G2.4 judges semantically; G2.5 pins it
structurally, with the six causal categories and seven concepts as enums.

**Codified as-is, not redesigned:**

- List documents (`line list`, `change list`, `workspace list`,
  `lens list`) carry `version: 1` and are invariant under undo-all; act
  documents carry `oplog_seq` instead and are not versioned. The asymmetry
  is meaningful — a listing is an interchange shape, an act document
  describes one operation — so it stays.
- `undo` reports its position twice, as `undo_seq` and `oplog_seq`, one
  value under a historical and a uniform name. Removing either breaks a
  parser for zero benefit; both are in the schema.
- Machine field names use storage vocabulary (`tree`, `chunks_verified`)
  where §5.1 specifies it. §4.2's seven-noun cap governs the human surface
  (G2.3); the machine surface optimizes for precision.

## Consequences

- G2.5 exists: 22 schemas, one scripted session exercising every
  normal-path command's success path plus one provoked error, validated by
  a stdlib subset validator (bare CI runners have no jsonschema). Measures
  0 gaps at this commit; demonstrably reports 22 against a shim that
  answers `{"ok": true}` to everything. The frozen harness's own output,
  from the run behind that claim:

  <!-- evidence: output of `python3 harness/g2/g2_5_json_contract.py` against the
  binary built at this ADR's PR, detail.steps elided for length; the recorded
  measurement lands as a gauntlet iteration on the results branch once this
  merges, and G2.5's scorecard in GAUNTLET.md is the durable citation. -->
  {
    "gate": "G2.5",
    "value": 0,
    "unit": "gaps",
    "note": "0 gap(s): 22 schemas, 22 commands exercised, 23 outputs valid",
    "detail": {
      "gaps": [],
      "steps": "23 steps, all problem-free (elided; rerun the harness for the full list)"
    },
    "coverage": {
      "ok": true,
      "note": ""
    }
  }
- The scenario pins today's `sync` semantics: `--dry-run` with no remote
  configured succeeds with `remote: null`. When remotes ship, the schema
  and scenario change with them, through a refreeze.
- Growth in the surface (a new command) fails G2.5 twice — no schema, no
  docs section — until the contract is extended deliberately.
