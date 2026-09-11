# ADR-7 — Workspaces

**Status:** Accepted · **Builds on:** ADR-6 (concurrent access) · **Amends:** ADR-16 (§7, what the line state holds)
**Answers:** ADR-16 open conflict 2 and ADR-17 open conflict 2, both of which deferred undo-under-concurrency to here
**Gates:** G1.4 (concurrency, HARD), G1.2 (byte fidelity, HARD — **passing as last recorded, UNVERIFIED for this change**), G1.3 (universal undo, HARD)

## Context

A workspace is the sixth of §4.2's seven nouns and the last piece G1.4 needs to
run at all. Two frozen harnesses draw it, and they specify more than a design
argument could:

- **`harness/g1/g1_4_concurrency.py:126-131`** initialises a repository at
  `work/repo`, then creates eight workspaces at `work/ws0` … `work/ws7` —
  **siblings of the repository, not children** — each by
  `ltx workspace new <path>` run with `cwd=work/repo`. Eight threads then run
  ordinary commands with `cwd=<workspace>`, and `:89` checks a **single**
  `oplog_seq` order across all of them. One repository, one log, eight working
  trees.
- **`harness/g1/g1_2_byte_fidelity.py:174`** uses `workspace new <dest>` as its
  *checkout* mechanism, falling back to `checkout --into <dest>` only if it
  fails, and then verifies the adversarial corpus at `<dest>` byte for byte —
  NFC/NFD pairs, invalid UTF-8, the executable bit, symlinks.

That second one is the sharpest constraint in this document. The last recorded
run of G1.2, in `bench/results/iteration-9.json`, passed:

```json
{
  "gate": "G1.2",
  "target": 0.0,
  "value": 0.0,
  "status": "PASS"
}
```

**That result does not validate what this ADR ships, and must not be read as
doing so.** It was produced when `workspace new` did not exist, so the harness
fell through to `checkout --into` — it measured the fallback, not the path the
CLI now takes. The moment `workspace new` succeeds, G1.2 measures it instead. So
a workspace that does not materialise the tip tree exactly does not merely fail
its own gate; it turns a gate that was passing red, and the recorded PASS would
be the last honest thing said about it.

**G1.2 is therefore UNVERIFIED for this change**, and re-running it is owed
before the gate is claimed again. It cannot be run in this checkout: its corpus
(`corpus/data/adversarial`) is deliberately not committed. What could be run is
its *comparison* — `collect_entries` and the path-set diff, imported from the
frozen harness — over a small stand-in with symlinks and an executable bit, and
over both marker shapes below. That is evidence about the mechanism, not a gate
result, and the distinction is the whole reason this paragraph exists.

ADR-6 already settled that concurrent commands queue rather than race, so this
ADR does not have to answer whether eight working trees can address one
repository. It only has to answer what a workspace *is*.

## Decision

### 1. A workspace is a directory that points at a repository

`.lattice` is always a **directory**. A repository's holds packs, the op-log and
HEAD; a workspace's holds one file, `repository`, naming the repository whose
directory has the content. `Repo::discover` already walks upward looking for
that one name, so it learns one new thing — that the directory may be a pointer
rather than the thing itself — rather than a second search.

> **Corrected before shipping.** This section first said `.lattice` as a *file*
> means workspace, following Git's `.git`-file shape. **That would have turned
> G1.2 from PASS to FAIL.** G1.2 compares the source and destination path sets
> as raw bytes and reports anything extra as `appeared after checkout, absent in
> source`; its `collect_entries` prunes `.lattice` from `dirnames` only, so a
> `.lattice` *file* lands in `filenames` and is never filtered.
>
> Measured by importing the frozen harness's own `collect_entries` and running
> it over both shapes:
>
> ```console
> $ python3 - <<'EOF'   # imports harness/g1/g1_2_byte_fidelity.py
> ... builds a source with a .lattice DIRECTORY (a repository),
> ... and a destination with the marker in each shape, then diffs the path sets
> EOF
> marker as a file      -> only_dst=[b'.lattice'] only_src=[]  =>  G1.2 FAILS
> marker as a directory -> only_dst=[] only_src=[]             =>  G1.2 PASSES
> ```
>
> <!-- evidence: output of an ad-hoc script importing collect_entries from the frozen harness harness/g1/g1_2_byte_fidelity.py; reproducible from the two shapes described above, and pinned by the test `a_workspace_marker_is_a_file_inside_a_lattice_directory_never_a_lattice_file` -->
>
> The harness is frozen, so the product adapts. This is the constraint the
> Context section named as the sharpest in the document, arriving exactly where
> it said it would — and it is why `workspace new` had to be measured against
> G1.2's own code before being believed.

A second marker *name* was the other option and loses for the reason Git's
choice was right: every tool, every ignore file and every "am I in a repository"
check would have to learn it separately. Keeping one name and distinguishing by
contents costs a directory entry and nothing else.

The repository's own root is a workspace too, not a special case. One code path
covers both, and the eight-workspace and zero-workspace repositories differ only
in how many rows a table has.

### 2. `workspace new` materialises the tip tree

Forced by G1.2, and correct anyway: a working tree with no files is not a place
anyone can work. It is `checkout --into` with a marker written afterwards, and
it reuses that machinery rather than growing a second materialiser — including
its collision reporting, which is exactly what the adversarial corpus is for.

The marker is written **after** the content, and the directory is fsynced
between: a crash during creation leaves a directory of files that is not yet a
workspace, which is inert. The reverse order leaves a workspace whose content is
incomplete, which is a workspace that lies.

### 3. Each workspace holds its own current line

`LineState.current` — one field — becomes per-workspace state. Eight workspaces
running `switch main` and `start line` against a single `current` cannot all be
right: the moment one switches, every other workspace reports being on a line
whose bytes it does not have. That is not a race ADR-6's lock can fix, because
it is not a race at all; it is one field being asked to mean eight things.

jj, whose change ids this project already borrows, resolved it the same way:
"workspaces give each working copy its own working-copy commit against one
shared repo".

Preserved working state moves with it. Today `LineRecord.working` holds the
bytes a line had while it was not current, which assumed exactly one "current".
It becomes per `(workspace, line)`: the bytes *this* workspace had for *that*
line when it last switched away.

```rust
pub struct WorkspaceRecord {
    /// Where this workspace's working tree lives, absolute.
    pub root: PathBuf,
    /// The line this workspace is on.
    pub current: String,
    /// Line -> the working state this workspace preserved for it.
    pub preserved: BTreeMap<String, String>,
}
```

This lands in the **same `LINES` key**, for ADR-16 §7's reason unchanged: a
switch mutates the source line's preserved state, the target's, and which line
is current, and those must move together or not at all. Adding a second table
would add a second write to keep in step and a new way for the two to disagree
after a crash. Eight workspaces rewriting one document is affordable precisely
because ADR-6 made them take turns.


> **Corrected 2026-09-12, from the G1.4 pilots.** Two things this section
> implied were never written down in the op-log or the engine, and the pilot
> found both: 20 of 8,000 operations failed, every one with "this line
> preserved no working state, so there is nothing to restore" (13 `undo`,
> 5 `switch main`, 2 `start line`), and a second pilot with the failing
> output captured showed that single error 22 times in 2,400.
> `bench/results/raw/adr7-workspace-undo.json`:
>
> ```json
> {
>   "ops_total": 8000,
>   "failures": 20,
>   "linearizability_violations": 0
> }
> ```
>
> First: a workspace joining a line it has never been on had nothing
> preserved for it, and the switch was refused. What it gets is the line's
> tip — the same act `workspace new` performs — published under the target
> line before the commit so a crash still leaves a pending switch the next
> command completes.
>
> Second, and the one that matters: `Switch` and `StartLine` entries named
> no workspace, so a workspace's `undo` could pick up *another* workspace's
> switch as the newest undoable entry and apply its inverse here, against
> preserved state this workspace never kept. The entries now record the
> workspace that made them, and eligibility treats another workspace's
> switch, start or lens change as not this one's to reverse — it keeps
> looking further back. That is on-disk format 7: additive, the field is
> skipped when empty so entries written earlier hash exactly as they did and
> nothing migrates, and the bump is there so a build without the field
> refuses the repository instead of re-hashing its entries and reporting a
> broken chain.

### 4. Undo is repository-scoped, and that is a stated limit

`ltx undo` reverses the op-log's last eligible entry, whichever workspace
appended it. So a workspace can undo work another workspace did.

That is surprising, and it is chosen anyway. The alternative — reverse *this
workspace's* last entry — reverses an entry with later entries standing on top
of it, from workspaces that knew nothing about the reversal. ADR-17 §8 has just
finished paying for exactly that class of bug at the root floor, and its lesson
was that eligibility must be exactly the precondition of the inverse. Under
workspace-scoped undo the precondition is no longer "nothing eligible is newer",
and ADR-16's (I2) LIFO lemma — the thing that makes undo-all *converge*, which
G1.3 measures — no longer holds.

So v1 keeps the sound rule and says out loud that it is repository-scoped. The
honest phrasing is a scope limit, not a promise: undo is *local causal undo over
one repository*, and a workspace is a view onto that repository rather than a
sovereign history. Making undo workspace-scoped is a widening that needs its own
eligibility argument, and it is deferred, not rejected.

### 5. `workspace list` ships in this slice

For the reason `change list` did in ADR-17 §6: a noun with no way to enumerate
it is a noun a user cannot inspect, and G2.5's concept model expects each of the
seven to be reachable. It carries no timestamp and no counter, so it is
invariant under apply-batch-then-undo-all.

## Consequences

- **On-disk format 3 → 4, and this one is a migration.** ADR-17 §9 introduced
  the per-entry format tag precisely so that this break would not force a
  repository to be recreated: `compute_id` dispatches on the format an entry was
  written at, so existing entries keep hashing the way they were hashed. The
  line state is a published document rather than a hashed entry, so it is
  rewritten in place on open. **This is the first migration, and it is the test
  of whether that mechanism actually works.** It does: exercised by
  `a_format_three_repository_migrates_and_keeps_what_it_replaced`, which builds
  a format-3 document by hand — no build that produces one exists any more —
  and asserts the lines, the moved working state, the version and the kept copy.
  Both halves are mutation-checked.

  **The break belongs to §3 alone, not to the slice.** Registering workspaces
  was purely additive — a published document is not hashed content, so a
  document written before the field existed reads back with an empty map and no
  version needed to move. Only *removing* `current` and `working` is what an
  older build must be kept away from. Sequencing it that way put the novel,
  destructive step last and smallest, instead of gating everything behind it.

  **The migration keeps what it replaces**, and the version moves last. Every
  earlier break refused to open; this one rewrites, and a rewrite that goes
  wrong is unrecoverable in a way a refusal is not. The pre-migration document
  is written to its own key before anything is touched, and an interruption
  leaves a repository still readable as format 3 that simply migrates again.
  One key buys the difference between "restore it" and "it is gone".
- **`line list` becomes workspace-relative.** It reports the current line of the
  workspace the command ran in. Two workspaces legitimately disagree about which
  line is current, and that is the feature.
- **A workspace whose directory is gone is a dangling row.** Removing a
  workspace directory does not remove its record; `workspace list` reports it as
  missing rather than pretending. Pruning is a verb, not a side effect of
  noticing.
- **`checkout --into` stays.** It writes a checkpoint's contents somewhere with
  no marker and no record — the export, not the workspace. G1.2 falls back to it
  and it remains the honest answer for "give me these bytes".
- **The ephemeral tier (ADR-10) grows a dimension.** Preserved trees are now per
  workspace as well as per line, so whatever eventually retrieves them has more
  to name.

## Open conflicts recorded, not resolved

1. **Undo across workspaces is sound but surprising** (§4). Recorded as a scope
   limit; the widening needs an eligibility argument this ADR does not make.
2. **Autosnapshot must be workspace-local.** `docs/prior-art/sapling.md` §89
   reports the one published account of the flagship use case rejecting jj for
   transparently updating other worktrees, and concludes the winning property is
   *deferred, explicit* propagation. Nothing here autosnapshots yet, but when it
   lands it must not cross a workspace boundary without being asked.
3. **The op-log is still a chain.** ADR-6 open conflict 1 stands: local
   workspaces take turns so the chain stays linear, and the DAG question belongs
   to sync.
