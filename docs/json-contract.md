# The JSON contract

Every normal-path command, run with `--json`, prints exactly one JSON
document on stdout. This file documents each document's top-level fields;
gate G2.5 (`harness/g2/g2_5_json_contract.py`) holds every command to the
schema frozen there and every section here to the schema's field list, so
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

`ok` · `root` — the absolute path of the repository just started.

## save

`ok` · `checkpoint` — the new checkpoint's address · `tree` — the directory
tree it holds · `message` · `parent` — the previous checkpoint's address,
`null` for the first · `oplog_seq` · `change` — the change this consumed
for a partial save, `null` for a plain save · `working_state` — the address
of the whole working state at save time, which for a partial save is the
only durable name the unsaved remainder has · `rescued_working_state`.

## status

`ok` · `status` — an object: `checkpoints`, `operations`, `chunks`, `packs`
(counts), `head` / `head_message` / `head_change` (the current checkpoint's
address, message, and consumed change, each `null` when absent), `root`.

## log

`ok` · `forensic` — whether every line's history is shown · `checkpoints` —
newest first; each carries `id`, `message`, `parent`, `tree`, `oplog_seq`,
`at_unix_ms`.

## verify

`ok` — true only when the structure verified and `errors` is empty ·
`structure_verified` · `complete` — whether this was `--complete`; only
then may the result be read as an unqualified "verified" ·
`checkpoints` / `checkpoints_partial` · `chunks_verified` / `chunks_absent`
/ `chunks_redacted` · `oplog_entries` · `errors` — each problem, as text.

## checkout

`ok` · `checkpoint` — what was written · `into` · `entries` — paths
written · `collisions` — names this filesystem could not hold, each with
`path`, `reason`, and the `collided_with` sibling (empty when the reason
is not a fold).

## undo

`ok` · `nothing_to_undo` · `undone_checkpoint` — `null` when the undone
operation made no checkpoint · `now_at` · `undo_seq` and `oplog_seq` — the
op-log position of the undo itself (one value, both names; `undo_seq` is
the historical spelling) · `preserved_working_state` — where the line's
working state went, if it had to be kept · `rescued_working_state` ·
`remote_effects_not_undone` — §4.3: remote residue this undo could not
reverse, empty for a purely local undo.

## start

`ok` · `line` · `created` — false when the line already existed ·
`now_at` — the checkpoint the line points at, `null` before any save ·
`oplog_seq` · `rescued_working_state`.

## switch

`ok` · `line` · `now_at` — `null` on a line with nothing saved yet ·
`oplog_seq` · `rescued_working_state`.

## assign

`ok` · `change` / `short` — the change assigned to, full id and short form ·
`created` — whether this assign opened it · `line` · `assigned` — the paths
taken · `refused` — paths not taken, each with `path` and `reason`
(reported as data, exit stays 0: a path that cannot be taken is not a
failed command) · `oplog_seq` · `rescued_working_state`.

## split

`ok` · `change` — the change that was split, `null` when none is current ·
`into` — the new changes' ids · `moved` — paths moved · `oplog_seq` ·
`rescued_working_state`.

## merge

`ok` · `line` — the line merged onto · `from` · `fast_forward` · `now_at` ·
`oplog_seq` · `rescued_working_state`.

## redact

`ok` · `target` · `chunks_destroyed` · `places_in_history` — how many
checkpoints held the content · `oplog_seq` · `rescued_working_state`.

## lens use

`ok` · `lens` · `oplog_seq` · `rescued_working_state`.

## lens list

`ok` · `version` · `lenses` — each with `name`, `active`, `hides`.

## line list

`ok` · `version` · `current` — this workspace's line (two workspaces may
legitimately disagree, ADR-7) · `lines` — each with `name` and `checkpoint`
(`null` before any save on that line).

## change list

`ok` · `version` · `changes` — each with `id`, `short`, `current`, and its
`assigned` paths.

## workspace new

`ok` · `workspace` — the new workspace's id · `root` · `entries` — paths of
working state written into it · `oplog_seq` · `rescued_working_state`.

## workspace list

`ok` · `version` · `workspaces` — each with `id`, `short`, `root`, and
`present` (false when its directory is gone).

## sync

`ok` · `dry_run` · `remote` — `null` while no remote is configured ·
`would_send` / `would_receive` · `oplog_seq` · `rescued_working_state`.

## thin

`ok` · `collected` — unreferenced chunks removed · `packs_removed` ·
`oplog_seq` · `rescued_working_state`.

## error document

Every failure, from every command: `ok` (always `false`) · `error` — what
happened · `category` — one causal category of six (`not-a-repository`,
`not-found`, `corrupt`, `io`, `invalid`, `busy`) · `concept` — which of the
seven §4.2 concepts the error is about · `recovery` — a way back; every
`ltx` command it names exists. G2.4 measures these five requirements; the
schema here pins the shape.
