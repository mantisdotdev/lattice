# ADR-15 — The undo model: local causal undo, floored at the root checkpoint

**Status:** Accepted · **Gates:** G1.3 (universal undo, HARD), G1.1 (crash safety, HARD)
**Requires:** DISAGREEMENTS.md Challenge 12 (undo scope), Challenge 8 (sync retraction)

## Context

`ltx undo` is a HARD gate (G1.3). The gate's harness does `init`, writes a file,
`save "seed"`, snapshots the user-visible state as `initial`, applies 1–12 random
state-changing commands, then calls `ltx undo` in a loop until it reports
`{"nothing_to_undo": true}`, and asserts the final user-visible state **equals
`initial`** — the post-seed state.

That last requirement forces the model. A naive "undo reverses every operation
back to genesis" would walk past the seed save to an empty repository, and
`final != initial`. So undo's baseline is not a free choice; the frozen harness
determines it.

The equality domain — stated in the gate and the harness — is **working-tree
bytes, checkpoint graph, lines, changesets**, explicitly **excluding the op-log
and ephemeral autosnapshots**. Comparing the op-log would make the gate
unsatisfiable, because undo is itself an operation the op-log must record.

## Decision

### 1. Undo is local causal undo, and it appends

`ltx undo` reverses the local, user-visible effect of exactly **one** operation
and records the reversal by **appending** `Operation::Undo { undone_seq }` to the
op-log — never by truncating. Checkpoints are immutable and are never deleted by
undo (§2c: no checkpointed data is ever lost); an undone checkpoint simply leaves
the reachable-from-HEAD graph, which is what makes it redoable later.

For the operations that exist today:

- **save** is undone by moving the current checkpoint head to the saved
  checkpoint's `parent` (the durable op-log head via `set_head`, then the `HEAD`
  file, in save's own write order), and appending `Operation::Undo`. `save` never
  mutates the working tree, so undo needs no working-tree restoration here — it is
  pure head movement plus the appended record.
- **init** is the non-undoable **floor**. It establishes the repository container
  and creates no checkpoint, so it produces no undoable user-visible state.

### 2. `nothing_to_undo` fires at the root checkpoint

`ltx undo` reports `{"nothing_to_undo": true}` (exit 0, **no** op appended) exactly
when the current head checkpoint has no `parent` (it is the root) or when nothing
has been saved. Undo-all therefore converges monotonically to the root and stops
there. Because the harness performs exactly one save before snapshotting
`initial`, the root *is* that baseline, and `final == initial` holds.

This is a deliberate deviation from jj, whose undo can walk back to repository
init. Lattice floors undo at the root checkpoint. It is recorded here because it
is implied by the frozen harness and by nothing else.

### 3. `log --forensic --json` returns reachable checkpoint state, not the op-log

The harness compares the whole object from `ltx log --forensic --json`, so it must
be a pure function of user-visible state, invariant under (apply-batch +
undo-all). Two changes to the JSON output of `log`:

- **Drop the `operations` array.** It is the raw op-log, which grows on every
  command (including undo) and can never return to its post-seed value — and it is
  explicitly outside the equality domain. G1.1 reads only `checkpoints`, so this
  is safe there. The raw op-log moves to `ltx internals oplog --json` (plumbing,
  §4.2), and `log`'s human text rendering may still print operations.
- **`checkpoints` becomes the reachable set.** A new `reachable_checkpoints()`
  walks `parent` from the head checkpoint to the root, newest first. Undo moves
  HEAD to the parent, so an undone mid-batch save becomes unreachable and drops
  out; undo-all restores exactly `[seed]`. `checkpoints()` (every authentic
  Save-referenced checkpoint) is unchanged and still backs `verify` and `status`,
  which must see every saved checkpoint.

Each checkpoint entry keeps its immutable fields (id, tree, message, parent,
at_unix_ms, oplog_seq); `oplog_seq` is resolved at read time from the Save entry's
seq, and because undo appends rather than renumbering, the seed's seq is stable,
so the seed entry is byte-identical across the two snapshots. No volatile
per-entry "is-current" marker is added — it would move under undo and break G1.1's
membership check.

### 4. Peripheral decisions (defaults; revisit as noted)

- **Raw op-log home:** `ltx internals oplog --json`. The `log` text output may keep
  showing operations.
- **`init` in the command surface:** `state_changing: false`. Init creates the
  container, not undoable user-visible state, so it stays out of the
  undoable/non-undoable dichotomy (whose non-undoable set is frozen as
  {redact, thin}).
- **Redo-branch policy** (a new mutating command after an undo — truncate the redo
  branch, or preserve it) and **per-command undo-step fan-out** (whether a heavy
  command like merge may decompose into more undo steps than the harness's
  `applied*3+8` budget allows): **deferred.** Neither is exercised by init/save;
  both must be fixed before the general command surface (start, switch, …, merge)
  lands, and the fan-out question may be a harness/spec decision.

## Undo is monotonic; it is not itself undoable yet

Undo walks toward the root and stops there, so undo-all converges to
`nothing_to_undo` — which G1.3 requires. Reversing an undo would be a forward
"redo"; under repeated `ltx undo` that oscillates and never reaches
`nothing_to_undo`, so it cannot be what `ltx undo` does. `Operation::Undo` is
therefore marked **not undoable** (`is_undoable` is false, and the command
surface lists `undo` as `undoable: false`). This deliberately narrows §4.3's
literal "undo itself is undoable via `ltx undo`" to keep G1.3 satisfiable; redo,
if it lands, is a separate forward command, deferred.

## Consequences

- This slice makes the machinery correct and the query surfaces invariant, and the
  init/save/undo path passes with **0 failures**. It does **not** by itself clear
  G1.3's coverage axis: `coverage.ok` stays false until the discovered surface
  contains every one of the twelve `REQUIRED_OPERATIONS` and each is emitted ≥100
  times. G1.3 cannot fully pass until all twelve operations and their inverses
  exist. That is stated plainly rather than hidden.
- Evidence for the "0 failures" claim — the `undo-property` harness at the gate's
  seed, on the reference machine:

  ```
  $ ./target/release/undo-property --sequences 1000 --seed 20260903 --json
  {"emitted":{"save":4403,"undo":2159},"failures":0,"seed":20260903,"sequences":1000}
  ```

  The full ≥100,000-sequence run is performed by the G1.3 harness on the
  measurement machine (on tmpfs, where the throwaway repos cost no fsync).
- The general undo surface (start, switch, assign, split, sync, redact, thin,
  lens, merge, workspace), redo-branch truncation, working-tree restoration for
  tree-mutating undo, and op-log compaction (ADR-13) are tracked follow-ups.
