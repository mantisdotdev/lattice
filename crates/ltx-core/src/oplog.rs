//! The operation log: an append-only, Merkle-linked record of every repo-level
//! operation (§5.3).
//!
//! It powers universal undo and audit, so two properties matter more than
//! anything else here:
//!
//!   1. **Every entry links its predecessor by hash.** Tampering with any
//!      historical entry breaks the chain from that point forward, and
//!      `verify` walks it.
//!   2. **An operation is not done until its entry is durable.** ADR-4 measured
//!      what that costs: `F_FULLFSYNC` on the reference machine is ~4.7 ms and
//!      pre-allocation does not help, because the cost is the media flush
//!      itself. Plain `fsync(2)` is 78× faster and is NOT crash-safe on macOS,
//!      so G1.1 forbids it.
//!
//! That 4.7 ms is affordable once per command (5% of G1.5's budget) and
//! ruinous under concurrency: G1.4's 8 workspaces × 10,000 operations would be
//! 6.3 hours of pure flushing. ADR-4's answer is group commit, which measured a
//! 25× amortisation, and it is implemented here rather than in the daemon so
//! the daemonless path gets it too.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::chunk::ChunkId;
use crate::error::{Error, Result};

const ENTRIES: TableDefinition<u64, &[u8]> = TableDefinition::new("oplog");
const HEADS: TableDefinition<&str, u64> = TableDefinition::new("heads");

/// Line state, held under a SINGLE key (`state`).
///
/// One key rather than one per line for two reasons (ADR-16 §7): a publish
/// mutates three facts at once — the source line's preserved working state, the
/// target's, and which line is current — so a single-key write makes it atomic
/// by construction; and user-controlled line names never become redb keys, so
/// the whole class of key-namespace questions does not arise.
const LINES: TableDefinition<&str, &[u8]> = TableDefinition::new("lines");
const LINES_KEY: &str = "state";
/// Where a migration parks the document it is about to replace.
const SUPERSEDED_LINES_KEY: &str = "state-superseded";

/// Checkpoint id -> the sequence of the `Save` that recorded it.
///
/// The op-log is the authority on what history contains, but answering "does a
/// Save reference this checkpoint?" by scanning it means loading every entry —
/// unbounded work for a bounded question, and on the read path of `log`. This
/// index answers it in one lookup, and is written in the same transaction as
/// the Save itself so the two can never disagree.
const SAVED: TableDefinition<&str, u64> = TableDefinition::new("saved");

/// On-disk format version, and the format new entries are written at.
///
/// 2 added lines: `Save` and `StartLine` gained fields, which changes what
/// `Entry::compute_id` re-serialises, so an older log reported every entry as
/// "altered" (ADR-16 §9). 3 adds changes and assignment (ADR-17 §9), and with
/// them the per-entry `format` tag that makes this the LAST break requiring a
/// repository to be recreated: from here, `compute_id` dispatches on the
/// format an entry was WRITTEN at, so entries written by an older build keep
/// hashing the way that build hashed them and the Merkle chain stays
/// continuous across a format change.
///
/// An in-place migration is impossible by construction — rewriting entries to
/// a new shape changes their ids, which is what the chain exists to detect —
/// so the tag is the only mechanism that can work, and it can only be
/// introduced AT a break.
///
/// 5 moves a checkpoint's blob to the address its id already named (ADR-8).
/// Entries are untouched — the ids they carry are unchanged, because the bytes
/// hashed to produce them are the bytes now stored — so this break is again
/// outside the chain, and is migrated rather than refused.
///
/// 6 adds the `Compact`, `Sync`, `Lens` and `Split` operations. No entry is
/// rewritten and nothing migrates; the bump exists so a build that predates the
/// variants refuses the repository with a way forward instead of failing to
/// parse an entry it never heard of.
///
/// 7 records, on `Switch` and `StartLine`, the workspace that made them, so an
/// undo can tell whose switch it is reversing (ADR-7, correction of
/// 2026-09-12), and records an `Undo` that found nothing to reverse with no
/// `undone_seq`. Additive both times: a field skipped when empty or absent, so
/// an entry written at 6 or earlier hashes exactly as it did and nothing
/// migrates. The bump
/// exists so a build that does not know the field refuses the repository
/// rather than re-hashing an entry without it and reporting a broken chain.
pub const FORMAT_VERSION: u64 = 7;

/// The oldest format this build can read. Below this the per-entry tag does
/// not exist, so an entry's original serialisation cannot be reproduced and
/// `verify_chain` could only report phantom tampering.
pub const MIN_READABLE_FORMAT: u64 = 3;

const FORMAT_KEY: &str = "format";

/// A change: a logical unit of work that has not been checkpointed yet
/// (§4.2, noun 2).
///
/// Holds a selection over the working tree, not content. The bytes stay on
/// disk and remain the truth while their line is current (ADR-16 §1); this
/// records only which of them a user has claimed for this unit of work.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeRecord {
    /// Working-tree paths assigned to this change, relative to the root, as
    /// raw bytes — the same doctrine tree entry names follow, so a path that
    /// is not valid UTF-8 is assignable like any other.
    pub assigned: std::collections::BTreeSet<Vec<u8>>,
}

/// A change that a `save --change` checkpointed, carried in that save's entry.
///
/// The whole record travels, not just the id: consuming a change removes it
/// from the line state, so its assignments then exist nowhere else and the
/// inverse would have nothing to put back (ADR-17 §7).
///
/// `Checkpoint` gains no field for this. Adding one would change `body_id` and
/// re-address every checkpoint in every repository, and the association is
/// provenance recorded by an operation rather than content — so it follows
/// `oplog_seq` exactly: a back-reference resolved at read time from here.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointedChange {
    pub id: String,
    pub record: ChangeRecord,
    /// Whether it was the current change. A save consumes the change it
    /// checkpoints, so a bare `assign` afterwards must not go on adding to
    /// something already checkpointed — and the inverse must put that back.
    pub was_current: bool,
}

/// One line of work: where it points, the working state held for it while it
/// is not current, and the changes open on it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineRecord {
    /// Checkpoint id this line points at, if it has one.
    ///
    /// An id, not an op-log seq: undo moves a tip to `checkpoint.parent`, which
    /// has an id but no unambiguous sequence number.
    pub tip: Option<String>,
    /// Live, un-checkpointed changes on this line, by id.
    ///
    /// Here rather than in a table of their own (ADR-17 §4): a switch already
    /// mutates the source line's preserved state, the target's, and `current`
    /// in one write, and an assignment is a selection over exactly those
    /// working-tree bytes. Sharing the key makes it atomic by construction
    /// instead of by a second write that has to be kept in step.
    ///
    /// Per-line, not repository-global, and that is not a close call: a switch
    /// replaces the working tree wholesale, so a global change set would name
    /// paths holding another line's content the instant it happened.
    ///
    /// `BTreeMap` so the serialised form and `change list` are canonical
    /// rather than creation-ordered — `change list` sits in G1.3's equality
    /// domain, where an unstable order would fail undo-all for a reason that
    /// has nothing to do with undo.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub changes: std::collections::BTreeMap<String, ChangeRecord>,
    /// The change a bare `ltx assign` adds to. `None` until the first assign.
    ///
    /// A current change exists because G1.4's frozen pool draws `assign .`
    /// with no change named, ten thousand times; without one, each draw would
    /// either fail or mint a change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_change: Option<String>,
}

/// One working tree over a repository (§4.2, noun 6).
///
/// The repository's own root is a workspace too, not a special case: eight
/// workspaces and none differ only in how many rows this table has.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceRecord {
    /// Where this workspace's working tree is, absolute, as raw bytes —
    /// or `None` for the repository's OWN root.
    ///
    /// `None` rather than a recorded path, because the repository's root is
    /// wherever the repository is: storing it would make renaming the
    /// repository's directory break every record of where its own working tree
    /// was, and repointing that on open is machinery for a fact that did not
    /// need storing. Every OTHER workspace is somewhere else by definition, and
    /// says where.
    ///
    /// Bytes rather than a `String`, following the doctrine tree entry names
    /// follow: a path need not be valid UTF-8, and a workspace whose directory
    /// this engine could not name would be one it could not find again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Vec<u8>>,
    /// The line this workspace is on.
    ///
    /// Per workspace rather than per repository (ADR-7 §3). One `current`
    /// could not mean eight things: the moment one workspace switched, every
    /// other would report being on a line whose bytes it does not have. That
    /// is not a race the repository lock can fix, because it is not a race.
    pub current: String,
    /// Line -> the working state THIS workspace preserved for it.
    ///
    /// Was `LineRecord.working`, which assumed exactly one current line. The
    /// bytes a line holds while it is not current are the bytes a particular
    /// workspace left there, and two workspaces leave different ones.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub preserved: std::collections::BTreeMap<String, String>,
    /// The lens this workspace looks through. Per workspace for the reason
    /// `current` is: two working trees may legitimately want different views.
    /// Defaults to the built-in lens that hides nothing, so a document written
    /// before the field existed reads back meaning exactly what it meant.
    #[serde(default = "default_lens", skip_serializing_if = "is_default_lens")]
    pub lens: String,
}

/// The one lens every repository has: it hides nothing.
pub const DEFAULT_LENS: &str = "clean";

fn default_lens() -> String {
    DEFAULT_LENS.to_string()
}

fn is_default_lens(lens: &str) -> bool {
    lens == DEFAULT_LENS
}

/// Which lines exist and which one is current.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineState {
    pub lines: std::collections::BTreeMap<String, LineRecord>,
    /// Working trees over this repository, by opaque id.
    ///
    /// In the same key as the lines, for ADR-16 §7's reason unchanged: a switch
    /// mutates several facts at once and they must move together or not at all.
    /// Eight workspaces rewriting one document is affordable precisely because
    /// ADR-6 made them take turns.
    ///
    /// Additive, so no on-disk format break: the line state is a published
    /// document, not hashed content — `Entry::compute_id` covers the operation
    /// and nothing else — so a document written before this field existed reads
    /// back with an empty map. The break comes later, when `current` moves in
    /// here and stops being one field meaning eight things (ADR-7 §3).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub workspaces: std::collections::BTreeMap<String, WorkspaceRecord>,
}

/// The line every repository starts on. G1.1 and G1.4 both run `switch main`
/// against a repository whose only setup is `init` + `save`, so it must exist
/// by default — and it is created by `init`, never by a `StartLine` entry,
/// which would be an eligible undo target (ADR-16 §2).
pub const DEFAULT_LINE: &str = "main";

impl LineState {
    /// The state a fresh repository starts in: one line, and one workspace —
    /// the repository's own root, which is a workspace and not a special case
    /// (ADR-7 §1).
    pub fn initial(workspace: &str) -> Self {
        let mut lines = std::collections::BTreeMap::new();
        lines.insert(DEFAULT_LINE.to_string(), LineRecord::default());
        let mut workspaces = std::collections::BTreeMap::new();
        workspaces.insert(
            workspace.to_string(),
            WorkspaceRecord {
                // The repository's own root: tracked, not stored.
                root: None,
                current: DEFAULT_LINE.to_string(),
                preserved: std::collections::BTreeMap::new(),
                lens: DEFAULT_LENS.to_string(),
            },
        );
        LineState { lines, workspaces }
    }
}

/// The line state as format 3 wrote it, read only in order to migrate it.
///
/// A separate type rather than `#[serde(default)]` on the live one: these
/// fields are GONE, and leaving them readable on `LineState` would leave two
/// ways to say where a workspace is — the thing ADR-7 §3 removes.
#[derive(Deserialize)]
pub(crate) struct LineStateV3 {
    pub current: String,
    pub lines: std::collections::BTreeMap<String, LineRecordV3>,
}

#[derive(Deserialize)]
pub(crate) struct LineRecordV3 {
    pub tip: Option<String>,
    pub working: Option<String>,
    #[serde(default)]
    pub changes: std::collections::BTreeMap<String, ChangeRecord>,
    #[serde(default)]
    pub current_change: Option<String>,
}

/// How long a committing thread waits for others to join its batch.
///
/// Small enough to be invisible against a ~4.7 ms flush, large enough that
/// concurrent workspaces actually coalesce.
const GROUP_WINDOW: Duration = Duration::from_micros(500);

/// What an operation did. Every state-changing command appears here — §6's
/// coverage contract requires the undo generator to enumerate this surface, so
/// a new command that does not add a variant cannot silently dodge G1.3.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Operation {
    Init,
    Save {
        message: String,
        checkpoint: String,
        /// The line whose tip this save advanced — undo must move that line's
        /// tip back, not whichever line happens to be current later.
        line: String,
        /// The change this save consumed and the assignment set it took, so
        /// the inverse restores it exactly. `None` for a save of the whole
        /// working state, which consumes nothing: assignment is a labelling,
        /// never a gate (ADR-17 §5).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change: Option<CheckpointedChange>,
    },
    StartLine {
        name: String,
        /// The line that was current before, so the inverse can return to it.
        from: String,
        /// Whether this call actually created the line. `start <existing>` is
        /// a switch and must exit 0 (G1.4 draws it thousands of times), so the
        /// record says what happened and the inverse reads it — otherwise undo
        /// would delete a line the FIRST start created.
        created: bool,
        /// The workspace whose working tree this changed. Its inverse belongs
        /// to that workspace alone — applied anywhere else it would put one
        /// working tree's switch onto another. Empty on entries written before
        /// format 7, which are treated as the undoing workspace's own.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        workspace: String,
    },
    /// Route working-tree paths into a change.
    ///
    /// Records an intent and touches no byte on disk, and neither does its
    /// inverse (ADR-17 §2) — which is why every field here is a label rather
    /// than an address, and why there is no capture to take.
    ///
    /// One call appends exactly ONE entry however many paths it moved. G1.3's
    /// undo budget is `applied * 3 + 8`, so an assign over k paths decomposing
    /// into k entries would exhaust it (ADR-17 §6).
    Assign {
        change: String,
        /// The line the change lives on. Changes are per-line, so the inverse
        /// puts the paths back where they were taken from rather than onto
        /// whichever line happens to be current by then.
        line: String,
        /// The paths this call actually moved — not the ones named on the
        /// command line, which may include paths already in the change or ones
        /// it refused. The inverse reverses what happened, not what was asked.
        paths: Vec<Vec<u8>>,
        /// Whether this call created the change. The same role
        /// `StartLine::created` plays: without it, `assign c f; assign c g;
        /// undo` would delete a change the FIRST assign created.
        created: bool,
        /// The change that was current before, so the inverse restores it.
        from_current: Option<String>,
        /// For the paths that were already in another change, the change they
        /// came from. Without this, undoing `assign --to c2 f` after `assign
        /// --to c1 f` would leave `f` unowned rather than owned by c1 — the
        /// class of bug `StartLine::created` exists to prevent, one level down.
        ///
        /// Paths that had no previous owner are absent rather than carried as
        /// null: `paths` already names them, and one authoritative home per
        /// fact is what keeps the two lists from ever disagreeing.
        displaced: Vec<(Vec<u8>, String)>,
    },
    /// Register a working tree over this repository.
    ///
    /// NOT undoable, and that is a decision rather than an oversight. ADR-7 §4
    /// makes undo repository-scoped, so `ltx undo` run anywhere reverses the
    /// log's last eligible entry whichever workspace appended it — and that
    /// could be the entry that created the workspace somebody else is working
    /// in at this moment. Undo exists to take back what you did, not to remove
    /// the ground another person is standing on.
    ///
    /// Nothing is stranded by that. The files it materialised live outside the
    /// repository entirely, so unlike an undone `start` there is no working
    /// state for the ephemeral tier to preserve — the directory simply stays
    /// where it is, with or without its marker.
    Workspace {
        id: String,
        /// The working tree, as raw bytes: a path need not be valid UTF-8, and
        /// a workspace this engine could not name is one it could not find.
        root: Vec<u8>,
    },
    Switch {
        from: String,
        to: String,
        /// The workspace that switched; see `StartLine::workspace`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        workspace: String,
    },
    /// An undo. `undone_seq` names the entry it reversed, and is absent when
    /// the call found nothing to reverse — still an attempt, made at a point
    /// in the history, and recorded for the reason a dry-run sync is: an
    /// operation eight workspaces interleave has to have a position, and one
    /// that leaves no trace cannot be shown to have happened.
    Undo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        undone_seq: Option<u64>,
    },
    Adopt {
        source: String,
    },
    /// Redaction is recorded like anything else, and is NOT undoable —
    /// docs/DISAGREEMENTS.md Challenge 12. The record is its own audit trail.
    Redact {
        target: String,
        redactor: String,
    },
    /// Thinning is likewise recorded and not undoable (§2c requires every
    /// thinning be logged).
    Thin {
        collected: u64,
    },
    /// An op-log segment was archived (ADR-13's first half). Not undoable:
    /// an archive is a copy, and there is nothing to put back.
    Compact {
        from_seq: u64,
        to_seq: u64,
    },
    /// A sync was attempted. `dry_run` never moves content, so its inverse is
    /// nothing — but the attempt is still a fact history records, and the one
    /// a concurrent history is ordered by.
    Sync {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote: Option<String>,
        dry_run: bool,
    },
    /// A workspace changed which lens it looks through.
    Lens {
        workspace: String,
        from: String,
        to: String,
    },
    /// One line took another's history. Fast-forward only: `after` is the
    /// other line's tip, which already contained `before`. `captured` is the
    /// working tree as it stood, durable, so the inverse can put it back;
    /// `None` when the merge moved nothing.
    Merge {
        line: String,
        from: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        captured: Option<String>,
    },
    /// A change was split: each moved path left `change` for the change named
    /// beside it. The inverse moves them back and removes what was minted.
    Split {
        line: String,
        /// The change that was split — the current change at the time, and
        /// so also what the inverse restores as current. `None` when nothing
        /// was current: the attempt is still a recorded fact, and moved
        /// nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change: Option<String>,
        /// (path, the change it went to). Every destination was minted by this
        /// split, so the inverse deletes them all.
        moved: Vec<(Vec<u8>, String)>,
    },
}

impl Operation {
    /// Whether `ltx undo` can reverse this.
    ///
    /// Challenge 12: §4.3 promises every state-changing command is undoable,
    /// but undoing a redaction would resurrect the secret it destroyed and
    /// falsify a GDPR erasure claim, and thinned data is simply gone. The
    /// honest scope is local causal undo with those two named exclusions among
    /// the state-changing commands, so G1.3 can assert each REFUSES undo rather
    /// than silently doing nothing. `Init` is separate: it establishes the
    /// repository container below the undo floor (ADR-15), is not a
    /// state-changing command for undo purposes, and is not undoable.
    ///
    /// `Undo` is also not itself reversible via `ltx undo` in this model. Undo
    /// is monotonic toward the root so that undo-all converges (G1.3 requires
    /// it); reversing an undo would be a forward "redo", which would oscillate
    /// under repeated `ltx undo` and never reach `nothing_to_undo`. Redo is a
    /// separate forward mechanism, deferred (ADR-15).
    ///
    /// `Adopt` has no defined inverse yet. Marking it here rather than only in
    /// the dispatcher means a core caller cannot append one and have undo skip
    /// it in silence; defining its inverse belongs to the adopt slice.
    pub fn is_undoable(&self) -> bool {
        !matches!(
            self,
            Operation::Init
                | Operation::Undo { .. }
                | Operation::Adopt { .. }
                | Operation::Redact { .. }
                | Operation::Thin { .. }
                | Operation::Compact { .. }
                | Operation::Workspace { .. }
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Operation::Init => "init",
            Operation::Save { .. } => "save",
            Operation::StartLine { .. } => "start",
            Operation::Assign { .. } => "assign",
            Operation::Workspace { .. } => "workspace",
            Operation::Switch { .. } => "switch",
            Operation::Undo { .. } => "undo",
            Operation::Adopt { .. } => "adopt",
            Operation::Redact { .. } => "redact",
            Operation::Thin { .. } => "thin",
            Operation::Compact { .. } => "compact",
            Operation::Sync { .. } => "sync",
            Operation::Lens { .. } => "lens",
            Operation::Split { .. } => "split",
            Operation::Merge { .. } => "merge",
        }
    }
}

/// One durable record.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    /// Hash of the previous entry, making the log Merkle-linked. The first
    /// entry links to all-zeroes.
    pub prev: String,
    /// Hash of this entry's own content, excluding this field.
    pub id: String,
    pub at_unix_ms: u64,
    pub operation: Operation,
    /// The on-disk format this entry was WRITTEN at, which fixes how its id is
    /// computed for the rest of the entry's life (ADR-17 §9).
    ///
    /// Without this, a build that changed an `Operation` variant could not
    /// re-derive the ids of entries an older build wrote, so every one of them
    /// would verify as tampered — which is why formats 1 and 2 could only be
    /// refused outright rather than migrated.
    pub format: u64,
}

impl Entry {
    /// Content hash over everything but `id`, so `id` can be recomputed and
    /// checked without a separate canonical form to keep in sync.
    ///
    /// Dispatches on `format`, so an entry written by an older build hashes
    /// the way that build hashed it. When a future version changes an
    /// `Operation` variant it adds an arm here and leaves the existing ones
    /// untouched; existing entries keep verifying.
    ///
    /// `format` is itself inside the payload. It has to be: if it were not
    /// authenticated, editing the tag on a stored entry would change the rule
    /// used to check that entry, which is a downgrade attack against the chain
    /// rather than a version field.
    fn compute_id(seq: u64, prev: &str, at: u64, op: &Operation, format: u64) -> Result<String> {
        let payload = match format {
            // 3, 4 and 5 serialise an operation identically. Each break since
            // 3 has been outside the hashed payload — 4 changed the line state,
            // a published document; 5 changed where a checkpoint blob is
            // stored, which is content. The tag is still inside the payload, so
            // entries written at different formats hash differently and each
            // verifies under its own rule.
            3..=7 => serde_json::to_vec(&(seq, prev, at, op, format))?,
            other => {
                return Err(Error::UnsupportedFormat(format!(
                    "entry {seq} records on-disk format {other}, which this build cannot hash"
                )))
            }
        };
        Ok(ChunkId::of(&payload).to_hex())
    }
}

/// One frame of the append-only mirror: exactly the triple `commit_batch`
/// writes into the index tables, so replaying frames reproduces those tables.
#[derive(Serialize, Deserialize)]
struct MirrorFrame {
    entry: Entry,
    lines: Option<LineState>,
    format: Option<u64>,
}

/// The op-log's append-only mirror (ADR-25).
///
/// ADR-3 bought crash atomicity from redb for exactly this metadata — and
/// G1.1 measured the purchase failing: redb 2.6.3 can abort on open with an
/// internal page-manager assertion, not an error, after legal torn-write
/// power loss, leaving the only queryable copy of history unopenable. An
/// index that can panic must be an index that can burn: every batch is
/// appended here and fsynced BEFORE the database commit, and a database that
/// cannot open is quarantined and rebuilt from these frames. Frames are
/// length-prefixed and checksummed; a torn tail is truncated away, which is
/// safe because a frame whose fsync did not finish was never acknowledged to
/// any caller.
struct Mirror {
    file: std::fs::File,
    path: std::path::PathBuf,
}

const MIRROR_MAGIC: &[u8; 8] = b"LTXMIRR1";
const MIRROR_CHECKSUM_LEN: usize = 8;
/// No frame is unbounded: an entry plus one LineState snapshot is small, and
/// a length prefix past this cap is damage, not a frame.
const MIRROR_MAX_FRAME: u32 = 64 * 1024 * 1024;
/// Rebuild and migration stream in transactions of this many frames, so a
/// long history is never held in memory whole.
const MIRROR_REPLAY_CHUNK: usize = 512;

impl Mirror {
    fn path_beside(db_path: &std::path::Path) -> std::path::PathBuf {
        db_path.with_file_name("oplog.append")
    }

    /// Open or create the mirror and scan it. Returns the mirror positioned
    /// for appending, the highest sequence it holds, and how many frames are
    /// intact. Anything after the last intact frame is truncated.
    fn open(path: &std::path::Path) -> Result<(Self, u64, u64)> {
        use std::io::{Read, Seek, SeekFrom, Write};
        let created = !path.exists();
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut header = [0u8; 8];
        let mut good_end: u64 = MIRROR_MAGIC.len() as u64;
        let mut last_seq = 0u64;
        let mut frames = 0u64;
        let header_ok = file.read_exact(&mut header).is_ok() && &header == MIRROR_MAGIC;
        if header_ok {
            loop {
                match Self::read_frame(&mut file) {
                    Ok(Some((frame, end))) => {
                        last_seq = frame.entry.seq;
                        frames += 1;
                        good_end = end;
                    }
                    // Clean end, or a torn/checksum-failed tail: everything
                    // before `good_end` is intact, everything after was never
                    // acknowledged.
                    Ok(None) | Err(_) => break,
                }
            }
        } else {
            // Empty file or damaged header. A header that never made it to
            // disk means no frame was ever acknowledged from this file.
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(MIRROR_MAGIC)?;
            file.sync_all()?;
        }
        file.set_len(good_end)?;
        file.seek(SeekFrom::End(0))?;
        if created {
            // The file's NAME is durable only once its directory is synced;
            // fsync on the file alone does not commit the directory entry.
            if let Some(dir) = path.parent() {
                if let Ok(d) = std::fs::File::open(dir) {
                    let _ = d.sync_all();
                }
            }
        }
        Ok((
            Mirror {
                file,
                path: path.to_path_buf(),
            },
            last_seq,
            frames,
        ))
    }

    /// Every intact frame of THIS mirror, streamed from a fresh read handle
    /// so the append position is undisturbed.
    fn frames_iter(&self) -> Result<MirrorFrames> {
        Self::frames(&self.path)
    }

    /// The next intact frame, or None at a clean end-of-file. A frame cut
    /// short or failing its checksum is an error — the caller treats the
    /// rest of the file as a torn tail.
    fn read_frame(file: &mut std::fs::File) -> Result<Option<(MirrorFrame, u64)>> {
        use std::io::{Read, Seek};
        let mut len_buf = [0u8; 4];
        match file.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_le_bytes(len_buf);
        if len == 0 || len > MIRROR_MAX_FRAME {
            return Err(Error::Corrupt(format!(
                "mirror frame length {len} is not a frame"
            )));
        }
        let mut sum = [0u8; MIRROR_CHECKSUM_LEN];
        file.read_exact(&mut sum)?;
        let mut payload = vec![0u8; len as usize];
        file.read_exact(&mut payload)?;
        if blake3::hash(&payload).as_bytes()[..MIRROR_CHECKSUM_LEN] != sum {
            return Err(Error::Corrupt("mirror frame checksum mismatch".into()));
        }
        let frame: MirrorFrame = serde_json::from_slice(&payload)?;
        let end = file.stream_position()?;
        Ok(Some((frame, end)))
    }

    fn encode(batch: &[(Entry, Option<LineState>, Option<u64>)]) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        for (entry, lines, format) in batch {
            let payload = serde_json::to_vec(&MirrorFrame {
                entry: entry.clone(),
                lines: lines.clone(),
                format: *format,
            })?;
            buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            buf.extend_from_slice(&blake3::hash(&payload).as_bytes()[..MIRROR_CHECKSUM_LEN]);
            buf.extend_from_slice(&payload);
        }
        Ok(buf)
    }

    /// Append a batch and make it durable. `sync_all` is the real barrier on
    /// every supported platform (F_FULLFSYNC on macOS, per ADR-18).
    fn append(&mut self, batch: &[(Entry, Option<LineState>, Option<u64>)]) -> Result<()> {
        use std::io::Write;
        let buf = Self::encode(batch)?;
        self.file.write_all(&buf)?;
        self.file.sync_all()?;
        Ok(())
    }

    fn offset(&mut self) -> Result<u64> {
        use std::io::Seek;
        Ok(self.file.stream_position()?)
    }

    /// Roll an unacknowledged append back out, so a database commit that
    /// failed cannot resurrect its batch on a later open.
    fn truncate_to(&mut self, offset: u64) -> Result<()> {
        use std::io::{Seek, SeekFrom};
        self.file.set_len(offset)?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Every intact frame, streamed in order.
    fn frames(path: &std::path::Path) -> Result<MirrorFrames> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(path)?;
        let mut header = [0u8; 8];
        let usable = file.read_exact(&mut header).is_ok() && &header == MIRROR_MAGIC;
        if !usable {
            file.seek(SeekFrom::End(0))?;
        }
        Ok(MirrorFrames { file, done: !usable })
    }
}

struct MirrorFrames {
    file: std::fs::File,
    done: bool,
}

impl Iterator for MirrorFrames {
    type Item = MirrorFrame;

    fn next(&mut self) -> Option<MirrorFrame> {
        if self.done {
            return None;
        }
        match Mirror::read_frame(&mut self.file) {
            Ok(Some((frame, _))) => Some(frame),
            // A torn tail ends replay exactly where open() truncates it.
            Ok(None) | Err(_) => {
                self.done = true;
                None
            }
        }
    }
}

/// Append-only operation log over redb.
///
/// redb rather than a hand-rolled file because ADR-3 buys crash atomicity here
/// rather than building it: this is the metadata whose torn write G1.1 punishes
/// hardest, and §0.8's novelty budget says to buy where a proven design exists.
/// What G1.1 then measured is that the bought atomicity has a failure mode of
/// its own, so the log now also keeps the append-only mirror above (ADR-25)
/// and treats the database as rebuildable.
pub struct OpLog {
    db: Database,
    group: Mutex<GroupState>,
    ready: Condvar,
    /// None only for the in-memory test constructor; every on-disk log
    /// mirrors (ADR-25).
    mirror: Mutex<Option<Mirror>>,
}

#[derive(Default)]
struct GroupState {
    /// Entries staged but not yet flushed, each with the LineState publish
    /// that must land in the SAME transaction as the entry (ADR-16 §7), so the
    /// log and the line state can never disagree after a crash.
    pending: Vec<(Entry, Option<LineState>, Option<u64>)>,
    /// Sequence number through which the log is durable.
    durable_through: u64,
    /// A flush is in progress; late arrivals wait rather than starting another.
    flushing: bool,
    /// Highest sequence number ASSIGNED, whether or not it is durable yet.
    ///
    /// This is the fix for a real data-loss race, and it is worth stating
    /// because the broken version looked obviously correct: `append` derived
    /// the next sequence from `pending.last()` or, when pending was empty, from
    /// the database. But a flush drains `pending` and releases the lock while
    /// committing, so a thread arriving in that window saw an empty `pending`
    /// AND a database that did not yet contain the in-flight batch. It
    /// therefore reassigned sequence numbers already in flight, and
    /// `table.insert` overwrote them. Measured: 4 of 200 concurrent appends
    /// silently lost.
    ///
    /// Assignment now comes from this counter alone, under the lock, and never
    /// from durable state.
    next_seq: u64,
    /// Id of the highest-numbered assigned entry, for chaining `prev`. Same
    /// reasoning: the chain must be continuous across the flush window.
    last_id: Option<String>,
    /// Set once a batch commit has FAILED. redb's commit is all-or-nothing, so
    /// a failure means none of that batch is durable — yet its sequence numbers
    /// are already spent and other threads may be waiting on them. Dropping the
    /// entries would either livelock a waiter (durability can never reach a seq
    /// whose entry is gone) or, once a later commit advanced past it, tell that
    /// waiter its entry is durable when it is not, breaking the Merkle chain.
    /// So a failed commit poisons the log instead: this and every in-flight and
    /// future append fails honestly, and the process should exit. On restart
    /// the log reopens from durable state with a continuous chain.
    poisoned: Option<String>,
}

impl OpLog {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let mirror_path = Mirror::path_beside(path);
        let db = match Self::open_and_probe(path) {
            Ok(db) => db,
            Err(open_error) => {
                if !mirror_path.exists() {
                    return Err(open_error);
                }
                Self::rebuild_database_from_mirror(path, &mirror_path, &open_error)?;
                // The rebuilt database proves itself under the same probe; a
                // rebuild that cannot pass it has nothing left to hide behind.
                Self::open_and_probe(path)?
            }
        };
        let mirror = Mirror::open(&mirror_path)?;
        Self::from_parts(db, Some(mirror))
    }

    /// redb 2.6.3 can refuse power-loss damage by PANICKING, not erring —
    /// and at two different moments. `page_manager.rs:266` asserts during
    /// open itself; `page_manager.rs:243` (raw_file_len >= header layout)
    /// only fires at first USE, so a database can open cleanly and then kill
    /// whatever command touches it next. The probe transaction forces that
    /// first use to happen here, inside the guard, so both roads lead to the
    /// same place: the mirror rebuild.
    fn open_and_probe(path: &std::path::Path) -> Result<Database> {
        let owned = path.to_path_buf();
        let attempt = std::panic::catch_unwind(move || -> Result<Database> {
            let db = Database::create(&owned).map_err(redb::Error::from)?;
            let tx = db.begin_write()?;
            tx.open_table(ENTRIES)?;
            tx.open_table(HEADS)?;
            tx.open_table(LINES)?;
            tx.open_table(SAVED)?;
            tx.commit()?;
            Ok(db)
        });
        match attempt {
            Ok(result) => result,
            Err(panic) => {
                let text = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("panic with no message");
                Err(Error::Corrupt(format!(
                    "operation-log index refused to open: {text}"
                )))
            }
        }
    }

    /// Quarantine whatever stands at `db_path` and rebuild it from the
    /// mirror beside it, holding the result to the open probe. False when
    /// no mirror exists — there is nothing to rebuild from. The caller
    /// holds the repository lock; this function does not know about locks.
    pub(crate) fn heal(db_path: &std::path::Path) -> Result<bool> {
        let mirror_path = Mirror::path_beside(db_path);
        if !mirror_path.exists() {
            return Ok(false);
        }
        let cause = Error::Corrupt("a command crashed inside the metadata index".into());
        Self::rebuild_database_from_mirror(db_path, &mirror_path, &cause)?;
        Self::open_and_probe(db_path)?;
        Ok(true)
    }

    /// Quarantine the unopenable database and rebuild it from the mirror.
    /// Nothing is deleted: the damaged file is renamed beside its
    /// replacement, so what happened can still be examined. The rebuilt
    /// database is dropped on return; the caller reopens it through the
    /// probe, holding it to the same standard as any other open.
    fn rebuild_database_from_mirror(
        db_path: &std::path::Path,
        mirror_path: &std::path::Path,
        cause: &Error,
    ) -> Result<()> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let quarantine = db_path.with_file_name(format!("meta.redb.corrupt-{stamp}"));
        std::fs::rename(db_path, &quarantine).map_err(|e| {
            Error::Corrupt(format!(
                "operation-log index is unopenable ({cause}) and could not be \
                 quarantined for rebuild: {e}"
            ))
        })?;
        let db = Database::create(db_path).map_err(|e| {
            Error::Corrupt(format!(
                "operation-log index is unopenable ({cause}) and a replacement \
                 could not be created: {e}"
            ))
        })?;
        let mut pending: Vec<(Entry, Option<LineState>, Option<u64>)> = Vec::new();
        for frame in Mirror::frames(mirror_path)? {
            pending.push((frame.entry, frame.lines, frame.format));
            if pending.len() >= MIRROR_REPLAY_CHUNK {
                Self::write_tables(&db, &pending)?;
                pending.clear();
            }
        }
        if !pending.is_empty() {
            Self::write_tables(&db, &pending)?;
        }
        Ok(())
    }

    /// One write transaction applying `batch` to all four tables — shared by
    /// rebuild, heal-forward and `commit_batch`, so a replayed batch can
    /// never mean something different from a live one.
    fn write_tables(
        db: &Database,
        batch: &[(Entry, Option<LineState>, Option<u64>)],
    ) -> Result<()> {
        let tx = db.begin_write()?;
        Self::apply_batch_to_tables(&tx, batch)?;
        tx.commit()?;
        Ok(())
    }

    /// Build an OpLog over an already-created redb Database, recovering the
    /// group-commit counters from durable state. Shared by `open` and, in
    /// tests, by a constructor over a fault-injecting backend (which gets no
    /// mirror, and with it none of the mirror's healing).
    fn from_database(db: Database) -> Result<Self> {
        Self::from_parts(db, None)
    }

    fn from_parts(db: Database, mirror: Option<(Mirror, u64, u64)>) -> Result<Self> {
        {
            let tx = db.begin_write()?;
            tx.open_table(ENTRIES)?;
            tx.open_table(HEADS)?;
            tx.open_table(LINES)?;
            tx.open_table(SAVED)?;
            tx.commit()?;
        }
        let db_last = {
            let tx = db.begin_read()?;
            let table = tx.open_table(ENTRIES)?;
            let value = match table.last()? {
                Some((k, _)) => k.value(),
                None => 0,
            };
            value
        };
        let mirror = match mirror {
            None => None,
            Some((mut m, mirror_last, mirror_frames)) => {
                if mirror_last > db_last {
                    // The mirror is ahead: power was lost between the mirror
                    // fsync and the database commit. The frames are the
                    // acknowledged truth — heal the database forward.
                    let mut pending: Vec<(Entry, Option<LineState>, Option<u64>)> = Vec::new();
                    for frame in m.frames_iter()? {
                        if frame.entry.seq <= db_last {
                            continue;
                        }
                        pending.push((frame.entry, frame.lines, frame.format));
                        if pending.len() >= MIRROR_REPLAY_CHUNK {
                            Self::write_tables(&db, &pending)?;
                            pending.clear();
                        }
                    }
                    if !pending.is_empty() {
                        Self::write_tables(&db, &pending)?;
                    }
                } else if mirror_frames == 0 && db_last > 0 {
                    // A repository from before the mirror existed: write the
                    // whole mirror now, so the next unopenable database has
                    // something to rebuild from.
                    Self::write_full_mirror(&db, &mut m)?;
                } else if mirror_last < db_last {
                    // Shorter than the database it mirrors — a partial legacy
                    // file with no authority. Rewrite it whole.
                    m.truncate_to(MIRROR_MAGIC.len() as u64)?;
                    Self::write_full_mirror(&db, &mut m)?;
                }
                Some(m)
            }
        };
        let last = {
            let tx = db.begin_read()?;
            let table = tx.open_table(ENTRIES)?;
            let value = match table.last()? {
                Some((k, _)) => k.value(),
                None => 0,
            };
            value
        };
        let last_id = {
            let tx = db.begin_read()?;
            let table = tx.open_table(ENTRIES)?;
            let id = match table.last()? {
                Some((_, v)) => Some(serde_json::from_slice::<Entry>(v.value())?.id),
                None => None,
            };
            id
        };
        Ok(OpLog {
            db,
            group: Mutex::new(GroupState {
                durable_through: last,
                next_seq: last,
                last_id,
                ..Default::default()
            }),
            ready: Condvar::new(),
            mirror: Mutex::new(mirror),
        })
    }

    /// Write the whole mirror from the database — the migration path for a
    /// repository older than the mirror. Intermediate LineState snapshots no
    /// longer exist anywhere, so the final frame carries the current one; a
    /// rebuild from this mirror reproduces the tables exactly as they stand.
    fn write_full_mirror(db: &Database, mirror: &mut Mirror) -> Result<()> {
        let (lines_now, format_now, last_seq) = {
            let tx = db.begin_read()?;
            let lines = tx
                .open_table(LINES)?
                .get(LINES_KEY)?
                .map(|v| serde_json::from_slice::<LineState>(v.value()))
                .transpose()?;
            let format = tx.open_table(HEADS)?.get(FORMAT_KEY)?.map(|v| v.value());
            let last = tx
                .open_table(ENTRIES)?
                .last()?
                .map(|(k, _)| k.value())
                .unwrap_or(0);
            (lines, format, last)
        };
        let mut from = 0u64;
        loop {
            let chunk: Vec<(Entry, Option<LineState>, Option<u64>)> = {
                let tx = db.begin_read()?;
                let table = tx.open_table(ENTRIES)?;
                let mut out = Vec::new();
                for item in table.range(from..)?.take(MIRROR_REPLAY_CHUNK) {
                    let (_, v) = item?;
                    let entry: Entry = serde_json::from_slice(v.value())?;
                    let is_last = entry.seq == last_seq;
                    out.push((
                        entry,
                        if is_last { lines_now.clone() } else { None },
                        if is_last { format_now } else { None },
                    ));
                }
                out
            };
            let Some((last_entry, _, _)) = chunk.last() else {
                break;
            };
            from = last_entry.seq + 1;
            mirror.append(&chunk)?;
        }
        Ok(())
    }

    pub fn head(&self) -> Result<Option<Entry>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(ENTRIES)?;
        let entry = match table.last()? {
            Some((_, v)) => Some(serde_json::from_slice::<Entry>(v.value())?),
            None => None,
        };
        Ok(entry)
    }

    pub fn len(&self) -> Result<u64> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(ENTRIES)?.len()?)
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Entries from `from` onwards. A range read: `compact` archives only
    /// what is new since the last archive, and loading the whole log to find
    /// that would make every compaction cost the size of history.
    pub fn entries_from(&self, from: u64) -> Result<Vec<Entry>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(ENTRIES)?;
        let mut out = Vec::new();
        for item in table.range(from..)? {
            let (_, value) = item?;
            out.push(serde_json::from_slice(value.value())?);
        }
        Ok(out)
    }

    pub fn entries(&self) -> Result<Vec<Entry>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(ENTRIES)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            let (_, v) = row?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    pub fn get(&self, seq: u64) -> Result<Option<Entry>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(ENTRIES)?;
        match table.get(seq)? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    }

    /// Append an operation and return once it is DURABLE.
    ///
    /// Group commit: a thread stages its entry, waits a brief window for others
    /// to arrive, and then one thread flushes the whole batch. Every caller
    /// returns only after the flush that covers its own sequence number, so the
    /// durability promise is per-operation even though the cost is shared.
    /// Append an operation with no state publish.
    pub fn append(&self, operation: Operation) -> Result<Entry> {
        self.commit(operation, None)
    }

    /// Create the repository's first entry, its line state and its format
    /// version in a SINGLE transaction.
    ///
    /// Init must be all-or-nothing: a `.lattice` that exists but records no
    /// format is refused by `open` AND by `init` ("already contains a
    /// repository"), which is a directory with no way forward.
    pub fn commit_initial(
        &self,
        operation: Operation,
        lines: LineState,
        format: u64,
    ) -> Result<Entry> {
        self.commit_inner(operation, Some(lines), Some(format))
    }

    /// Append an operation and, in the SAME durable transaction, publish the
    /// new line state. One transaction is what makes "which entries are undone"
    /// and "where the lines point" impossible to disagree after a crash, and it
    /// is why `set_head` is retired as a write (ADR-16 §7).
    pub fn commit(&self, operation: Operation, lines: Option<LineState>) -> Result<Entry> {
        self.commit_inner(operation, lines, None)
    }

    fn commit_inner(
        &self,
        operation: Operation,
        lines: Option<LineState>,
        format: Option<u64>,
    ) -> Result<Entry> {
        let entry = {
            let mut state = self.group.lock().unwrap();
            if let Some(msg) = &state.poisoned {
                return Err(Error::Corrupt(format!(
                    "operation log is unwritable after a failed commit: {msg}"
                )));
            }
            // Sequence and predecessor come from the in-memory counters, never
            // from durable state -- see GroupState::next_seq for why.
            state.next_seq += 1;
            let seq = state.next_seq;
            let prev = state.last_id.clone().unwrap_or_else(|| "0".repeat(64));
            let at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            // New entries are always written at the current format; the tag
            // records that so a future build can still reproduce this hash.
            let id = Entry::compute_id(seq, &prev, at, &operation, FORMAT_VERSION)?;
            let entry = Entry {
                seq,
                prev,
                id: id.clone(),
                at_unix_ms: at,
                operation,
                format: FORMAT_VERSION,
            };
            state.last_id = Some(id);
            state.pending.push((entry.clone(), lines, format));
            entry
        };

        self.flush_through(entry.seq)?;
        Ok(entry)
    }

    /// Make everything up to `seq` durable, coalescing with concurrent callers.
    fn flush_through(&self, seq: u64) -> Result<()> {
        let mut state = self.group.lock().unwrap();
        loop {
            if let Some(msg) = &state.poisoned {
                return Err(Error::Corrupt(format!(
                    "operation log is unwritable after a failed commit: {msg}"
                )));
            }
            if state.durable_through >= seq {
                return Ok(());
            }
            if state.flushing {
                // Someone else is already paying for the flush; their batch
                // may well include our entry.
                let (guard, _) = self
                    .ready
                    .wait_timeout(state, Duration::from_secs(30))
                    .unwrap();
                state = guard;
                continue;
            }

            state.flushing = true;
            // Brief window for other threads to join this batch. This is the
            // whole of ADR-4's 25× amortisation.
            let (guard, _) = self.ready.wait_timeout(state, GROUP_WINDOW).unwrap();
            state = guard;

            // `mem::take` rather than `drain(..).collect()`: same effect,
            // one allocation instead of two, and it is what
            // `clippy::drain_collect` asks for.
            let batch: Vec<(Entry, Option<LineState>, Option<u64>)> =
                std::mem::take(&mut state.pending);
            drop(state);

            // If commit_batch unwinds, this guard re-locks and clears
            // `flushing` (poisoning the log) so no thread waits forever on a
            // flush that panicked mid-way.
            let mut fg = FlushGuard {
                log: self,
                armed: true,
            };
            let result = self.commit_batch(&batch);

            state = self.group.lock().unwrap();
            fg.armed = false;
            state.flushing = false;
            match result {
                Ok(highest) => {
                    state.durable_through = state.durable_through.max(highest);
                    self.ready.notify_all();
                }
                Err(e) => {
                    // The batch did not land, and redb's commit is
                    // all-or-nothing, so NONE of it is durable. The entries are
                    // NOT restaged and NOT dropped-and-forgotten: instead the
                    // log is poisoned (see GroupState::poisoned). Every waiter,
                    // on wake, sees the poison and fails; so does every future
                    // append. That is the only outcome that neither livelocks a
                    // waiter nor reports a lost entry as durable.
                    state.poisoned = Some(e.to_string());
                    state.pending.clear();
                    self.ready.notify_all();
                    return Err(e);
                }
            }
        }
    }

    fn commit_batch(&self, batch: &[(Entry, Option<LineState>, Option<u64>)]) -> Result<u64> {
        if batch.is_empty() {
            return Ok(0);
        }
        // Mirror first (ADR-25): once these frames are fsynced, the batch
        // survives even a database that refuses to open ever again. If the
        // database commit below then fails, the frames are rolled back out —
        // a batch whose caller was told "no" must not resurrect on a later
        // open's heal-forward.
        let mut mirror_guard = self.mirror.lock().unwrap();
        let rollback_to = match mirror_guard.as_mut() {
            Some(m) => {
                let offset = m.offset()?;
                m.append(batch)?;
                Some(offset)
            }
            None => None,
        };
        let committed: Result<u64> = (|| {
            let tx = self.db.begin_write()?;
            Self::apply_batch_to_tables(&tx, batch)?;
            // redb's commit is the durability barrier; it fsyncs.
            tx.commit()?;
            Ok(batch.last().map(|(e, _, _)| e.seq).unwrap_or(0))
        })();
        if committed.is_err() {
            if let (Some(m), Some(offset)) = (mirror_guard.as_mut(), rollback_to) {
                // Best effort: if this truncate fails too, the log poisons on
                // the commit error anyway.
                let _ = m.truncate_to(offset);
            }
        }
        committed
    }

    /// The one meaning of "apply a batch": entries, the line publishes that
    /// must land in the same transaction (ADR-16 §7), the format rung, and
    /// the Save index. Shared by live commits, heal-forward and rebuild, so
    /// a replayed batch can never mean something different from a live one.
    fn apply_batch_to_tables(
        tx: &redb::WriteTransaction,
        batch: &[(Entry, Option<LineState>, Option<u64>)],
    ) -> Result<()> {
        {
            let mut table = tx.open_table(ENTRIES)?;
            for (entry, _, _) in batch {
                let bytes = serde_json::to_vec(entry)?;
                table.insert(entry.seq, bytes.as_slice())?;
            }
        }
        {
            // Line publishes land in the same transaction as their entries, in
            // sequence order. A state-changing command holds the repository
            // exclusively from its LineState read to this commit (`&mut Repo`
            // within a process, redb's exclusive lock across them), so a batch
            // carries at most one publish per writer and applying them in order
            // is the writer's own sequence. True cross-process concurrency is
            // the deferred workspace slice (ADR-16, open conflict 2).
            let mut table = tx.open_table(LINES)?;
            for (_, lines, _) in batch {
                if let Some(state) = lines {
                    let bytes = serde_json::to_vec(state)?;
                    table.insert(LINES_KEY, bytes.as_slice())?;
                }
            }
        }
        {
            let mut table = tx.open_table(HEADS)?;
            for (_, _, format) in batch {
                if let Some(v) = format {
                    table.insert(FORMAT_KEY, *v)?;
                }
            }
        }
        {
            // Index each Save by the checkpoint it recorded, in the entry's own
            // transaction, so a lookup can never see one without the other.
            let mut table = tx.open_table(SAVED)?;
            for (entry, _, _) in batch {
                if let Operation::Save { checkpoint, .. } = &entry.operation {
                    table.insert(checkpoint.as_str(), entry.seq)?;
                }
            }
        }
        Ok(())
    }

    /// The sequence of the `Save` that recorded this checkpoint, if any.
    ///
    /// One indexed lookup — the question "is this checkpoint part of history?"
    /// must not cost a scan of the whole log.
    /// Every checkpoint a `Save` has recorded, with the sequence of the
    /// newest Save naming it. Read from the index, so enumerating history's
    /// checkpoints costs one table and no entry is deserialised — which is
    /// what let a run of tens of thousands of operations keep loading the
    /// whole log to answer `log`, `thin` and `status`.
    pub fn saved_checkpoints(&self) -> Result<Vec<(String, u64)>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(SAVED)?;
        let mut out = Vec::new();
        for item in table.iter()? {
            let (key, value) = item?;
            out.push((key.value().to_string(), value.value()));
        }
        Ok(out)
    }

    /// Visit entries newest first, stopping when `visit` returns `Ok(false)`.
    ///
    /// A reverse range over the table, so a caller that wants the newest few
    /// pays for the newest few. `undo` wants exactly that: the highest live,
    /// eligible entry, which is almost always within a handful of the tail.
    /// Loading every entry to find it made each undo cost the size of history.
    pub fn walk_newest_first(&self, mut visit: impl FnMut(Entry) -> Result<bool>) -> Result<()> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(ENTRIES)?;
        for item in table.range(0u64..)?.rev() {
            let (_, value) = item?;
            let entry: Entry = serde_json::from_slice(value.value())?;
            if !visit(entry)? {
                break;
            }
        }
        Ok(())
    }

    pub fn save_seq(&self, checkpoint: &str) -> Result<Option<u64>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(SAVED)?;
        Ok(table.get(checkpoint)?.map(|v| v.value()))
    }

    /// One entry by sequence, or `None` if there is none at that position.
    ///
    /// An indexed read, so a caller asking about a single operation does not
    /// pay for loading the whole log.
    pub fn entry(&self, seq: u64) -> Result<Option<Entry>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(ENTRIES)?;
        match table.get(seq)? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    }

    /// Publish a migrated line state and the format it is written at, together.
    ///
    /// One transaction, and it has to be. As two, a crash between them leaves a
    /// NEW document under an OLD version — and the rerun reads the migrated
    /// document with the old reader, fails on a field that is gone, and reports
    /// a serde error whose recovery says this is probably a bug. Every command
    /// then fails with no way forward, which is the opposite of what ordering
    /// the writes was meant to achieve. Atomic by construction instead, the way
    /// ADR-16 §7 makes the line publish atomic for the same reason.
    pub fn publish_migrated_lines(&self, state: &LineState, version: u64) -> Result<()> {
        let bytes = serde_json::to_vec(state)?;
        let tx = self.db.begin_write()?;
        {
            let mut lines = tx.open_table(LINES)?;
            lines.insert(LINES_KEY, bytes.as_slice())?;
            let mut heads = tx.open_table(HEADS)?;
            heads.insert(FORMAT_KEY, version)?;
        }
        // redb's commit is the durability barrier; it fsyncs.
        tx.commit()?;
        Ok(())
    }

    /// Publish a line state given as raw bytes.
    ///
    /// Test-only: `LINES["state"]` is authoritative and every read deserialises
    /// it, so arbitrary bytes here turn every later command into a
    /// deserialisation error. It exists to build the shape an older format
    /// wrote, which no current build can produce.
    #[cfg(test)]
    pub(crate) fn publish_raw_line_state(&self, raw: &[u8]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(LINES)?;
            table.insert(LINES_KEY, raw)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The document a migration replaced, if one did.
    pub fn superseded_line_state(&self) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(LINES)?;
        Ok(table.get(SUPERSEDED_LINES_KEY)?.map(|v| v.value().to_vec()))
    }

    /// The published line state as raw bytes, for a reader that must interpret
    /// it under a format other than this build's.
    pub fn raw_line_state(&self) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(LINES)?;
        Ok(table.get(LINES_KEY)?.map(|v| v.value().to_vec()))
    }

    /// Keep a superseded line state under its own key, before a migration
    /// overwrites the live one.
    ///
    /// Never read by the engine. It exists so that a migration that goes wrong
    /// leaves something to restore from — every earlier format break refused
    /// to open, and a rewrite is the first one that could destroy rather than
    /// decline.
    pub fn keep_superseded_line_state(&self, raw: &[u8]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(LINES)?;
            table.insert(SUPERSEDED_LINES_KEY, raw)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The published line state, or `None` for a repository that has none.
    pub fn line_state(&self) -> Result<Option<LineState>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(LINES)?;
        match table.get(LINES_KEY)? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    }

    /// Record the on-disk format version. Written by `init` in the same
    /// transaction that creates the repository's first entry.
    pub fn set_format_version(&self, version: u64) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(HEADS)?;
            table.insert(FORMAT_KEY, version)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The recorded format version, or `None` for a database written before
    /// versions were recorded.
    pub fn format_version(&self) -> Result<Option<u64>> {
        self.get_head(FORMAT_KEY)
    }

    /// The format recorded in the database at `path`, without building an
    /// `OpLog` over it.
    ///
    /// The gate has to run BEFORE anything deserialises an entry, and building
    /// an `OpLog` deserialises the newest one to recover the chain head. An
    /// entry written at a format this build cannot read need not have this
    /// build's shape — a format-2 entry has no `format` field at all, and a
    /// format-1 `Save` has no `line` — so that read fails with a serde error
    /// and the user is told "missing field `format`" and that this is probably
    /// a bug in Lattice. What they should be told is which formats this build
    /// reads and to start a fresh repository, which is what the gate says.
    ///
    /// Reads through the same accessor as `format_version`, so there is one
    /// answer to "what format is this" rather than two that can drift.
    pub fn format_version_at(path: &std::path::Path) -> Result<Option<u64>> {
        let db = Database::create(path)?;
        let tx = db.begin_read()?;
        // A database with no `heads` table has never recorded a format, which
        // is the unversioned format 1 — not an error.
        let table = match tx.open_table(HEADS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(table.get(FORMAT_KEY)?.map(|v| v.value()))
    }

    /// Publish line state without appending an operation.
    ///
    /// Used only by `init`, which must create the default line below the undo
    /// floor — it cannot be created by a `StartLine` entry, which undo would be
    /// eligible to reverse (ADR-16 §2).
    pub fn publish_lines(&self, state: &LineState) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(LINES)?;
            let bytes = serde_json::to_vec(state)?;
            table.insert(LINES_KEY, bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Walk the hash chain. Returns the first break, or None if intact.
    ///
    /// This is what makes "append-only" checkable rather than asserted: an
    /// edited historical entry changes its own id, which no longer matches the
    /// `prev` its successor recorded.
    pub fn verify_chain(&self) -> Result<Option<String>> {
        // A tampered entry may carry a `prev` or `id` that is short OR whose
        // 12th byte falls inside a multibyte UTF-8 codepoint. This function
        // exists to REPORT such tampering, so it must never panic slicing the
        // very field it is inspecting — hence a char-boundary-safe truncation,
        // not a raw byte index.
        fn short(s: &str) -> &str {
            let mut end = s.len().min(12);
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            &s[..end]
        }
        let mut expected_prev = "0".repeat(64);
        for entry in self.entries()? {
            if entry.prev != expected_prev {
                return Ok(Some(format!(
                    "operation {} links to {} but its predecessor hashes to {}",
                    entry.seq,
                    short(&entry.prev),
                    short(&expected_prev)
                )));
            }
            // Hashed by the rule of the format the entry was written at, not
            // by this build's, so a chain that spans a format change verifies.
            //
            // A tag this build cannot hash is REPORTED, not raised. An entry
            // whose format was edited to an unhashable value is exactly the
            // damage this function exists to describe, and raising would leave
            // `ltx verify` unable to produce a report at all — the one thing it
            // must always do. Same doctrine as `short` above: never fail on the
            // field you are inspecting.
            let recomputed = match Entry::compute_id(
                entry.seq,
                &entry.prev,
                entry.at_unix_ms,
                &entry.operation,
                entry.format,
            ) {
                Ok(id) => id,
                Err(Error::UnsupportedFormat(why)) => {
                    return Ok(Some(format!(
                        "operation {} cannot be authenticated: {why}",
                        entry.seq
                    )))
                }
                Err(other) => return Err(other),
            };
            if recomputed != entry.id {
                return Ok(Some(format!(
                    "operation {} has been altered: its content hashes to {} \
                     but it records {}",
                    entry.seq,
                    short(&recomputed),
                    short(&entry.id)
                )));
            }
            expected_prev = entry.id;
        }
        Ok(None)
    }

    pub fn set_head(&self, name: &str, seq: u64) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(HEADS)?;
            table.insert(name, seq)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_head(&self, name: &str) -> Result<Option<u64>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(HEADS)?;
        Ok(table.get(name)?.map(|v| v.value()))
    }
}

/// Restores the `flushing` flag if the committing thread unwinds.
///
/// On the normal path `armed` is cleared once the lock is reacquired. If
/// `commit_batch` panics, this drops with `armed` still set, re-locks, clears
/// `flushing`, and poisons the log — so a panicked flush cannot leave every
/// other thread waiting on a flush that will never complete.
struct FlushGuard<'a> {
    log: &'a OpLog,
    armed: bool,
}

impl Drop for FlushGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self.log.group.lock().unwrap_or_else(|e| e.into_inner());
        state.flushing = false;
        if state.poisoned.is_none() {
            state.poisoned = Some("a commit unwound mid-flush".to_string());
        }
        self.log.ready.notify_all();
    }
}

// OpLog is Send + Sync by auto-derivation: redb's Database is Send + Sync, and
// the group state is behind a Mutex. No `unsafe impl` is needed, and one would
// only serve to silence the compiler if a non-thread-safe field were added
// later — exactly the check worth keeping.

impl std::fmt::Debug for OpLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpLog").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn log() -> (tempfile::TempDir, OpLog) {
        let dir = tempfile::tempdir().unwrap();
        let log = OpLog::open(&dir.path().join("oplog.redb")).unwrap();
        (dir, log)
    }

    /// A redb backend that injects a sync failure on demand, so the
    /// poison-on-commit-failure path can be exercised through the REAL commit
    /// (redb's write barrier fails) rather than a hook in production code.
    #[derive(Debug)]
    struct FailingBackend {
        inner: redb::backends::InMemoryBackend,
        fail: Arc<AtomicBool>,
    }

    impl redb::StorageBackend for FailingBackend {
        fn len(&self) -> std::result::Result<u64, std::io::Error> {
            self.inner.len()
        }
        fn read(&self, offset: u64, len: usize) -> std::result::Result<Vec<u8>, std::io::Error> {
            self.inner.read(offset, len)
        }
        fn set_len(&self, len: u64) -> std::result::Result<(), std::io::Error> {
            self.inner.set_len(len)
        }
        fn sync_data(&self, eventual: bool) -> std::result::Result<(), std::io::Error> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("injected sync failure"));
            }
            self.inner.sync_data(eventual)
        }
        fn write(&self, offset: u64, data: &[u8]) -> std::result::Result<(), std::io::Error> {
            self.inner.write(offset, data)
        }
    }

    fn failing_log() -> (OpLog, Arc<AtomicBool>) {
        let fail = Arc::new(AtomicBool::new(false));
        let backend = FailingBackend {
            inner: redb::backends::InMemoryBackend::new(),
            fail: fail.clone(),
        };
        let db = Database::builder().create_with_backend(backend).unwrap();
        (OpLog::from_database(db).unwrap(), fail)
    }

    #[test]
    fn appends_are_sequential_and_chained() {
        let (_d, log) = log();
        let a = log.append(Operation::Init).unwrap();
        let b = log
            .append(Operation::StartLine {
                name: "auth".into(),
                from: "main".into(),
                created: true,
                workspace: "root".into(),
            })
            .unwrap();
        assert_eq!(a.seq, 1);
        assert_eq!(b.seq, 2);
        assert_eq!(b.prev, a.id, "each entry links its predecessor by hash");
        assert_eq!(a.prev, "0".repeat(64));
        assert!(log.verify_chain().unwrap().is_none());
    }

    #[test]
    fn an_altered_entry_breaks_the_chain() {
        let (dir, log) = log();
        log.append(Operation::Init).unwrap();
        log.append(Operation::Save {
            message: "first".into(),
            checkpoint: "abc".into(),
            line: "main".into(),
            change: None,
        })
        .unwrap();
        assert!(log.verify_chain().unwrap().is_none());
        drop(log);

        // Rewrite entry 2's message, leaving its recorded id untouched.
        let db = Database::create(dir.path().join("oplog.redb")).unwrap();
        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(ENTRIES).unwrap();
                let raw = table.get(2u64).unwrap().unwrap().value().to_vec();
                let mut entry: Entry = serde_json::from_slice(&raw).unwrap();
                entry.operation = Operation::Save {
                    message: "tampered".into(),
                    checkpoint: "abc".into(),
                    line: "main".into(),
                    change: None,
                };
                let bytes = serde_json::to_vec(&entry).unwrap();
                table.insert(2u64, bytes.as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        drop(db);

        let log = OpLog::open(&dir.path().join("oplog.redb")).unwrap();
        let broken = log.verify_chain().unwrap();
        assert!(broken.is_some(), "an altered entry must break the chain");
        assert!(broken.unwrap().contains("altered"));
    }

    #[test]
    fn an_entry_whose_format_tag_cannot_be_hashed_is_reported_not_raised() {
        // `verify` exists to describe damage, so an entry whose format tag has
        // been edited to a value this build cannot hash has to come back as a
        // chain break. Raising instead left `ltx verify` unable to produce a
        // report at all — the one thing it must always do.
        let (dir, log) = log();
        log.append(Operation::Init).unwrap();
        drop(log);

        let db = Database::create(dir.path().join("oplog.redb")).unwrap();
        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(ENTRIES).unwrap();
                let raw = table.get(1u64).unwrap().unwrap().value().to_vec();
                let mut entry: Entry = serde_json::from_slice(&raw).unwrap();
                entry.format = 99;
                let bytes = serde_json::to_vec(&entry).unwrap();
                table.insert(1u64, bytes.as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        drop(db);

        let log = OpLog::open(&dir.path().join("oplog.redb")).unwrap();
        let broken = log
            .verify_chain()
            .expect("an unhashable tag is damage to report, not an error to raise");
        assert!(
            broken
                .unwrap_or_default()
                .contains("cannot be authenticated"),
            "and the report says which operation and why"
        );
    }

    #[test]
    fn redaction_and_thinning_are_not_undoable() {
        // Challenge 12. Undoing a redaction would resurrect the secret it
        // destroyed; thinned data is gone. Both are recorded, neither reverses.
        assert!(!Operation::Redact {
            target: "x".into(),
            redactor: "k".into()
        }
        .is_undoable());
        assert!(!Operation::Thin { collected: 3 }.is_undoable());
        assert!(Operation::Save {
            message: "m".into(),
            checkpoint: "c".into(),
            line: "main".into(),
            change: None,
        }
        .is_undoable());
        assert!(
            Operation::Assign {
                change: "c1".into(),
                line: "main".into(),
                paths: vec![b"a.txt".to_vec()],
                created: true,
                from_current: None,
                displaced: Vec::new(),
            }
            .is_undoable(),
            "assign is a state-changing command, so §4.3 promises it reverses"
        );
        assert!(
            !Operation::Undo {
                undone_seq: Some(1)
            }
            .is_undoable(),
            "undo is monotonic toward the root; reversing it (redo) is a \
             separate deferred forward move, so undo is not itself undoable"
        );
        assert!(
            !Operation::Init.is_undoable(),
            "init is the undo floor and is not undoable"
        );
    }

    #[test]
    fn concurrent_appends_are_all_durable_and_uniquely_numbered() {
        // The G1.4 property in miniature: group commit must not lose or
        // duplicate an entry when several threads append at once.
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(OpLog::open(&dir.path().join("oplog.redb")).unwrap());
        let mut handles = Vec::new();
        for t in 0..8 {
            let log = Arc::clone(&log);
            handles.push(std::thread::spawn(move || {
                for i in 0..25 {
                    log.append(Operation::StartLine {
                        name: format!("t{t}-{i}"),
                        from: "main".into(),
                        created: true,
                        workspace: "root".into(),
                    })
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let entries = log.entries().unwrap();
        assert_eq!(entries.len(), 200, "every append must be durable");
        let seqs: std::collections::BTreeSet<u64> = entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs.len(), 200, "sequence numbers must be unique");
        assert_eq!(*seqs.iter().next().unwrap(), 1);
        assert_eq!(*seqs.iter().next_back().unwrap(), 200);
        assert!(
            log.verify_chain().unwrap().is_none(),
            "chain must survive concurrency"
        );
    }

    #[test]
    fn a_failed_commit_poisons_the_log_rather_than_losing_or_faking_entries() {
        let (log, fail) = failing_log();
        log.append(Operation::Init).unwrap();

        fail.store(true, Ordering::SeqCst);
        // The append whose commit fails must report failure, not success.
        assert!(
            log.append(Operation::Thin { collected: 1 }).is_err(),
            "a failed commit must not report the entry as durable"
        );
        // Clear the fault. A log that merely dropped-and-forgot the failed
        // batch would now accept an append; a POISONED log stays unwritable.
        // This is what distinguishes the fix from the old behaviour.
        fail.store(false, Ordering::SeqCst);
        assert!(
            log.append(Operation::Thin { collected: 2 }).is_err(),
            "a poisoned log must refuse further appends even after the fault clears"
        );

        // The durable state is intact and gap-free: only the Init that actually
        // committed survives, and the chain still verifies.
        assert_eq!(log.len().unwrap(), 1, "only the durable Init survived");
        assert!(
            log.verify_chain().unwrap().is_none(),
            "chain must stay intact"
        );
    }

    #[test]
    fn verify_chain_reports_a_short_tampered_field_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplog.redb");
        {
            let log = OpLog::open(&path).unwrap();
            log.append(Operation::Init).unwrap();
        }
        // Tamper entry 1's `prev` down to two characters — shorter than the 12
        // bytes the report used to slice unconditionally.
        let db = Database::create(&path).unwrap();
        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(ENTRIES).unwrap();
                let raw = table.get(1u64).unwrap().unwrap().value().to_vec();
                let mut e: Entry = serde_json::from_slice(&raw).unwrap();
                e.prev = "ab".into();
                let bytes = serde_json::to_vec(&e).unwrap();
                table.insert(1u64, bytes.as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        drop(db);

        let log = OpLog::open(&path).unwrap();
        let broken = log.verify_chain().unwrap();
        assert!(
            broken.is_some(),
            "a tampered entry must be reported, not panicked on"
        );
    }

    #[test]
    fn new_entries_record_the_format_they_were_written_at() {
        let dir = tempfile::tempdir().unwrap();
        let log = OpLog::open(&dir.path().join("oplog.redb")).unwrap();
        log.append(Operation::Init).unwrap();
        let entries = log.entries().unwrap();
        assert_eq!(entries[0].format, FORMAT_VERSION);
        assert!(
            log.verify_chain().unwrap().is_none(),
            "an entry must verify under the format it recorded"
        );
    }

    #[test]
    fn retagging_an_entry_to_another_format_is_refused_not_accepted() {
        // The tag decides which rule authenticates the entry, so it must not
        // be editable into a rule that would accept different content — that
        // is a downgrade attack against the chain, not a version field. It is
        // inside the hashed payload for exactly this reason, which is also why
        // it had to be introduced AT a break rather than added later.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplog.redb");
        {
            let log = OpLog::open(&path).unwrap();
            log.append(Operation::Init).unwrap();
            assert!(log.verify_chain().unwrap().is_none());
        }
        let db = Database::create(&path).unwrap();
        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(ENTRIES).unwrap();
                let raw = table.get(1u64).unwrap().unwrap().value().to_vec();
                let mut e: Entry = serde_json::from_slice(&raw).unwrap();
                e.format = FORMAT_VERSION + 1;
                let bytes = serde_json::to_vec(&e).unwrap();
                table.insert(1u64, bytes.as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        drop(db);

        let log = OpLog::open(&path).unwrap();
        // Reported as altered, or refused as unhashable — either is a refusal.
        // What must NOT happen is a clean pass.
        assert!(
            !matches!(log.verify_chain(), Ok(None)),
            "a re-tagged entry must never verify clean"
        );
    }

    #[test]
    fn verify_chain_does_not_panic_on_a_multibyte_field_at_byte_twelve() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplog.redb");
        {
            let log = OpLog::open(&path).unwrap();
            log.append(Operation::Init).unwrap();
        }
        // Tamper entry 1's `prev` to 11 ASCII bytes + a 3-byte codepoint, so
        // byte index 12 lands INSIDE the euro sign — a raw [..12] slice would
        // panic "not a char boundary".
        let db = Database::create(&path).unwrap();
        {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(ENTRIES).unwrap();
                let raw = table.get(1u64).unwrap().unwrap().value().to_vec();
                let mut e: Entry = serde_json::from_slice(&raw).unwrap();
                e.prev = format!("{}\u{20AC}", "a".repeat(11));
                let bytes = serde_json::to_vec(&e).unwrap();
                table.insert(1u64, bytes.as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        drop(db);

        let log = OpLog::open(&path).unwrap();
        let broken = log.verify_chain().unwrap();
        assert!(
            broken.is_some(),
            "a hostile multibyte field must be reported, not panicked on"
        );
    }

    #[test]
    fn reopening_continues_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplog.redb");
        let first = {
            let log = OpLog::open(&path).unwrap();
            log.append(Operation::Init).unwrap()
        };
        let log = OpLog::open(&path).unwrap();
        let second = log.append(Operation::Thin { collected: 1 }).unwrap();
        assert_eq!(second.seq, 2);
        assert_eq!(second.prev, first.id);
        assert!(log.verify_chain().unwrap().is_none());
    }

    #[test]
    fn the_database_rebuilds_from_the_mirror_when_it_cannot_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.redb");
        {
            let log = OpLog::open(&path).unwrap();
            log.append(Operation::Init).unwrap();
            log.append(Operation::Save {
                message: "first".into(),
                checkpoint: "abc".into(),
                line: "main".into(),
                change: None,
            })
            .unwrap();
        }
        // The index is damaged beyond opening; the mirror is the survivor.
        std::fs::write(&path, b"not a database at all").unwrap();
        let log = OpLog::open(&path).unwrap();
        assert_eq!(log.len().unwrap(), 2);
        assert!(log.verify_chain().unwrap().is_none());
        assert_eq!(
            log.saved_checkpoints().unwrap(),
            vec![("abc".to_string(), 2)]
        );
        let quarantined = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("meta.redb.corrupt-")
            });
        assert!(quarantined, "the damaged file is kept beside its replacement");
    }

    #[test]
    fn a_torn_mirror_tail_is_truncated_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.redb");
        {
            let log = OpLog::open(&path).unwrap();
            log.append(Operation::Init).unwrap();
        }
        let mirror = dir.path().join("oplog.append");
        let intact = std::fs::metadata(&mirror).unwrap().len();
        let mut bytes = std::fs::read(&mirror).unwrap();
        bytes.extend_from_slice(&[7u8; 9]); // meaningless torn tail
        std::fs::write(&mirror, &bytes).unwrap();
        let log = OpLog::open(&path).unwrap();
        assert_eq!(log.len().unwrap(), 1);
        assert_eq!(
            std::fs::metadata(&mirror).unwrap().len(),
            intact,
            "open truncated the tail away"
        );
        log.append(Operation::Init).unwrap();
        assert_eq!(log.len().unwrap(), 2, "appending after truncation works");
    }

    #[test]
    fn a_mirror_ahead_of_the_database_heals_it_forward() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.redb");
        let entry3 = {
            let log = OpLog::open(&path).unwrap();
            log.append(Operation::Init).unwrap();
            let e2 = log.append(Operation::Init).unwrap();
            let at = 12345u64;
            let id = Entry::compute_id(3, &e2.id, at, &Operation::Init, FORMAT_VERSION).unwrap();
            Entry {
                seq: 3,
                prev: e2.id.clone(),
                id,
                at_unix_ms: at,
                operation: Operation::Init,
                format: FORMAT_VERSION,
            }
        };
        // Hand-write the next frame into the mirror alone, as if power died
        // between the mirror fsync and the database commit.
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(dir.path().join("oplog.append"))
                .unwrap();
            let payload = serde_json::to_vec(&MirrorFrame {
                entry: entry3.clone(),
                lines: None,
                format: None,
            })
            .unwrap();
            file.write_all(&(payload.len() as u32).to_le_bytes()).unwrap();
            file.write_all(&blake3::hash(&payload).as_bytes()[..MIRROR_CHECKSUM_LEN])
                .unwrap();
            file.write_all(&payload).unwrap();
        }
        let log = OpLog::open(&path).unwrap();
        assert_eq!(log.len().unwrap(), 3, "the mirror's extra frame healed forward");
        assert_eq!(log.head().unwrap().unwrap().id, entry3.id);
        assert!(log.verify_chain().unwrap().is_none());
    }

    #[test]
    fn a_repository_without_a_mirror_grows_one_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.redb");
        {
            let log = OpLog::open(&path).unwrap();
            log.append(Operation::Init).unwrap();
            log.append(Operation::Save {
                message: "first".into(),
                checkpoint: "abc".into(),
                line: "main".into(),
                change: None,
            })
            .unwrap();
        }
        // A pre-mirror repository: the file never existed.
        std::fs::remove_file(dir.path().join("oplog.append")).unwrap();
        {
            let _ = OpLog::open(&path).unwrap();
        }
        // The migrated mirror must be enough to survive the database dying.
        std::fs::write(&path, b"garbage").unwrap();
        let log = OpLog::open(&path).unwrap();
        assert_eq!(log.len().unwrap(), 2);
        assert_eq!(log.saved_checkpoints().unwrap().len(), 1);
        assert!(log.verify_chain().unwrap().is_none());
    }
}
