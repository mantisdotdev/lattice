# ADR-16 — Lines, and undo generalised to an op-log dispatcher

**Status:** Accepted · **Amends:** ADR-15 (§3, the `log --forensic` view)
**Gates:** G1.3 (universal undo, HARD), G1.1 (crash safety, HARD), G1.7 (switch latency, SOFT)

## Context

`ltx start <name>` and `ltx switch <name>` are two of the twelve operations G1.3
requires. §6 forbids omitting a command from the published surface to dodge a
gate, so adding them makes them state-changing — and G1.3 then emits them in
random batches and requires **undo-all to return the user-visible state to the
post-seed snapshot**, with `ltx line list --json` inside its equality domain.

Undo today (ADR-15) walks the checkpoint parent chain. That cannot reverse a
`start` or a `switch`. So undo must generalise — while still converging to
`nothing_to_undo`, because an undo that oscillates or fails to terminate fails
the gate.

## Decision

### 1. A line is a named pointer plus a preserved working state

```
LineState  { current: String, lines: BTreeMap<String, LineRecord> }
LineRecord { tip: Option<String>,      // checkpoint id
             working: Option<String> } // preserved working tree address
```

**Linchpin invariant:** `lines[current].working` is always `None`. Becoming
current *consumes* the preserved state; ceasing to be current *sets* it. While a
line is current, the bytes on disk are the truth. Every inverse below is exact
and mechanical only because of this.

`tip` holds a checkpoint **id**, not an op-log seq: undo moves a tip to
`checkpoint.parent`, which has an id but no unambiguous seq.

### 2. `main` is created by `init`, below the undo floor

G1.1 and G1.4 both run `switch main` against a repository whose entire setup is
`init` + `save seed`, with no `ltx start main` anywhere — so `main` must exist by
default. It must **not** be created by a `StartLine` entry: that entry would be
an eligible undo target and undo-all would delete `main`. `init` writes the
initial `LineState` in the *same redb transaction* as `Operation::Init`, which is
not undoable, so `main` sits below the floor.

### 3. `start` always switches; `switch` never refuses a dirty tree

`ltx start <name>` postcondition is uniform — *the line exists and is current* —
whether or not it existed. `start <existing>` exits 0 and records
`StartLine{name, from, created: false}`, behaving exactly as a switch (G1.4 draws
`start line` thousands of times against one repo with a target of 0 failures).

`ltx switch <name>` refuses only an unknown or invalid name. There is **no
dirty-tree refusal** — §4.3 forbids Git's "your local changes would be
overwritten" trap, and state is preserved per line.

### 4. Undo becomes a dispatcher over the op-log, and provably terminates

Undo selects the highest-seq **live, eligible** entry and applies its
kind-specific inverse. The parent-chain walk survives verbatim as the `Save` arm.

```
undone(L)    = { e.undone_seq : e in L, e.operation = Undo{undone_seq} }
live(L)      = { e in L : e.operation.is_undoable() and e.seq not in undone(L) }
eligible(e)  = Save{checkpoint,..} => checkpoint.parent.is_some()   // the root floor
               StartLine{..}       => true
               Switch{..}          => true
target(L)    = argmax_seq { e in live(L) : eligible(e) }
```

**Inverses** (exhaustive match — a new variant without an arm is a compile error,
which is how §6 is mechanised):

| Operation | Inverse |
|---|---|
| `Save{checkpoint, line}` | `lines[line].tip = checkpoint.parent`; working tree untouched (save never mutates it) |
| `StartLine{name, from, created:true}` | remove `lines[name]`; `current = from`; materialise `lines[from].working`; consume it |
| `StartLine{.., created:false}` | identical to the Switch inverse (it *was* a switch) |
| `Switch{from, to}` | `lines[to].working = capture(now)`; `current = from`; materialise `lines[from].working`; consume it |

Three load-bearing invariants:

- **(I1) Scan candidates, not entries.** After any undo the newest entry is an
  `Undo`, so "reverse the last operation" must mean "the last *live candidate*".
- **(I2) LIFO over the live set.** Selection always takes the maximum, so when
  entry *s* is reversed, every live entry above it is already reversed and the
  repository state equals the state immediately after *s* was applied. This is
  what licenses local inverses with no conflict analysis — and it is exactly what
  would break if selective or out-of-order undo were added.
- **(I3) Eligibility depends only on immutable data** — the candidate's own
  operation fields and its own checkpoint's `parent` — never on the mutable
  head. Today's code tests the *mutable* head's parent; this strengthens it.

**Termination.** Let `mu(L) = |{ e in live(L) : eligible(e) }|`. A successful undo
appends exactly one `Undo` (not a candidate, so the candidate set never grows) and
adds exactly one seq to `undone(L)`, removing exactly one element from `live(L)`;
by (I3) no member's eligibility can be restored. So `mu` strictly decreases by one
per undo, undo-all terminates in exactly `mu` steps, and `nothing_to_undo` is an
absorbing fixed point. Undo never appends a forward operation, so it cannot
oscillate — preserving ADR-15's convergence guarantee.

### 5. `ltx line list --json` — the exact compared document

G1.3 compares the whole parsed document with `!=`, so every field must be
invariant under (apply-batch + undo-all):

```json
{"ok": true, "version": 1, "current": "main",
 "lines": [{"name": "main", "checkpoint": "<64-hex or null>"}]}
```

`lines` sorted by name (BTreeMap order — content-derived, not creation order).
**Excluded, deliberately:** `working`/autosnapshot addresses (the ephemeral tier
the gate explicitly excludes), any timestamp (undo cannot restore a clock),
per-line `oplog_seq` (renumbered by future compaction), and any count over all
saves (never returns to the seed value).

### 6. Switch is data-safe by construction

**Rule: no materialisation ever happens without the current working tree first
being captured into the pack store as durable content** — on the forward path and
inside undo.

`switch`: self-target short-circuits (append the entry, touch no file — G1.4 draws
`switch main` while already on `main` thousands of times); unknown name refuses
before appending; otherwise capture → publish (entry + `LineState` in **one**
transaction) → materialise. Publish-before-materialise is deliberate: a crash
leaves you unambiguously on the target with a re-runnable idempotent
materialisation, and no work is lost because the capture is durable first. If the
captured tree equals the target tree, materialisation is skipped entirely.

`start` of a **new** name captures, publishes, and does **not** materialise — the
new line inherits both the tip and the on-disk bytes. Inheriting the tip is forced:
G1.1 asserts no checkpoint from its baseline is lost after a crash on a pool that
includes `start`, and a line starting with no tip would empty that array.

**Reconciling materialiser.** `restore_tree` is purely additive: it never removes
a path absent from the target tree. Switching would leave the previous line's
files behind, so the working tree becomes the union of both lines and switch stops
being involutive. Switch therefore uses a *reconciling* materialiser that also
deletes what the target does not name (skipping `.lattice`), reusing the existing
collision / CWE-59 / unsafe-component protections. `checkout --into <user path>`
stays additive — it writes into a destination the user named. Two functions over
one walker; no boolean parameter.

### 7. Storage: one table, one key, one transaction per command

`LINES: TableDefinition<&str, &[u8]>` holding the single key `"state"` →
serialised `LineState`. One key because a publish mutates three facts at once
(source `working`, target `working`, `current`) and must be atomic — and because
user-controlled line names then never become redb keys.

`OpLog::commit(operation, writes)` replaces `append` + `set_head` as the single
durable write path, so the entry and the `LineState` land together or not at all.
This halves save's fsyncs and dissolves the crash window that would otherwise make
the "already undone" set ambiguous.

**Head resolution ladder**, most durable first, preserving the doctrine fixed in
PR #4: (1) `LINES["state"] → current → tip` — authoritative; (2)
`HEADS["checkpoint"]` — read-only legacy rung; (3) `.lattice/HEAD` — human-readable
cache, only ever equal-or-staler. `set_head` is retired as a *write*.

### 8. Amendment to ADR-15 §3: `log --forensic` is the union over line tips

ADR-15 made `log`'s `checkpoints` the current line's reachable chain. With lines,
that would make an ordinary `switch` look like checkpoint loss to G1.1, which
asserts no baseline checkpoint disappears. `--forensic` therefore reports the
**union of the chains reachable from every live line tip**, which makes §2(c)'s
"no checkpointed data is ever lost" true by construction in the array the HARD
gate actually reads. `history()` (the default view) stays the current line's chain.

### 9. On-disk format break, taken now

Adding `line` to `Save` and `from`/`created` to `StartLine` changes what
`Entry::compute_id` re-serialises, so every pre-existing entry would be reported
"altered" by `verify_chain` and fail G1.1. A `skip_serializing_if` workaround would
permanently encode a sentinel meaning "an old save whose line is unknown" — a
second representation of one fact. So: a clean break, `HEADS["format"] = 2` written
by `init`, and `Repo::open` refusing an older database with a real recovery action.
No released repository exists; development repositories must be recreated. **This
window closes permanently once `start` ships and a real `StartLine` entry exists.**

## Consequences

- Ships: `init` creating `main`; `start`/`switch` with per-line preserved working
  state; `ltx line list`; general op-log-driven undo, floored at the root save and
  proven terminating; `oplog_seq` on every state-changing command; the reconciling
  materialiser.
- **Gates still not passing, stated plainly.** G1.3 remains a coverage FAIL: this
  slice removes 2 of the 10 missing required operations, leaving 8. What it buys is
  `failures == 0` with the `lines` domain *live* rather than vacuously equal. G1.1
  still needs `sync`/`internals compact`/`internals thin`; G1.4 needs
  `workspace new` and true cross-process concurrency; G1.7 needs `adopt`, a daemon,
  and per-call costs that are currently O(history × store).
- Deferred: workspaces and the (workspace, line) key widening; the ephemeral tier
  (ADR-10) that will own preserved trees; `adopt`; the daemon; redo; `assign`,
  `split`, `sync`, `redact`, `thin`, `lens`, `merge`, `workspace`.

## Open conflicts recorded, not resolved

1. **G1.4 vs ADR-15 §2.** G1.4 requires every successful command's `--json` to
   carry a strictly-increasing `oplog_seq`, while ADR-15 specifies
   `nothing_to_undo` as "exit 0, no op appended" — so two non-overlapping no-op
   undos report the same position. This slice emits `oplog_seq` everywhere and
   leaves the no-op case to the concurrency slice. Resolutions all have costs
   (append a no-op entry; allocate tickets, which breaks across processes; or amend
   a frozen expectation).
2. **Undo scope.** Global reverse-chronological undo is what G1.3 requires. Under
   G1.4's 8 concurrent workspaces one workspace's undo would reverse another's
   operation, and the (I2) LIFO lemma does not hold under concurrency. Must be
   decided before `workspace new`.
3. `Operation::Adopt` is `is_undoable()` with no inverse. It is unreachable today
   (no `adopt` command), so the dispatcher treats it as ineligible; giving it an
   inverse — or moving it into the non-undoable set ADR-15 records as frozen — is an
   ADR-level decision for the `adopt` slice.
4. Challenge 12's "a non-undoable command must REFUSE undo" versus G1.3 breaking
   its undo-all loop on a non-zero exit. Bites when `redact`/`thin` enter the
   surface; candidate is to skip but *name* them in the JSON.
5. G1.3 requires a surface command whose first word is `thin`; G1.4 invokes
   `internals thin`. Both harnesses are frozen.
