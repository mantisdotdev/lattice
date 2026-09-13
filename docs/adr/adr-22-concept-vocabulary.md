# ADR-22 — The concept vocabulary, and the lint that closes it

**Status:** Accepted · **Required by:** §4.2 (seven concepts, CONSTRAINT), gate G2.3 · **Creates:** `harness/g2/g2_3_concept_lint.py` (frozen)
**Gates touched:** G2.3 (concept lint, HARD) — first harness

## Context

§4.2 caps user-facing nouns at seven, and the README turns it into a promise:
"no index, no HEAD, no detached anything." Until now nothing enforced it. The
revised spec names an "ADR-11" as the naming record; no such file was ever
written. This ADR is that record, under the number that is actually free.

An audit of every normal-path `--help` screen before this ADR found the
promise already broken in four small ways: `assign` said **working-tree**
(Git's name for the first concept), `verify` measured the repository "against
its own **hashes**", the global `--json` flag emitted "a stable JSON
**object**", and `assign --to`'s help said a mistyped id cannot **mint** a
change. All four are reworded in the same PR that lands the lint.

## Decision

**The lint is closed-world.** Every word on a normal-path help screen must be
one of: a spelling of the seven concepts; a banned word (Git and graph
vocabulary — one occurrence is one violation); or a member of a frozen list of
vetted plain English. A word in none of the three is a violation. A ban list
alone can never catch an eighth noun being coined; a closed vocabulary can.
The price is deliberate friction: any new help-text word fails G2.3 until it
is added, and the vocabulary lives inside the frozen harness file, so adding
one is `gauntlet freeze --refreeze G2.3` plus a note here, never a quiet edit.

**The vocabulary is inline in the harness, not a data file.** The §0.3 freeze
closure pins the entry script and its `harness/lib` imports; a sibling data
file would sit outside the freeze and be quietly editable — the exact hole the
closure exists to close. G2.4 embeds its categories the same way.

**`internals` is exempt, except its doorway.** Screens under `ltx internals`
may say chunk, pack, and Merkle — the person who typed `internals` asked for
the machinery. The one-line `internals` row in the root help is linted like
everything else, because the root screen is the first thing every user reads.

## Rulings

Words that could be mistaken for an eighth concept, vetted with reasons:

- **repository, file, path, directory** — physical containers, not
  working-model concepts.
- **history** — the collective of checkpoints, as "weather" is of days.
- **conflict** — per the §4.2 revision, a state a change or checkpoint *has*,
  never an object on a normal path. The word is vetted; the first-class
  object stays reachable under `ltx internals`.
- **address** — how a checkpoint is named from outside (ADR-8); an attribute,
  like a size.
- **forensic** — an adjective on `log`; describes a view, names nothing.

Banned outright, beyond Git's own surface nouns: graph words (node, edge,
ancestor, parent, …), storage words (hash, blob, object, tree, ref, oid,
Merkle), and the two §4.2 explicitly folded or struck: **changeset** (a change
not yet checkpointed) and **snapshot** (a checkpoint, coined differently).

## Consequences

- G2.3 exists and measures 0 violations over 26 screens at the commit that
  fixes the four wordings; the harness demonstrably fails against a shim
  binary that advertises `commit` and `stash` (`--ltx` exists for that check
  and nothing else). The frozen harness's own output, from the run behind
  that claim:

  <!-- evidence: output of `python3 harness/g2/g2_3_concept_lint.py` against the
  binary built at this ADR's PR (the run CodeRabbit asked to see beside the
  claim); the recorded measurement lands as a gauntlet iteration on the results
  branch once this merges, and G2.3's scorecard in GAUNTLET.md is the durable
  citation. -->
  ```json
  {
    "gate": "G2.3",
    "value": 0,
    "unit": "violations",
    "note": "0 violation(s) across 26 help screens; vocabulary: 16 concept forms, 190 vetted words, 66 banned",
    "detail": {
      "violations": [],
      "screens_walked": [
        "(root)", "assign", "change", "change list", "checkout", "init",
        "lens", "lens list", "lens use", "line", "line list", "log",
        "merge", "redact", "save", "split", "start", "status", "switch",
        "sync", "thin", "undo", "verify", "workspace", "workspace list",
        "workspace new"
      ]
    },
    "coverage": { "ok": true, "note": "" }
  }
  ```
- Runtime *output* vocabulary is not covered here. §5.1 deliberately puts
  "chunks" in `verify`'s mouth; a future revision owns that surface. G2.4
  already holds error documents to the seven concepts via its `concept` field.
- `ltx-core`'s internal rustdoc still says "working-tree paths" in three
  places. Implementation comments are not user-facing surface; left alone.
