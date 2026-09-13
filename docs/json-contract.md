# The JSON contract

Every normal-path command, run with `--json`, prints exactly one JSON
document on stdout. This file documents each document's top-level fields;
gate G2.5 (`harness/g2/g2_5_json_contract.py`) holds every command to the
schema frozen there and every section's `Fields:` line here to the schema's
field list — both directions, a missing field and a stale one alike — so
this page cannot silently drift from the binary.

Two conventions, decided in ADR-23:

- **Success documents are closed.** A field appears in the schema and here,
  or the gate fails; a new field is a deliberate contract change, made by
  refreezing the harness.
- **List documents carry `version: 1`; act documents do not.** A listing is
  a data interchange shape a tool may store and diff, so it is versioned
  and is invariant under undo-all (no timestamps, no counters). An act
  document describes the one operation that just ran, identified by its
  `oplog_seq`.

Machine field names deliberately use precise storage vocabulary (`tree`,
`chunks_verified`) where §5.1 specifies it; §4.2's seven-noun cap governs
the human surface, which G2.3 lints separately.

Most act documents carry `oplog_seq` — the operation-log position this
command wrote, the linearizability anchor G1.4 checks — and
`rescued_working_state` — the address of working state the engine had to
preserve out of the way first, `null` when nothing needed rescuing.

## init

Fields: `ok` · `root`

`ok` · `root` — the absolute path of the repository just started.

## save

Fields: `change` · `checkpoint` · `message` · `ok` · `oplog_seq` · `parent` · `rescued_working_state` · `tree` · `working_state`

`ok` · `checkpoint` — the new checkpoint's address · `tree` — the directory
tree it holds · `message` · `parent` — the previous checkpoint's address,
`null` for the first · `oplog_seq` · `change` — the change this consumed
for a partial save, `null` for a plain save · `working_state` — the address
of the whole working state at save time, which for a partial save is the
only durable name the unsaved remainder has · `rescued_working_state`.

## status

Fields: `ok` · `status`

`ok` · `status` — an object: `checkpoints`, `operations`, `chunks`, `packs`
(counts), `head` / `head_message` / `head_change` (the current checkpoint's
address, message, and consumed change, each `null` when absent), `root`.

## log

Fields: `checkpoints` · `forensic` · `ok`

`ok` · `forensic` — whether every line's history is shown · `checkpoints` —
newest first; each carries `id`, `message`, `parent`, `tree`, `oplog_seq`,
`at_unix_ms`.

## verify

Fields: `checkpoints` · `checkpoints_partial` · `chunks_absent` · `chunks_redacted` · `chunks_verified` · `complete` · `errors` · `ok` · `oplog_entries` · `structure_verified`

`ok` — true only when the structure verified and `errors` is empty ·
`structure_verified` · `complete` — whether this was `--complete`; only
then may the result be read as an unqualified "verified" ·
`checkpoints` / `checkpoints_partial` · `chunks_verified` / `chunks_absent`
/ `chunks_redacted` · `oplog_entries` · `errors` — each problem, as text.

## checkout

Fields: `checkpoint` · `collisions` · `entries` · `into` · `ok`

`ok` · `checkpoint` — what was written · `into` · `entries` — the number of paths
written · `collisions` — names this filesystem could not hold, each with
`path`, `reason`, and the `collided_with` sibling (empty when the reason
is not a fold).

## undo

Fields: `nothing_to_undo` · `now_at` · `ok` · `oplog_seq` · `preserved_working_state` · `remote_effects_not_undone` · `rescued_working_state` · `undo_seq` · `undone_checkpoint`

`ok` · `nothing_to_undo` · `undone_checkpoint` — `null` when the undone
operation made no checkpoint · `now_at` · `undo_seq` and `oplog_seq` — the
op-log position of the undo itself (one value, both names; `undo_seq` is
the historical spelling) · `preserved_working_state` — where the line's
working state went, if it had to be kept · `rescued_working_state` ·
`remote_effects_not_undone` — §4.3: remote residue this undo could not
reverse, empty for a purely local undo.

## start

Fields: `created` · `line` · `now_at` · `ok` · `oplog_seq` · `rescued_working_state`

`ok` · `line` · `created` — false when the line already existed ·
`now_at` — the checkpoint the line points at, `null` before any save ·
`oplog_seq` · `rescued_working_state`.

## switch

Fields: `line` · `now_at` · `ok` · `oplog_seq` · `rescued_working_state`

`ok` · `line` · `now_at` — `null` on a line with nothing saved yet ·
`oplog_seq` · `rescued_working_state`.

## assign

Fields: `assigned` · `change` · `created` · `line` · `ok` · `oplog_seq` · `refused` · `rescued_working_state` · `short`

`ok` · `change` / `short` — the change assigned to, full id and short form ·
`created` — whether this assign opened it · `line` · `assigned` — the paths
taken · `refused` — paths not taken, each with `path` and `reason`
(reported as data, exit stays 0: a path that cannot be taken is not a
failed command) · `oplog_seq` · `rescued_working_state`.

## split

Fields: `change` · `into` · `moved` · `ok` · `oplog_seq` · `rescued_working_state`

`ok` · `change` — the change that was split, `null` when none is current ·
`into` — the new changes' ids · `moved` — paths moved · `oplog_seq` ·
`rescued_working_state`.

## merge

Fields: `fast_forward` · `from` · `line` · `now_at` · `ok` · `oplog_seq` · `rescued_working_state`

`ok` · `line` — the line merged onto · `from` · `fast_forward` · `now_at` ·
`oplog_seq` · `rescued_working_state`.

## redact

Fields: `chunks_destroyed` · `ok` · `oplog_seq` · `places_in_history` · `rescued_working_state` · `target`

`ok` · `target` · `chunks_destroyed` · `places_in_history` — how many
checkpoints held the content · `oplog_seq` · `rescued_working_state`.

## lens use

Fields: `lens` · `ok` · `oplog_seq` · `rescued_working_state`

`ok` · `lens` · `oplog_seq` · `rescued_working_state`.

## lens list

Fields: `lenses` · `ok` · `version`

`ok` · `version` · `lenses` — each with `name`, `active`, `hides`.

## line list

Fields: `current` · `lines` · `ok` · `version`

`ok` · `version` · `current` — this workspace's line (two workspaces may
legitimately disagree, ADR-7) · `lines` — each with `name` and `checkpoint`
(`null` before any save on that line).

## change list

Fields: `changes` · `ok` · `version`

`ok` · `version` · `changes` — each with `id`, `short`, `current`, and its
`assigned` paths.

## workspace new

Fields: `entries` · `ok` · `oplog_seq` · `rescued_working_state` · `root` · `workspace`

`ok` · `workspace` — the new workspace's id · `root` · `entries` — the number of
working-state paths written into it · `oplog_seq` · `rescued_working_state`.

## workspace list

Fields: `ok` · `version` · `workspaces`

`ok` · `version` · `workspaces` — each with `id`, `short`, `root`, and
`present` (false when its directory is gone).

## sync

Fields: `dry_run` · `ok` · `oplog_seq` · `remote` · `rescued_working_state` · `would_receive` · `would_send`

`ok` · `dry_run` · `remote` — `null` while no remote is configured ·
`would_send` / `would_receive` · `oplog_seq` · `rescued_working_state`.

## thin

Fields: `collected` · `ok` · `oplog_seq` · `packs_removed` · `rescued_working_state`

`ok` · `collected` — unreferenced chunks removed · `packs_removed` ·
`oplog_seq` · `rescued_working_state`.

## error document

Fields: `category` · `concept` · `error` · `ok` · `recovery`

Every failure, from every command: `ok` (always `false`) · `error` — what
happened · `category` — one causal category of six (`not-a-repository`,
`not-found`, `corrupt`, `io`, `invalid`, `busy`) · `concept` — which of the
seven §4.2 concepts the error is about · `recovery` — a way back; every
`ltx` command it names exists. G2.4 measures these five requirements; the
schema here pins the shape.
