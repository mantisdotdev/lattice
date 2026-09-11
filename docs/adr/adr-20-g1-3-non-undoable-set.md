# ADR-20 — G1.3 must enumerate the non-undoable set it was told to enumerate

**Status:** Accepted · **Required by:** §0.3 (harness changes after freezing) · **Amends:** the frozen `harness/g1/g1_3_universal_undo.py`
**Gates touched:** G1.3 (universal undo, HARD) — reverts to FAIL(stale) until re-measured under the amended harness

## Context

`docs/DISAGREEMENTS.md` Challenge 12 established that "every state-changing
command is undoable" is a promise that must be broken for exactly two
commands, and said how the gate would carry that:

> **Resolution:** ACCEPTED — undo is scoped precisely, and the exclusions are
> enumerated in the gate rather than left implicit.
> - **Redaction and thinning are explicitly non-undoable** …
> - **G1.3's harness enumerates the non-undoable set** and asserts that (a)
>   every command outside it is undoable, and (b) every command inside it
>   *refuses* …

The harness that was frozen does neither. It discovers the state-changing
surface from `ltx internals command-surface --json`, which publishes an
`undoable` field for every command, and never reads that field. It applies a
sequence, runs `ltx undo` until nothing is left, and requires the whole equality
domain — every working-tree byte, the checkpoint graph, lines, changes — to
equal what it was before. Every command is held to the same rule, including the
two the project decided must not obey it.

This was invisible while the non-undoable commands did not exist. Now two of
them do, and the third is blocked by it:

- `thin` and `internals compact` are recorded and not undoable, and they pass
  the frozen harness **by accident**: neither changes anything the equality
  domain holds, so undo-all converges without ever reversing them. The gate is
  not asserting what Challenge 12 said it would; it is failing to notice.
- `redact`, the command Challenge 12 was written for, cannot be built to pass
  the frozen harness at all. Its purpose is destroying content that must not
  come back. A sequence that redacts a path and then switches lines cannot
  converge under undo-all, because the bytes are gone and undo must not resurrect
  them. The only way to ship `redact` under the frozen gate would be sample
  arguments that never name a real path — a command that exists and is never
  exercised, which is the gaming §0.3 forbids.

So the frozen harness contradicts an accepted resolution, and the contradiction
now has a cost: the last of G1.3's twelve required verbs is the one it makes
impossible.

## Decision

Amend `harness/g1/g1_3_universal_undo.py` to implement Challenge 12 as written:

1. **Read `undoable` from the discovered surface.** The non-undoable set is
   whatever the binary publishes as `undoable: false` among its state-changing
   commands, not a list in the harness. A command that lies about itself is
   caught by (2).
2. **Assert refusal, not reversal, for the non-undoable set.** After undo-all,
   read `ltx internals oplog --json` and require that no `undo` entry names an
   entry whose operation is non-undoable. An engine that reverses a thinning or
   a redaction fails the gate; today nothing checks this.
3. **Exclude what a redaction destroyed from the working-tree comparison.**
   Every `redact` emission reports its target in `--json`; the paths it names
   are dropped from both the initial and the final snapshot before they are
   compared. Everything else in the domain is compared exactly as before.
   `redact`'s sample argument names the seed path the batch guarantees exists,
   so the exclusion is exercised on every draw rather than never.

## §0.3 classification, with evidence

Two changes, in opposite directions, and both are stated:

- (2) makes the measurement **stricter**: a new assertion that no non-undoable
  operation was reversed. The evidence that it can fail is that nothing today
  prevents an engine from appending an `undo` entry for a `thin`; the amended
  harness would report that, and the frozen one reports a clean pass.
- (3) makes the measurement **looser in letter and equivalent in intent**: bytes
  a redaction destroyed are no longer required to return. That is precisely the
  exclusion Challenge 12 accepted — "undoing a redaction would resurrect it" —
  and the frozen harness omitted it. The evidence is the resolution's own text,
  quoted above, and the current run's coverage note, which names `redact` as
  the required operation the surface omits.

Nothing else in the harness changes. Sequence generation, the undo budget, the
in-process property harness and the emission floor are untouched.

## Consequences

- G1.3 reverts to FAIL(stale) on amendment and is re-frozen at the new hash.
  The measurement taken under the frozen harness on this date stands in the
  record as what it was: a run that could not have exercised the exclusion.
- `redact` can now be built as the destructive operation it is, with the gate
  asserting it refuses undo rather than requiring it to comply.
- The harness gains its first dependence on the `undoable` field, so a command
  published as non-undoable that quietly is undoable becomes a gate failure —
  which is the honest direction for that field to be checked in.

## Not decided here

G1.1's coverage contract requires a fault hit in a `merge` critical section
that nothing in its frozen operation pool can produce (ADR-9, consequences).
That is a separate inconsistency in a separate harness and gets its own record.
