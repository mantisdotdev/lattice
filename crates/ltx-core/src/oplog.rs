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
pub const FORMAT_VERSION: u64 = 3;

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
    /// Tree address of the working state preserved for this line.
    ///
    /// Always `None` for the CURRENT line — becoming current consumes the
    /// preserved state, ceasing to be current sets it. While a line is current
    /// the bytes on disk are the truth. Every undo inverse is exact and
    /// mechanical only because of this invariant (ADR-16 §1).
    pub working: Option<String>,
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

/// Which lines exist and which one is current.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineState {
    pub current: String,
    pub lines: std::collections::BTreeMap<String, LineRecord>,
}

/// The line every repository starts on. G1.1 and G1.4 both run `switch main`
/// against a repository whose only setup is `init` + `save`, so it must exist
/// by default — and it is created by `init`, never by a `StartLine` entry,
/// which would be an eligible undo target (ADR-16 §2).
pub const DEFAULT_LINE: &str = "main";

impl LineState {
    pub fn initial() -> Self {
        let mut lines = std::collections::BTreeMap::new();
        lines.insert(DEFAULT_LINE.to_string(), LineRecord::default());
        LineState {
            current: DEFAULT_LINE.to_string(),
            lines,
        }
    }
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
    Switch {
        from: String,
        to: String,
    },
    Undo {
        undone_seq: u64,
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
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Operation::Init => "init",
            Operation::Save { .. } => "save",
            Operation::StartLine { .. } => "start",
            Operation::Assign { .. } => "assign",
            Operation::Switch { .. } => "switch",
            Operation::Undo { .. } => "undo",
            Operation::Adopt { .. } => "adopt",
            Operation::Redact { .. } => "redact",
            Operation::Thin { .. } => "thin",
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
            3 => serde_json::to_vec(&(seq, prev, at, op, format))?,
            other => {
                return Err(Error::UnsupportedFormat(format!(
                    "entry {seq} records on-disk format {other}, which this build cannot hash"
                )))
            }
        };
        Ok(ChunkId::of(&payload).to_hex())
    }
}

/// Append-only operation log over redb.
///
/// redb rather than a hand-rolled file because ADR-3 buys crash atomicity here
/// rather than building it: this is the metadata whose torn write G1.1 punishes
/// hardest, and §0.8's novelty budget says to buy where a proven design exists.
pub struct OpLog {
    db: Database,
    group: Mutex<GroupState>,
    ready: Condvar,
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
        Self::from_database(Database::create(path)?)
    }

    /// Build an OpLog over an already-created redb Database, recovering the
    /// group-commit counters from durable state. Shared by `open` and, in
    /// tests, by a constructor over a fault-injecting backend.
    fn from_database(db: Database) -> Result<Self> {
        {
            let tx = db.begin_write()?;
            tx.open_table(ENTRIES)?;
            tx.open_table(HEADS)?;
            tx.open_table(LINES)?;
            tx.open_table(SAVED)?;
            tx.commit()?;
        }
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
        })
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
        let tx = self.db.begin_write()?;
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
        // redb's commit is the durability barrier; it fsyncs.
        tx.commit()?;
        Ok(batch.last().map(|(e, _, _)| e.seq).unwrap_or(0))
    }

    /// The sequence of the `Save` that recorded this checkpoint, if any.
    ///
    /// One indexed lookup — the question "is this checkpoint part of history?"
    /// must not cost a scan of the whole log.
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
            let recomputed = Entry::compute_id(
                entry.seq,
                &entry.prev,
                entry.at_unix_ms,
                &entry.operation,
                entry.format,
            )?;
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
            !Operation::Undo { undone_seq: 1 }.is_undoable(),
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
}
