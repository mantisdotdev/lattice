# ADR-17 — Changes, assignment, and partial save

**Status:** Accepted · **Amends:** ADR-16 (§7, the publish payload; §9, the format break)
**Deviates from:** SPEC-REVISED §4.2's literal spelling of `ltx save <change>` — see §5
**Gates:** G1.3 (universal undo, HARD), G1.4 (concurrency, HARD), G1.1 (crash safety, HARD), G2.4 (error recovery, HARD)

## Context

`assign` is the fifth of G1.3's twelve required operations, and the first that
adds a **noun** rather than a verb over existing state. SPEC-REVISED §4.2 fixes
what that noun is:

> Therefore `ltx assign` assigns hunks or entities to a *change*, and
> `ltx save <change>` checkpoints one.

A change is §4.2's "a logical unit of work with a stable ID" — one of the seven
permitted user-facing nouns, and the thing that replaces the index without
reintroducing a staging gate.

Three frozen harnesses constrain this more than any design argument does, and
they were read before anything was designed:

- **`harness/g1/g1_4_concurrency.py:45`** draws `["assign", "."]` — a bare
  path, no change named — and `:66` counts any non-zero exit as a failure,
  across 8 workspaces x 10,000 operations against a target of 0.
- **`harness/g1/g1_3_universal_undo.py:143`** places `query("change", "list")`
  inside the equality domain that undo-all is compared against.
- **`harness/g1/g1_1_crash_safety.py:77`** and **`g1_4:44`** pass multi-word
  positional *messages* to `save` (`"trial checkpoint"`, `"concurrent edit"`)
  and require exit 0.

All three match their `harness/FREEZE.json` hashes. §0.3 makes them
unamendable, so they are inputs to this decision, not consequences of it.

## Decision

### 1. `assign` routes uncheckpointed working state into a change

`docs/prior-art/sapling.md` §3.5 says `ltx assign` inherits `sl absorb`'s
contract — mapping changed lines onto the checkpoint in an existing stack that
last touched them. That is retroactive history editing, and it is a different
operation from the one SPEC-REVISED describes. **The spec reading governs**, on
three independent grounds:

- SPEC-REVISED declares itself authoritative ("This is the authoritative
  specification") and states the staging reading in the same sentence that ties
  `assign` to `save <change>`. The prior-art doc is the G0.1 deliverable, and
  `harness/g0/g0_1_prior_art.py` measures it for *structure* only — section
  headings, word counts, source counts. No sentence in any prior-art §3 is a
  requirement in `harness/gates.toml` or `FREEZE.json`.
- §3.5 delegates its binding force to "ADR-014", a number since taken by an
  unrelated accepted decision (`adr-14-g1-2-case-folding-coverage.md`). Nothing
  in this repository currently records absorb's invariants. This ADR is where
  they land.
- Decisively: under the absorb reading, G1.4 would measure a command that can
  never act. Its worker writes a **brand-new file** before every operation, so
  no checkpoint has ever touched that content and absorb would have nothing to
  attribute it to — a no-op on all ~10,000 draws. A command measured ten
  thousand times that cannot do anything is what §0.3 exists to forbid.

Retroactive assignment is **deferred, not rejected**. It returns as a widening
of this verb's target set, never as a second verb beside it, and the v1 refusal
is worded as a scope limit so that widening reads as a continuation:

> target change `7f3a` is already checkpointed; retroactive assignment is not
> in v1

The line-provenance engine it needs is owed to G3.1 (`ltx trace`) and G4.1
regardless, so deferring it costs machinery we are not building twice.

### 2. What survives from absorb

Of absorb's two stated properties, exactly one still means something here, and
it is the one that matters:

| Property | Under this ADR |
|---|---|
| **Never writes to working state** | **Invariant.** `assign` records an intent; it does not touch a byte on disk. Nor does its inverse. This is what keeps ADR-16 §1's linchpin true — while a line is current, the bytes on disk are the truth — and it is why assign's inverse needs no capture and no materialisation. |
| **Never conflicts** (line provenance, not 3-way merge) | Vacuous with an explicit target, and retained as a **design constraint on any future auto-routing form**: route by provenance, never merge. `assign .` with no change named routes to the current change, which is selection, not merge. |
| **Refusal semantics** (ambiguous is left untouched and *reported*) | **Retained, with the trigger restated.** Not provenance ambiguity but applicability: a path that no longer exists, an unreadable path, a target outside the working state. Refusals are reported, never silent — and per §6 they exit 0. |

One hazard is inherited from the index that absorb does not have, and it is
recorded rather than solved: a change assembled by assignment can produce a
tree **that never existed on disk**, and so was never built or tested. This is
a known `git add -p` property, not a new defect, but a document claiming to
replace `add -p` may not be silent about it.

### 3. A change's identity is opaque and machine-minted

128 random bits, rendered as 32 lowercase hex, displayed and typed as the
shortest unique prefix (minimum 4 characters). Entropy is injected at the
`Repo` boundary, never reached for inside the engine, so the id source is a
counter in tests.

The alternatives and why they lose:

- **Content-addressed** — self-defeating: the id would change on every assign,
  which is the opposite of stable.
- **Sequential** — best to type, and free to allocate, but a change id is
  recorded in immutable history as the identity of a unit of work and travels
  with a checkpoint over sync. `change 3` here and `change 3` there would be
  different work under one identity.
- **User-coined name** — the tempting one, and the reason it loses is *time*,
  not collision. A line may be renamed and reused freely because a line is a
  mutable pointer. A change is an identity: reusing `fix` next month puts two
  unrelated units of work under one name, permanently, with no safe automatic
  repair — and it corrupts exactly the lineage G3.1's
  `ltx trace --agent --unreviewed --touching src/auth/` is built to answer.

The ergonomic cost is smaller than it looks, because §6 forces a *current
change*: the common path never types an id at all. An id is typed only when
several changes are open at once — which is precisely when they need to be
distinct. Human-readable **labels** beside the identity (the way a container
name sits beside a container id) are a deliberate later thickening if the
ergonomics prove painful, not a layer built for a need that has not appeared.

### 4. Changes live in the line record, not a new table

`LineRecord` gains two fields; `LINES` keeps its single key.

```rust
pub struct ChangeRecord {
    /// Working-tree paths assigned to this change, relative to the root, as
    /// raw bytes — the same doctrine tree entry names follow.
    pub assigned: BTreeSet<Vec<u8>>,
}

pub struct LineRecord {
    pub tip: Option<String>,
    pub working: Option<String>,
    /// Live, un-checkpointed changes on this line. BTreeMap so the serialised
    /// form and `change list` are canonical rather than creation-ordered.
    pub changes: BTreeMap<String, ChangeRecord>,
    /// The change a bare `ltx assign` adds to. None until the first assign.
    pub current_change: Option<String>,
}
```

A separate `CHANGES` table was the obvious move and is wrong for the reason
ADR-16 §7 gives for `LINES` having one key: a switch mutates the source line's
preserved state, the target's, and `current` at once, and must be atomic by
construction. Assignments are a selection over working-tree bytes and belong to
that same atomic unit. Putting them in a second table adds a second write for
`publish_lines` to keep in step and a new way for line state and change state
to disagree after a crash.

**Per-line, not repository-global**, and this is not a close call.
Assignments name paths whose bytes exist only in the current line's working
tree; `switch_to` replaces those bytes wholesale. A global change set would,
immediately after a switch, name paths that hold another line's content —
dangling by construction, and the shape in which `save <change>` would
checkpoint bytes the user never assigned. Living in `LineRecord` means
assignments are preserved and restored with `working`, in the same atomic
publish, at no extra cost.

`start_line` copies `changes` and `current_change` to the new line, for the
same reason it inherits the on-disk bytes: work that is still there but whose
labelling silently vanished is a loss of user intent.

**Bound, per "nothing from outside is unbounded":** assigned paths per change
are capped, and the cap is reported when hit. When hunk-level assignment lands,
assignment sets move to content-addressed blobs referenced by address — the
pattern `working` already uses — rather than growing this key.

### 5. One `save`, with a scope flag

```
ltx save "<message>"                     # the whole working state (unchanged)
ltx save "<message>" --change <id>       # only that change's assigned paths
```

SPEC-REVISED §4.2 spells this `ltx save <change>`. **That spelling is
unshippable** and the deviation is recorded here: `g1_1_crash_safety.py:77`
and `g1_4_concurrency.py:44` pass `"trial checkpoint"` and `"concurrent edit"`
as positional arguments to `save` and require exit 0. A positional that means
"a change if it resolves, else a message" is two readings of one argument and
would change meaning the day a change id resembles a message. The harnesses
measure behaviour and are frozen; the spec sentence specifies spelling. The
behaviour it specifies — checkpointing one change — is preserved exactly.

This stays **one command**: `--change` narrows scope the way `--limit` narrows
`log`. It is not a boolean that switches algorithms, and there is no second
verb, so "one way to do each thing" holds.

**Plain `save` never becomes implicitly partial.** "Save the current change if
one exists" was considered and rejected: the same command would mean different
things depending on whether an assign happened an hour ago, and the failure
mode is a user believing their work is durable when only part of it is.
Assignment is a *labelling*, never a *gate* — which is what keeps §4.3's "there
is no staging area to pass through" true.

**A partial save still writes a complete tree**: the parent's tree with each
assigned path replaced by its current working content, and removed where an
assigned path no longer exists on disk. Nothing downstream of `Checkpoint.tree`
— checkout, restore_tree, verify_tree, the parent walks — learns anything about
changes. What becomes false is only *the tip tree equals the working tree*,
which §8 enumerates.

`save --change` **captures the full working tree** into the store as well. It
already walks the whole tree to compute the diff, so the unassigned remainder
becomes content-addressed at no extra cost, rather than having no durable
address until the next switch.

`save --change` on a change with no assignments **refuses**, appending nothing.
The batch can reach it via `assign c f; undo; save --change c`.

### 6. The command surface the frozen harnesses require

```
ltx assign <path>...                 # to the current change, creating one if absent
ltx assign --to <id> <path>...       # to a named existing change
ltx change list                      # every live change on the current line
```

Forced, point by point:

- **`assign .` must work with no change named** (`g1_4:45`), so a *current
  change* exists, held per line. A bare assign creates one only if there is
  none — ten thousand bare assigns must not create ten thousand changes.
- **`--to <id>` never creates.** An unknown id is an error, so there is exactly
  one creation path.
- **Refusal exits 0** with a `refused` array in `--json`. `g1_4:66` counts a
  non-zero exit as a failure over ~10,000 draws. This overrides
  `sapling.md` §3.5's "refuses (loudly, with the reason)" as to *mechanism*;
  loudly, in JSON, at exit 0.
- **`ltx change list` ships in this slice, not after it.** Until it exists,
  G1.3's `changes` domain compares `{"error": ...}` to `{"error": ...}` and is
  vacuously equal — the gate would bless an undo that loses every change.
  Shipping `assign` without it would leave that hole open while appearing to
  close it. It exits 0 with an empty list in a repository with no changes, and
  its document carries no timestamp, no counter and no `oplog_seq`, so it is
  invariant under apply-batch-then-undo-all.
- **`sample_args` for `assign` is `["seed.txt"]`** — the one path the harness
  guarantees exists. The emission counter increments before the return code is
  checked, so args naming a nonexistent path would satisfy the >=100-emission
  coverage bar while testing nothing.
- **One command appends exactly one op-log entry.** ADR-15 §4 deferred
  per-command undo-step fan-out "before the general command surface lands";
  this is where it lands. The harness's undo budget is `applied * 3 + 8`, so an
  assign over *k* paths decomposing into *k* entries would exhaust it. A
  multi-path assign carries its paths *inside* one entry.

### 7. The op-log entries, and what their inverses need

```rust
Assign {
    change: String,
    line: String,
    /// The paths this call actually moved — not the paths named on the command
    /// line — so the inverse is exact and mechanical.
    paths: Vec<Vec<u8>>,
    /// Whether this call created the change. Same role as StartLine::created:
    /// without it, `assign c f; assign c g; undo` would delete a change the
    /// FIRST assign created.
    created: bool,
    /// The change that was current before, so the inverse restores it.
    from_current: Option<String>,
    /// For each path, the change that owned it before, if any. Without this,
    /// undoing `assign --to c2 f` after `assign --to c1 f` would leave `f`
    /// unowned rather than owned by c1 — the same class of bug that
    /// StartLine::created exists to prevent, one level down.
    displaced: Vec<(Vec<u8>, Option<String>)>,
},
Save {
    message: String,
    checkpoint: String,
    line: String,
    /// The change this save consumed and the assignment set it took, so the
    /// inverse can restore it exactly. None for a whole-working-state save.
    change: Option<CheckpointedChange>,
},
```

`Checkpoint` gains **no stored field**. Adding `change` to the blob would change
`body_id` and re-address every checkpoint in every repository, and the
association is provenance recorded by an operation, not content. It therefore
follows `oplog_seq` exactly: a read-time back-reference resolved from the `Save`
entry, with the stored blob always carrying `None`.

Two rules the inverses must hold, both of which are how a working undo becomes
a broken one:

- **Undo appends exactly one `Undo` and no forward operation.** The tempting
  implementation of "restore the previous owner" is to re-run `assign`, which
  appends a forward entry *and* an `Undo`: `|live|` stays constant, ADR-16 §4's
  termination measure dies, and `ltx undo` oscillates forever without reaching
  `nothing_to_undo`. The new change state is published in the *same*
  `oplog.commit` transaction that records the undo.
- **Nothing but `assign`, `save --change`, and their inverses may mutate change
  state.** After a switch, assignments naming replaced paths look like garbage,
  and pruning them is tempting. But `Switch{from, to}` does not record what it
  pruned, so its inverse could not restore it and undo-all would fail the
  `changes` domain. If a full `save` must consume assignments, it records them
  in its own entry, exactly as it now records `line`.

### 8. Eligibility, and the hole at the root floor

`assign`'s inverse touches no file, so it does not care whether the assigned
path was since edited, deleted, or replaced by a directory. It therefore needs
neither of the two existing guards — there is no floor to fall through and no
checkpoint to orphan.

It needs a different one, and this is the sharpest finding of the design
review. `next_undo_target` **keeps scanning past an ineligible entry**. A `save`
at the root of its line is permanently ineligible (no parent to return to).
So:

```
ltx init                     # no seed save
printf 'A\n' > a.txt
ltx assign a.txt             # seq 2
ltx save "x" --change 7f3a   # seq 3 -> C1, parent = None: the root floor
ltx undo                     # skips seq 3 (floored), reverses seq 2
```

The assign is reversed **underneath a standing checkpoint that consumed it**.
The change loses its assignments while the checkpoint derived from them remains
the line tip. This is ADR-16 §4's (I3) failure mode reappearing one level up:
the LIFO argument accounts for entries that were *undone*, and a **floor**
removes an entry from the eligible set just as effectively.

G1.3 does not catch it — the harness always seeds a save first, so every later
checkpoint has a parent. Users hit it on their first repository.

**Rule:** an `Assign` is ineligible while a standing `Save` has consumed its
change. `ltx undo` then reports `nothing_to_undo` with the assignment still
standing, which is honest only if `change list` reports that change as
*consumed* rather than *pending* — so it does.

The rejected alternative was splitting `Save`'s eligibility per effect (tip
floored, change not), which would produce a change that is simultaneously
checkpointed and pending — exactly the ambiguity SPEC-REVISED §4.2's noun
collapse removes.

**Eligibility is exactly the precondition of the inverse, never wider.** An
entry that is eligible but whose `reverse` errors makes `ltx undo` exit
non-zero, and the harness's undo loop *breaks* on that — every remaining
operation stays applied and the comparison fails. That is not a spin; it is a
silent truncation of undo-all, which is worse.

### 9. Format version 3, and the last forced break

`FORMAT_VERSION` goes 2 -> 3. It must: `Operation::Save` gains a field, so
`Entry::compute_id` re-serialises differently and `verify_chain` would report
every pre-existing entry as altered — phantom tampering, an automatic G1.1
failure. `Operation::Assign` is a new variant a v2 build cannot parse at all.

ADR-16 §9 wrote that the free-break window "closes permanently once `start`
ships". `start` has shipped, so this break is not free by that ADR's own terms.
An in-place migration is impossible by construction: rewriting entries to the
new shape changes their ids and breaks the Merkle chain, which is the chain's
entire purpose.

**So the break also introduces a per-entry format tag.** Each entry records the
format that wrote it, and `compute_id` dispatches on it, reproducing a
historical serialisation exactly. This costs one integer per entry and one
match arm, and it can only be introduced *at* a break. It makes this the last
break that forces recreating a repository: every future one becomes a
migration.

## Consequences

- Development repositories must be recreated once more, and this is the last
  time that is true.
- `ltx status` and `VerifyReport` must gain an explicit "working state not
  represented in any checkpoint" field. After partial save, a status that
  prints a head with no qualifier, or a verify that says "verified", is a claim
  the engine can no longer support. This is not cosmetic: `status` is where a
  user would look before an operation that replaces their bytes.
- `error.rs` gains `NoSuchChange` and `InvalidChange`. `Concept::Change` exists
  in the enum today but **no error variant produces it**, so G2.4 has never
  seen a change-concept error path. Their recovery text names `ltx change
  list`, which `recovery_actions_name_only_implemented_commands` requires to be
  a shipped command — so the verb and the error text land in the same commit.
- `OpLog::commit`'s publish payload widens from line state to line-and-change
  state. Publishing changes in a separate call would reopen the crash window
  ADR-16 §7 closed.
- `save` gains a `SaveOutcome` carrying the rescued working state, closing the
  tracked follow-up from the `assign` design review: `save` was the one caller
  of `complete_pending_switch` that discarded the address, breaking ADR-16 §6's
  "named in the result".
- `crates/undo-property/src/main.rs` adds `assign` and `save --change` to its
  operation set and `changes` to its snapshot, so the property harness exercises
  the domain G1.3 compares rather than leaving it vacuous.
- The materialiser's tip-tree fallback (`materialise_working_tree` with no
  preserved tree) becomes lossy by definition once a tip tree can be synthetic.
  It is currently unreachable — every LIFO-reachable call site passes `Some` —
  so it becomes an **error rather than a fallback**: "no preserved working
  state" and "materialise the tip" are two different facts that were only ever
  equal under the invariant this ADR removes.

## Open conflicts recorded, not resolved

1. **`save --change` selects content after a repair materialisation.** `save`
   calls `complete_pending_switch` before snapshotting, and that repair rewrites
   files. An assignment made against the pre-repair bytes would then be resolved
   against bytes the user never saw. Either selection happens before any repair,
   or `save --change` refuses while a pending marker exists. Resolved in
   implementation; recorded here because it is a correctness constraint on
   ordering that no type enforces.
2. **Undo scope under concurrency** (inherited from ADR-16, open conflict 2):
   `assign .` runs in G1.4's 8 concurrent workspaces, where the (I2) LIFO lemma
   does not hold. This bites at the workspace slice, not this one, but `assign`
   is what makes it concrete.
3. **Redo-branch policy** (ADR-15 §4, deferred): the batch freely draws
   `assign; undo; save --change`, so a forward command after an undo is now
   exercised. No decision is taken here; it is no longer hypothetical.
