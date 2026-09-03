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
    },
    StartLine {
        name: String,
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
    /// honest scope is local causal undo with these two named exclusions, and
    /// naming them here means G1.3 can assert that each REFUSES undo rather
    /// than silently doing nothing.
    pub fn is_undoable(&self) -> bool {
        !matches!(self, Operation::Redact { .. } | Operation::Thin { .. })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Operation::Init => "init",
            Operation::Save { .. } => "save",
            Operation::StartLine { .. } => "start",
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
}

impl Entry {
    /// Content hash over everything but `id`, so `id` can be recomputed and
    /// checked without a separate canonical form to keep in sync.
    fn compute_id(seq: u64, prev: &str, at: u64, op: &Operation) -> Result<String> {
        let payload = serde_json::to_vec(&(seq, prev, at, op))?;
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
    /// Entries staged but not yet flushed.
    pending: Vec<Entry>,
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
}

impl OpLog {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let db = Database::create(path)?;
        {
            let tx = db.begin_write()?;
            tx.open_table(ENTRIES)?;
            tx.open_table(HEADS)?;
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
    pub fn append(&self, operation: Operation) -> Result<Entry> {
        let entry = {
            let mut state = self.group.lock().unwrap();
            // Sequence and predecessor come from the in-memory counters, never
            // from durable state -- see GroupState::next_seq for why.
            state.next_seq += 1;
            let seq = state.next_seq;
            let prev = state.last_id.clone().unwrap_or_else(|| "0".repeat(64));
            let at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let id = Entry::compute_id(seq, &prev, at, &operation)?;
            let entry = Entry {
                seq,
                prev,
                id: id.clone(),
                at_unix_ms: at,
                operation,
            };
            state.last_id = Some(id);
            state.pending.push(entry.clone());
            entry
        };

        self.flush_through(entry.seq)?;
        Ok(entry)
    }

    /// Make everything up to `seq` durable, coalescing with concurrent callers.
    fn flush_through(&self, seq: u64) -> Result<()> {
        let mut state = self.group.lock().unwrap();
        loop {
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
            let batch: Vec<Entry> = std::mem::take(&mut state.pending);
            drop(state);

            let result = self.commit_batch(&batch);

            state = self.group.lock().unwrap();
            state.flushing = false;
            match result {
                Ok(highest) => {
                    state.durable_through = state.durable_through.max(highest);
                    self.ready.notify_all();
                }
                Err(e) => {
                    // The batch did not land. The entries are NOT restaged:
                    // their sequence numbers are already spent, and reusing
                    // them against a partially committed transaction is the
                    // very overwrite this design exists to prevent. The failure
                    // propagates and the caller's operation is reported as not
                    // performed, which is the honest outcome -- the gap in
                    // sequence numbers is visible to `verify_chain`.
                    self.ready.notify_all();
                    return Err(e);
                }
            }
        }
    }

    fn commit_batch(&self, batch: &[Entry]) -> Result<u64> {
        if batch.is_empty() {
            return Ok(0);
        }
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(ENTRIES)?;
            for entry in batch {
                let bytes = serde_json::to_vec(entry)?;
                table.insert(entry.seq, bytes.as_slice())?;
            }
        }
        // redb's commit is the durability barrier; it fsyncs.
        tx.commit()?;
        Ok(batch.last().map(|e| e.seq).unwrap_or(0))
    }

    /// Walk the hash chain. Returns the first break, or None if intact.
    ///
    /// This is what makes "append-only" checkable rather than asserted: an
    /// edited historical entry changes its own id, which no longer matches the
    /// `prev` its successor recorded.
    pub fn verify_chain(&self) -> Result<Option<String>> {
        let mut expected_prev = "0".repeat(64);
        for entry in self.entries()? {
            if entry.prev != expected_prev {
                return Ok(Some(format!(
                    "operation {} links to {} but its predecessor hashes to {}",
                    entry.seq,
                    &entry.prev[..12],
                    &expected_prev[..12]
                )));
            }
            let recomputed =
                Entry::compute_id(entry.seq, &entry.prev, entry.at_unix_ms, &entry.operation)?;
            if recomputed != entry.id {
                return Ok(Some(format!(
                    "operation {} has been altered: its content hashes to {} \
                     but it records {}",
                    entry.seq,
                    &recomputed[..12],
                    &entry.id[..12]
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

// A store handle is safe to share: redb serialises its own writers, and the
// group state is behind a mutex.
unsafe impl Sync for OpLog {}
unsafe impl Send for OpLog {}

impl std::fmt::Debug for OpLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpLog").finish_non_exhaustive()
    }
}

#[allow(dead_code)]
fn unused(_: Error) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn log() -> (tempfile::TempDir, OpLog) {
        let dir = tempfile::tempdir().unwrap();
        let log = OpLog::open(&dir.path().join("oplog.redb")).unwrap();
        (dir, log)
    }

    #[test]
    fn appends_are_sequential_and_chained() {
        let (_d, log) = log();
        let a = log.append(Operation::Init).unwrap();
        let b = log
            .append(Operation::StartLine {
                name: "auth".into(),
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
            checkpoint: "c".into()
        }
        .is_undoable());
        assert!(
            Operation::Undo { undone_seq: 1 }.is_undoable(),
            "undo of undo is redo, and must remain available"
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
