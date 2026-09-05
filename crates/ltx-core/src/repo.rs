//! The repository: the one public entry point to everything.
//!
//! §8 constrains the shape — "the CLI contains no logic the API lacks" — so
//! every operation lives here as a method and the CLI is a thin translation of
//! argv into these calls. G5.5 measures that as a HARD gate, and the way to
//! pass it is to never write logic anywhere else.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::change;
use crate::chunk::ChunkId;
use crate::error::{Error, Result};
use crate::oplog::{
    CheckpointedChange, Entry, LineRecord, LineState, OpLog, Operation, DEFAULT_LINE,
    FORMAT_VERSION, MIN_READABLE_FORMAT,
};
use crate::platform;
use crate::store::{PackWriter, Store};
use crate::tree::{self, Node, Tree};

pub const REPO_DIR: &str = ".lattice";

/// An immutable, durable snapshot — the unit of history (§4.2, noun 3).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Checkpoint {
    pub id: String,
    pub tree: String,
    pub message: String,
    pub parent: Option<String>,
    pub at_unix_ms: u64,
    /// The op-log sequence that created this. A back-reference resolved from
    /// the op-log at read time, NOT part of the checkpoint's identity — see
    /// `body_id`. The stored blob always carries 0.
    pub oplog_seq: u64,
}

impl Checkpoint {
    /// The content address that authenticates this checkpoint: the hash of its
    /// body — tree, message, parent, and timestamp — and nothing else.
    ///
    /// `oplog_seq` is deliberately excluded. It is a back-reference resolved
    /// from the op-log at read time; folding it into the identity would force
    /// the blob to be rewritten once the sequence is known (a crash window),
    /// and, worse, letting the stored `id` field be trusted directly lets any
    /// ordinary file whose bytes deserialise as a Checkpoint impersonate one.
    fn body_id(&self) -> Result<String> {
        let body = serde_json::to_vec(&(&self.tree, &self.message, &self.parent, self.at_unix_ms))?;
        Ok(ChunkId::of(&body).to_hex())
    }

    /// True iff the blob's declared `id` actually hashes its body. A blob that
    /// fails this is not a checkpoint, whatever its `id` field claims.
    fn is_authentic(&self) -> bool {
        matches!(self.body_id(), Ok(computed) if computed == self.id)
    }
}

/// What `verify` found.
///
/// Challenge 4: `verify` on a partial clone cannot walk content it does not
/// hold, so it reports coverage explicitly and only `--complete` may print an
/// unqualified "verified". The `complete` flag exists so a caller cannot
/// mistake one for the other without ignoring a field named `complete`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifyReport {
    pub structure_verified: bool,
    pub chunks_verified: u64,
    pub chunks_absent: u64,
    /// How many checkpoints record only part of the working state that stood
    /// when they were written.
    ///
    /// Not damage, and not an error — these checkpoints are exactly what was
    /// asked for. It is here because "verified" reads as "everything I have is
    /// safe in history", and after a partial save that is not what the word
    /// can mean (ADR-17, consequences).
    pub checkpoints_partial: u64,
    pub checkpoints: u64,
    pub oplog_entries: u64,
    pub complete: bool,
    pub errors: Vec<String>,
}

/// Debug prints the root only. The store and op-log are large and their
/// contents are the repository's data, which has no business in a log line.
impl std::fmt::Debug for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repo")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

pub struct Repo {
    root: PathBuf,
    store: Store,
    oplog: OpLog,
    change_id_bits: ChangeIdBits,
}

/// Where a new change id gets its 128 bits (ADR-17 §3).
///
/// Injected rather than reached for: the engine never touches an entropy
/// source itself, so a test or the property runner substitutes a counter and
/// exercises the code that ships rather than a second path beside it.
/// Fallible because a machine that cannot produce entropy has to say so, not
/// mint a predictable identity.
pub type ChangeIdBits = Box<dyn FnMut() -> Result<[u8; 16]> + Send>;

fn os_change_id_bits() -> Result<[u8; 16]> {
    let mut bits = [0u8; 16];
    getrandom::fill(&mut bits).map_err(|e| Error::Io(std::io::Error::from(e)))?;
    Ok(bits)
}

/// The most paths one change may hold.
///
/// Nothing from outside is unbounded, and this set is unusual in being
/// published into the line state on EVERY operation — so an uncapped one would
/// grow every write in the repository, not just its own. Reaching it is a
/// refusal, reported and exiting 0, never an error: the paths already in the
/// change stay assigned.
///
/// When hunk-level assignment lands, assignment sets move to content-addressed
/// blobs referenced by address — the pattern `working` already uses — and this
/// stops bounding a redb value (ADR-17 §4).
const MAX_ASSIGNED_PATHS: usize = 1000;

impl Repo {
    fn repo_dir(root: &Path) -> PathBuf {
        root.join(REPO_DIR)
    }

    /// Create a repository here.
    pub fn init(root: &Path) -> Result<Self> {
        let dir = Self::repo_dir(root);
        if dir.exists() {
            // An existing `.lattice` that records NO operation is an init that
            // was interrupted before it could commit — not a repository. Left
            // alone it is a directory with no way forward, since `open` refuses
            // it for having no format and `init` refuses it for existing. So
            // init RESUMES it; only a directory with real history is refused.
            let probe = OpLog::open(&dir.join("meta.redb"))?;
            if !probe.is_empty()? {
                return Err(Error::Invalid(format!(
                    "{} already contains a Lattice repository",
                    root.display()
                )));
            }
            drop(probe);
        }
        fs::create_dir_all(dir.join("packs"))?;
        let store = Store::open(&dir.join("packs"))?;
        let oplog = OpLog::open(&dir.join("meta.redb"))?;
        // The Init entry, the default line and the format version land in ONE
        // transaction, so an interrupted init cannot leave a `.lattice` that
        // `open` refuses (no format) and `init` also refuses (already exists) —
        // a directory with no way forward.
        //
        // The default line is published here, NOT by a StartLine entry: such an
        // entry would be an eligible undo target and undo-all would delete
        // `main` (ADR-16 §2). `Init` is not undoable, so `main` sits below the
        // undo floor.
        oplog.commit_initial(Operation::Init, LineState::initial(), FORMAT_VERSION)?;
        // redb's create syncs `.lattice` itself, but the entry FOR `.lattice`
        // lives in `root` and is only committed once `root` is fsynced —
        // otherwise a crash can lose the whole repository directory after init
        // reported success.
        platform::sync_dir(root)?;
        Ok(Repo {
            root: root.to_path_buf(),
            store,
            oplog,
            change_id_bits: Box::new(os_change_id_bits),
        })
    }

    /// Find the repository containing `start`, walking upward.
    pub fn discover(start: &Path) -> Result<Self> {
        let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        let mut cursor = start.as_path();
        loop {
            if Self::repo_dir(cursor).is_dir() {
                return Self::open(cursor);
            }
            match cursor.parent() {
                Some(p) => cursor = p,
                None => return Err(Error::NotARepository(start.clone())),
            }
        }
    }

    pub fn open(root: &Path) -> Result<Self> {
        let dir = Self::repo_dir(root);
        if !dir.is_dir() {
            return Err(Error::NotARepository(root.to_path_buf()));
        }
        let store = Store::open(&dir.join("packs"))?;
        let oplog = OpLog::open(&dir.join("meta.redb"))?;
        // A RANGE, not an exact match. Formats 1 and 2 predate the per-entry
        // format tag, so their entries' original serialisation cannot be
        // reproduced and `verify_chain` could only report phantom tampering —
        // those are refused with a way forward (ADR-16 §9, ADR-17 §9). From 3
        // on, `Entry::compute_id` dispatches on the tag, so a log written by an
        // older build inside that range still verifies and is opened normally.
        //
        // A format NEWER than this build is refused too: its entries may use
        // variants this build cannot parse, and guessing at them is how a
        // reader corrupts a repository it did not understand.
        match oplog.format_version()? {
            Some(v) if (MIN_READABLE_FORMAT..=FORMAT_VERSION).contains(&v) => {}
            other => {
                return Err(Error::UnsupportedFormat(format!(
                    "this repository is on-disk format {} but this build reads formats {}–{}",
                    other
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "1 (unversioned)".into()),
                    MIN_READABLE_FORMAT,
                    FORMAT_VERSION
                )))
            }
        }
        Ok(Repo {
            root: root.to_path_buf(),
            store,
            oplog,
            change_id_bits: Box::new(os_change_id_bits),
        })
    }

    /// Replace where new change ids get their bits.
    ///
    /// The one seam ADR-17 §3 asks for: entropy enters at this boundary and
    /// nowhere else, so a caller that needs a run to be reproducible passes a
    /// counter instead of the machine's entropy.
    pub fn with_change_id_bits(
        mut self,
        bits: impl FnMut() -> Result<[u8; 16]> + Send + 'static,
    ) -> Self {
        self.change_id_bits = Box::new(bits);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn oplog(&self) -> &OpLog {
        &self.oplog
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    // ---------------------------------------------------------------- save

    /// Promote the working state to a checkpoint.
    ///
    /// One step. §4.3 makes that an invariant — there is no staging area to
    /// pass through, and the partial-save capability the index existed to serve
    /// is a property of *changes*, not a gate every save must pass.
    pub fn save(&mut self, message: &str, change: Option<&str>) -> Result<SaveOutcome> {
        let rescued = self.complete_pending_switch()?;
        let mut lines = self.line_state()?;
        let current = lines.current.clone();

        // Settle the scope before a byte is written. A `--change` that names
        // no open change, or one holding nothing, must leave the repository
        // exactly as it found it — ADR-17 §5 requires a save that refuses to
        // append nothing, and a store full of orphaned chunks is not nothing.
        let scope = match change {
            None => None,
            Some(typed) => {
                let record = lines.lines.entry(current.clone()).or_default();
                let id = resolve_change(typed, record, &current, &self.oplog)?;
                let held = record.changes.get(&id).cloned().unwrap_or_default();
                if held.assigned.is_empty() {
                    return Err(Error::InvalidChange(format!(
                        "change {} holds no paths, so there is nothing of it to \
                         checkpoint",
                        change::abbreviate(
                            &id,
                            &record
                                .changes
                                .keys()
                                .map(String::as_str)
                                .collect::<Vec<_>>()
                        )
                    )));
                }
                Some((id, held))
            }
        };

        let mut packer = PackWriter::new();
        // The whole working tree becomes durable content either way. A partial
        // save needs this as much as a full one does: it already walks the
        // tree, so the part it does NOT checkpoint becomes content-addressed
        // at no extra cost rather than having no durable address at all until
        // the next switch (ADR-17 §5).
        let working_tree = self.snapshot_dir(&self.root.clone(), &mut packer)?;

        let parent = self.head_checkpoint()?;
        let tree_id = match &scope {
            None => working_tree.clone(),
            Some((_, held)) => {
                let components: Vec<Vec<Vec<u8>>> = held
                    .assigned
                    .iter()
                    .map(|path| path.split(|b| *b == b'/').map(<[u8]>::to_vec).collect())
                    .collect();
                let paths: Vec<&[Vec<u8>]> = components.iter().map(Vec::as_slice).collect();
                let base = parent.as_ref().map(|c| c.tree.clone());
                let root = self.root.clone();
                self.splice_paths(base.as_deref(), &root, &paths, &mut packer)?
            }
        };
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut checkpoint = Checkpoint {
            id: String::new(),
            tree: tree_id,
            message: message.to_string(),
            parent: parent.as_ref().map(|c| c.id.clone()),
            at_unix_ms: at,
            oplog_seq: 0,
        };
        checkpoint.id = checkpoint.body_id()?;

        // Content — chunks, trees, and the checkpoint blob that names them — is
        // made durable BEFORE the op-log entry that references the checkpoint
        // id (ADR-3). A crash before the append leaves unreferenced blobs that
        // recovery discards; a crash AFTER would leave an op-log entry naming a
        // checkpoint whose blob was never written. So the blob comes first, and
        // is written exactly once — `oplog_seq` is not part of its identity, so
        // there is nothing to fill in afterward.
        let checkpoint_bytes = serde_json::to_vec(&checkpoint)?;
        packer.add(ChunkId::of(&checkpoint_bytes), &checkpoint_bytes);
        // Write only content the store does not already hold. Unchanged files
        // and unchanged subtrees are already durable in earlier packs; a save
        // that re-stored them would re-persist the whole working tree on every
        // one-byte edit.
        packer.retain_unknown(&self.store);
        self.store.write_pack(packer)?;

        let record = lines.lines.entry(current.clone()).or_default();
        record.tip = Some(checkpoint.id.clone());
        // A partial save CONSUMES the change it checkpointed: it is no longer
        // pending, and a bare `assign` must not go on adding to something
        // already checkpointed. A save of the whole working state consumes
        // nothing — assignment is a labelling, never a gate (ADR-17 §5).
        let consumed = scope.map(|(id, held)| {
            let was_current = record.current_change.as_deref() == Some(id.as_str());
            record.changes.remove(&id);
            if was_current {
                record.current_change = None;
            }
            CheckpointedChange {
                id,
                record: held,
                was_current,
            }
        });
        let entry = self.oplog.commit(
            Operation::Save {
                message: message.to_string(),
                checkpoint: checkpoint.id.clone(),
                line: current,
                change: consumed.clone(),
            },
            Some(lines),
        )?;
        self.write_head_pointer(&checkpoint.id)?;

        // The caller's copy carries the real sequence; reads resolve the same
        // value from the op-log, so the stored blob's 0 is never observed.
        checkpoint.oplog_seq = entry.seq;
        Ok(SaveOutcome {
            checkpoint,
            change: consumed.map(|c| c.id),
            working_state: working_tree,
            rescued_working_state: rescued,
        })
    }

    fn head_pointer_path(&self) -> PathBuf {
        Self::repo_dir(&self.root).join("HEAD")
    }

    fn write_head_pointer(&self, checkpoint_id: &str) -> Result<()> {
        let path = self.head_pointer_path();
        let tmp = path.with_extension("tmp");
        // The tmp file's CONTENTS must be durable BEFORE the rename that
        // publishes it. `fs::write` only reaches the page cache; a crash after
        // the rename but before the data reached disk leaves a rename committed
        // over a zero-length (or zero-filled) file, which is the classic
        // "rename gave me an empty file" outcome on XFS/btrfs/APFS. So we fsync
        // the tmp handle first, exactly as the pack store does for its files.
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(checkpoint_id.as_bytes())?;
            f.sync_all()?;
        }
        // Atomic replace, then sync the directory so the rename itself is
        // durable — a rename is only committed once the containing directory
        // is fsynced, which is the barrier G1.1's replayer models.
        fs::rename(&tmp, &path)?;
        platform::sync_dir(&Self::repo_dir(&self.root))?;
        Ok(())
    }

    /// The published line state, synthesised for a repository that has none.
    ///
    /// Head-resolution ladder, most durable first (ADR-16 §7). Each rung has
    /// exactly one authority, and the file is always last because it is a cache
    /// written AFTER the transaction — only ever equal to or staler than redb,
    /// never fresher. Letting it win would resurrect an undone checkpoint or
    /// mask a committed one after a crash.
    ///   1. `LINES["state"]` — authoritative.
    ///   2. `HEADS["checkpoint"]` — read-only legacy rung, for a database
    ///      written by an earlier build before lines existed.
    ///   3. `.lattice/HEAD` — human-readable cache.
    pub fn line_state(&self) -> Result<LineState> {
        if let Some(state) = self.oplog.line_state()? {
            return Ok(state);
        }
        let mut state = LineState::initial();
        if let Some(rec) = state.lines.get_mut(DEFAULT_LINE) {
            rec.tip = self.legacy_head_id()?;
        }
        Ok(state)
    }

    /// Rungs 2 and 3 of the ladder, for a repository with no published lines.
    fn legacy_head_id(&self) -> Result<Option<String>> {
        if let Some(seq) = self.oplog.get_head("checkpoint")? {
            if let Some(entry) = self.oplog.get(seq)? {
                if let Operation::Save { checkpoint, .. } = entry.operation {
                    return Ok(Some(checkpoint));
                }
            }
        }
        match fs::read_to_string(self.head_pointer_path()) {
            Ok(raw) => {
                let id = raw.trim();
                if ChunkId::from_hex(id).is_some() {
                    return Ok(Some(id.to_string()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // A permission or I/O failure must not read as "nothing saved yet".
            Err(e) => return Err(e.into()),
        }
        Ok(None)
    }

    /// The checkpoint the CURRENT line points at.
    pub fn head_checkpoint(&self) -> Result<Option<Checkpoint>> {
        let state = self.line_state()?;
        let Some(tip) = state.lines.get(&state.current).and_then(|r| r.tip.clone()) else {
            return Ok(None);
        };
        self.checkpoint(&tip)
    }

    pub fn checkpoint(&self, id: &str) -> Result<Option<Checkpoint>> {
        if ChunkId::from_hex(id).is_none() {
            return Err(Error::Invalid(format!("{id} is not a checkpoint address")));
        };
        // A checkpoint is content-addressed like everything else, but its own
        // address is over its body rather than its serialised form, so the
        // lookup is by scanning the addresses we know. Small and adequate for
        // the current history sizes; a checkpoint index is a later refinement.
        for candidate in self.store.all_chunk_ids() {
            if let Some(bytes) = self.store.read(candidate)? {
                if let Ok(mut cp) = serde_json::from_slice::<Checkpoint>(&bytes) {
                    // Authenticate before trusting: the blob's declared id must
                    // be the one asked for AND must actually hash its body.
                    // Without the second check, any ordinary file whose bytes
                    // deserialise as a Checkpoint could impersonate one.
                    if cp.id == id && cp.is_authentic() {
                        cp.oplog_seq = self.oplog_seq_for(id)?.unwrap_or(0);
                        return Ok(Some(cp));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Op-log sequence of the Save that created a checkpoint, if any. This is
    /// the authoritative back-reference; it is never stored in the blob.
    fn oplog_seq_for(&self, checkpoint_id: &str) -> Result<Option<u64>> {
        self.oplog.save_seq(checkpoint_id)
    }

    /// The change a checkpoint took, when it took only one.
    ///
    /// Resolved from the `Save` entry at read time, exactly as `oplog_seq` is:
    /// the association is provenance recorded by an operation, not content, so
    /// the checkpoint blob carries no field for it and its address does not
    /// depend on it (ADR-17 §7).
    fn change_a_checkpoint_took(&self, checkpoint_id: &str) -> Result<Option<String>> {
        let Some(seq) = self.oplog.save_seq(checkpoint_id)? else {
            return Ok(None);
        };
        Ok(self.oplog.entry(seq)?.and_then(|e| match e.operation {
            Operation::Save { change, .. } => change.map(|c| c.id),
            _ => None,
        }))
    }

    /// Every checkpoint, newest first.
    ///
    /// The op-log is the authority on which checkpoints exist and in what
    /// order; the store merely holds their blobs. A blob is a checkpoint only
    /// if some Save entry references its id and its body authenticates — so a
    /// file that merely looks like a checkpoint never appears here.
    pub fn checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let mut seq_by_id: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for entry in self.oplog.entries()? {
            if let Operation::Save { checkpoint, .. } = &entry.operation {
                seq_by_id.insert(checkpoint.clone(), entry.seq);
            }
        }

        let mut out: Vec<Checkpoint> = Vec::new();
        for candidate in self.store.all_chunk_ids() {
            if let Some(bytes) = self.store.read(candidate)? {
                if let Ok(mut cp) = serde_json::from_slice::<Checkpoint>(&bytes) {
                    if let Some(&seq) = seq_by_id.get(&cp.id) {
                        if cp.is_authentic() {
                            cp.oplog_seq = seq;
                            out.push(cp);
                        }
                    }
                }
            }
        }
        out.sort_by_key(|c| std::cmp::Reverse(c.oplog_seq));
        out.dedup_by(|a, b| a.id == b.id);
        Ok(out)
    }

    /// Checkpoints reachable from the current head, newest first.
    ///
    /// This is the checkpoint graph as `undo` sees it (ADR-15): undo moves the
    /// head to a checkpoint's parent, so an undone checkpoint leaves this set
    /// while remaining immutably in the store. `checkpoints()` — every saved
    /// checkpoint ever — still backs `verify` and `status`; only the history
    /// view walks from the head, so that it is invariant under undo-all.
    pub fn reachable_checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let mut out = Vec::new();
        let mut cursor = self.head_checkpoint()?;
        // A checkpoint's id hashes its parent, so a parent chain cannot form a
        // cycle; the walk is still bounded — nothing from outside is unbounded.
        let mut guard = 0usize;
        while let Some(cp) = cursor {
            guard += 1;
            if guard > 10_000_000 {
                return Err(Error::Corrupt(
                    "checkpoint parent chain exceeds the sane bound".into(),
                ));
            }
            let parent = cp.parent.clone();
            let cp_id = cp.id.clone();
            out.push(cp);
            cursor = match parent {
                None => None,
                Some(pid) => match self.checkpoint(&pid)? {
                    Some(parent) => Some(parent),
                    // A named parent that does not resolve is corruption — a
                    // hole in the history spine — not a silent end of the walk.
                    // `undo` treats the same condition the same way.
                    None => {
                        return Err(Error::Corrupt(format!(
                            "checkpoint {} names a parent {} that is not present",
                            crate::short_id(&cp_id),
                            crate::short_id(&pid)
                        )))
                    }
                },
            };
        }
        Ok(out)
    }

    /// The `log` view: the current line's history, or — with `forensic` — the
    /// union of every live line's history.
    ///
    /// The union matters (ADR-16 §8): G1.1 asserts no baseline checkpoint
    /// disappears, and with only the current line's chain an ordinary `switch`
    /// would look exactly like checkpoint loss. The union makes "no
    /// checkpointed data is ever lost" true by construction in the array the
    /// HARD gate actually reads.
    pub fn log_view(&self, forensic: bool, limit: Option<usize>) -> Result<Vec<Checkpoint>> {
        if !forensic {
            return self.history(limit);
        }
        // A limit of zero asks for nothing; the frontier loop below pushes
        // before it checks, so it would otherwise return one. `history` already
        // answers this correctly and the two views must agree.
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let state = self.line_state()?;
        // With a limit, resolve lazily and memoised: the caller asked for a
        // handful, so materialising every checkpoint in the repository first
        // would be unbounded work for a bounded request. Without one we are
        // returning everything anyway, and a single indexing pass beats
        // re-scanning the store per ancestry step.
        let lazy = limit.is_some();
        let mut by_id: std::collections::BTreeMap<String, Checkpoint> = if lazy {
            std::collections::BTreeMap::new()
        } else {
            self.checkpoints()?
                .into_iter()
                .map(|c| (c.id.clone(), c))
                .collect()
        };

        // An ordered frontier over every live line tip, expanded newest-first
        // ACROSS all lines. Walking one line to the limit and then stopping
        // would return that line's N while omitting newer checkpoints on
        // another — the limited view has to be the newest N of the whole
        // history, not of whichever line was visited first.
        let mut frontier: std::collections::BTreeMap<(u64, String), Checkpoint> =
            std::collections::BTreeMap::new();
        for rec in state.lines.values() {
            if let Some(tip) = rec.tip.clone() {
                let cp = self.resolve_cached(&tip, lazy, &mut by_id)?;
                frontier.insert((cp.oplog_seq, cp.id.clone()), cp);
            }
        }

        let mut out: Vec<Checkpoint> = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        while let Some(key) = frontier.keys().next_back().cloned() {
            let cp = frontier.remove(&key).expect("key came from this map");
            if !seen.insert(cp.id.clone()) {
                continue;
            }
            let parent = cp.parent.clone();
            out.push(cp);
            if limit.is_some_and(|n| out.len() >= n) {
                break;
            }
            if let Some(pid) = parent {
                let p = self.resolve_cached(&pid, lazy, &mut by_id)?;
                frontier.insert((p.oplog_seq, p.id.clone()), p);
            }
        }
        Ok(out)
    }

    /// Resolve a checkpoint id, memoising into `by_id`.
    ///
    /// A referenced id that does not resolve is a hole in the spine, not the
    /// end of a walk — returning a silently shorter history is exactly what the
    /// gate reading this array must never see.
    fn resolve_cached(
        &self,
        id: &str,
        lazy: bool,
        by_id: &mut std::collections::BTreeMap<String, Checkpoint>,
    ) -> Result<Checkpoint> {
        if lazy && !by_id.contains_key(id) {
            // Require the same Save reference `checkpoints()` requires, so the
            // lazy path cannot admit a self-consistent blob that history never
            // recorded — asked as ONE indexed lookup, so a bounded query never
            // loads the whole op-log to answer it.
            if self.oplog.save_seq(id)?.is_some() {
                if let Some(cp) = self.checkpoint(id)? {
                    by_id.insert(id.to_string(), cp);
                }
            }
        }
        by_id.get(id).cloned().ok_or_else(|| {
            Error::Corrupt(format!(
                "checkpoint {} is referenced but not present",
                crate::short_id(id)
            ))
        })
    }

    /// The history the user sees: reachable checkpoints, newest first,
    /// optionally capped at `limit`. The default-and-limit policy lives here
    /// rather than in the CLI (§8) — the CLI and the daemon API share it.
    pub fn history(&self, limit: Option<usize>) -> Result<Vec<Checkpoint>> {
        let all = self.reachable_checkpoints()?;
        Ok(match limit {
            Some(n) => all.into_iter().take(n).collect(),
            None => all,
        })
    }

    // -------------------------------------------------------------- undo

    /// Reverse the most recent undoable operation on user-visible state.
    ///
    /// For the operations that exist today this is a save: undo moves the head
    /// to the saved checkpoint's parent and appends an `Undo` record. At the
    /// root checkpoint (no parent) there is nothing to undo and no record is
    /// appended — ADR-15 explains why the root is the floor. The op-log entry
    /// is written before the head moves, mirroring save's durable order.
    /// Reverse the most recent undoable operation on user-visible state.
    ///
    /// Selects the highest-seq LIVE, ELIGIBLE op-log entry and applies its
    /// kind-specific inverse (ADR-16 §4). Three invariants make this correct:
    ///
    /// * **Scan candidates, not entries.** After any undo the newest entry is
    ///   an `Undo`, so "reverse the last operation" must mean the last *live
    ///   candidate*.
    /// * **LIFO over the live set.** Always taking the maximum means every live
    ///   entry above the target is already reversed, so the state equals the
    ///   state just after the target was applied — which is what licenses these
    ///   local inverses with no conflict analysis.
    /// * **Undoing a `start` must not orphan checkpoints**, so it is eligible
    ///   only while its line still points where its origin points. Eligibility
    ///   therefore reads current line state.
    ///
    /// Termination does not rest on eligibility: each undo appends an `Undo`
    /// naming its target, and `undone` is append-only, so that entry leaves the
    /// live set permanently. `|live|` is finite and strictly decreases by one
    /// per undo, so undo-all ends and `nothing_to_undo` is absorbing. Changing
    /// eligibility moves only WHERE the floor is, never whether the walk ends.
    pub fn undo(&mut self) -> Result<UndoOutcome> {
        let rescued = self.complete_pending_switch()?;
        let Some(target) = self.next_undo_target()? else {
            let mut out = UndoOutcome::nothing();
            out.rescued_working_state = rescued;
            return Ok(out);
        };
        let mut out = self.reverse(&target)?;
        out.rescued_working_state = rescued;
        Ok(out)
    }

    /// The highest-seq live, eligible entry, or None at the floor.
    fn next_undo_target(&self) -> Result<Option<Entry>> {
        let entries = self.oplog.entries()?;
        let undone: std::collections::HashSet<u64> = entries
            .iter()
            .filter_map(|e| match &e.operation {
                Operation::Undo { undone_seq } => Some(*undone_seq),
                _ => None,
            })
            .collect();
        // Changes a standing save has already checkpointed, per line — a
        // change id can exist on two lines at once, since `start` copies the
        // open changes along with the bytes they label.
        let consumed: std::collections::HashSet<(&str, &str)> = entries
            .iter()
            .filter(|e| !undone.contains(&e.seq))
            .filter_map(|e| match &e.operation {
                Operation::Save {
                    line,
                    change: Some(c),
                    ..
                } => Some((line.as_str(), c.id.as_str())),
                _ => None,
            })
            .collect();
        let lines = self.line_state()?;
        for entry in entries.iter().rev() {
            if !entry.operation.is_undoable() || undone.contains(&entry.seq) {
                continue;
            }
            if self.is_eligible(&entry.operation, &lines, &consumed)? {
                return Ok(Some(entry.clone()));
            }
        }
        Ok(None)
    }

    /// Whether an operation still has something to reverse.
    ///
    /// Depends only on immutable data. A save at the root of its line has no
    /// parent to return to — that is the floor ADR-15 established, expressed
    /// per-candidate rather than against the mutable head.
    fn is_eligible(
        &self,
        op: &Operation,
        lines: &LineState,
        consumed: &std::collections::HashSet<(&str, &str)>,
    ) -> Result<bool> {
        Ok(match op {
            Operation::Save { checkpoint, .. } => match self.checkpoint(checkpoint)? {
                Some(cp) => cp.parent.is_some(),
                None => false,
            },
            // Removing a line must not orphan checkpoints only it can reach.
            // If it still points somewhere its origin does not, the saves on it
            // must be reversed first — and if those are themselves at the root
            // floor, so is this. Otherwise `init; start L; save; undo` would
            // drop that checkpoint out of every line and out of every view.
            Operation::StartLine {
                name,
                from,
                created: true,
            } => {
                let tip = lines.lines.get(name).and_then(|r| r.tip.clone());
                let origin = lines.lines.get(from).and_then(|r| r.tip.clone());
                tip == origin
            }
            Operation::StartLine { .. } | Operation::Switch { .. } => true,
            // An assign is ineligible once a standing save has checkpointed a
            // change its inverse would write to (ADR-17 §8). `next_undo_target`
            // scans PAST an ineligible entry, and a save at the root of its
            // line is permanently ineligible for having no parent to return to
            // — so without this, `init; assign a.txt; save "x" --change c;
            // undo` reverses the assign underneath the standing checkpoint that
            // consumed it, and the change loses the assignments the line tip is
            // derived from. G1.3 cannot catch it: the harness always seeds a
            // save first, so every later checkpoint has a parent. Users hit it
            // on their first repository.
            //
            // The displaced changes are covered too, not just the target: the
            // inverse puts those paths BACK into the changes they came from,
            // and doing that to a change a save has consumed would resurrect it
            // as pending — leaving it simultaneously checkpointed and pending,
            // the ambiguity §4.2's noun collapse exists to remove.
            Operation::Assign {
                change,
                line,
                displaced,
                ..
            } => {
                !consumed.contains(&(line.as_str(), change.as_str()))
                    && !displaced
                        .iter()
                        .any(|(_, owner)| consumed.contains(&(line.as_str(), owner.as_str())))
            }
            // Everything else has no defined inverse. `Adopt` is also marked
            // non-undoable at the source so a core caller cannot append one and
            // have undo skip it in silence; defining its inverse belongs to the
            // adopt slice.
            //
            // Listed rather than caught by `_`, so that the compile-time
            // guarantee `reverse` documents holds for BOTH halves of undo. A
            // catch-all here would let a new undoable variant arrive with a
            // `reverse` arm and no eligibility arm: it would compile, become
            // permanently ineligible, and `ltx undo` would report
            // `nothing_to_undo` while its effect still stood.
            Operation::Init
            | Operation::Undo { .. }
            | Operation::Adopt { .. }
            | Operation::Redact { .. }
            | Operation::Thin { .. } => false,
        })
    }

    /// Apply the inverse of one entry. Exhaustive by kind, so a new undoable
    /// variant without an arm fails to compile — which is how §6's "no command
    /// silently dodges" is mechanised.
    fn reverse(&mut self, entry: &Entry) -> Result<UndoOutcome> {
        let mut lines = self.line_state()?;
        let mut outcome = UndoOutcome {
            nothing_to_undo: false,
            undone_checkpoint: None,
            now_at: None,
            undo_seq: None,
            rescued_working_state: None,
            preserved_working_state: None,
            remote_effects_not_undone: Vec::new(),
        };
        // What the working tree must look like afterwards, if it must change.
        let mut materialise: Option<Option<String>> = None;

        match &entry.operation {
            Operation::Save {
                checkpoint,
                line,
                change,
                ..
            } => {
                let cp = self.checkpoint(checkpoint)?.ok_or_else(|| {
                    Error::Corrupt(format!(
                        "operation {} saved checkpoint {} but its record is not present",
                        entry.seq,
                        crate::short_id(checkpoint)
                    ))
                })?;
                // Save never touched the working tree, so neither does its
                // inverse: this is pure tip movement.
                let rec = lines.lines.entry(line.clone()).or_default();
                rec.tip = cp.parent.clone();
                // A partial save consumed a change, which took it out of the
                // line state entirely. The entry is the only place its
                // assignments survive, so the inverse reads them from there.
                if let Some(consumed) = change {
                    rec.changes
                        .insert(consumed.id.clone(), consumed.record.clone());
                    if consumed.was_current {
                        rec.current_change = Some(consumed.id.clone());
                    }
                }
                outcome.undone_checkpoint = Some(cp.id.clone());
                outcome.now_at = cp.parent.clone();
            }
            Operation::StartLine {
                name,
                from,
                created,
            } => {
                let captured = self.capture_working_tree()?;
                if *created {
                    // The line being left is deleted, so there is nowhere in the
                    // line state to park the capture. It is still written to the
                    // store as durable content and named in the result, so the
                    // work is never destroyed — retrieval belongs to the
                    // ephemeral tier (ADR-16, open conflict 3).
                    lines.lines.remove(name);
                    outcome.preserved_working_state = Some(captured);
                } else {
                    lines.lines.entry(name.clone()).or_default().working = Some(captured);
                }
                let restored = lines.lines.entry(from.clone()).or_default().working.clone();
                lines.current = from.clone();
                materialise = Some(restored);
                outcome.now_at = lines.lines.get(from).and_then(|r| r.tip.clone());
            }
            Operation::Switch { from, to } => {
                if from != to {
                    let captured = self.capture_working_tree()?;
                    lines.lines.entry(to.clone()).or_default().working = Some(captured);
                    let restored = lines.lines.entry(from.clone()).or_default().working.clone();
                    lines.current = from.clone();
                    materialise = Some(restored);
                }
                outcome.now_at = lines.lines.get(from).and_then(|r| r.tip.clone());
            }
            Operation::Assign {
                change,
                line,
                paths,
                created,
                from_current,
                displaced,
            } => {
                // Assign wrote no byte of working state, so its inverse writes
                // none either (ADR-17 §2): there is no capture to take and
                // nothing to materialise, only labels to put back.
                let rec = lines.lines.entry(line.clone()).or_default();
                if *created {
                    rec.changes.remove(change);
                } else if let Some(record) = rec.changes.get_mut(change) {
                    for path in paths {
                        record.assigned.remove(path);
                    }
                }
                for (path, owner) in displaced {
                    // `entry` rather than `get_mut`: eligibility has already
                    // established that no standing save consumed this change,
                    // so this inserts into one that is there. Were it somehow
                    // not, restoring the assignment beats dropping it — undo
                    // must not fail here, because undo-all breaks on an error
                    // and would silently truncate.
                    rec.changes
                        .entry(owner.clone())
                        .or_default()
                        .assigned
                        .insert(path.clone());
                }
                rec.current_change = from_current.clone();
                outcome.now_at = rec.tip.clone();
            }
            // Listed rather than caught by `_`, so the exhaustiveness this
            // doc comment claims is real: a new undoable variant arriving with
            // an eligibility arm and no inverse would otherwise compile, and
            // `ltx undo` would exit non-zero on it — which breaks undo-all
            // mid-way rather than reporting nothing to undo (ADR-17 §8).
            Operation::Init
            | Operation::Undo { .. }
            | Operation::Adopt { .. }
            | Operation::Redact { .. }
            | Operation::Thin { .. } => {
                return Err(Error::Invalid(format!(
                    "operation {} ({}) has no inverse",
                    entry.seq,
                    entry.operation.name()
                )))
            }
        }

        let undo_entry = self.oplog.commit(
            Operation::Undo {
                undone_seq: entry.seq,
            },
            Some(lines.clone()),
        )?;
        outcome.undo_seq = Some(undo_entry.seq);

        if let Some(target) = materialise {
            // Same discipline as switch_to: the restored address stays in the
            // published state until the files are actually written, so a failed
            // materialisation cannot drop the only reference to that work. It
            // then reads as a pending switch, which the next command completes.
            self.materialise_working_tree(target.as_deref())?;
            let current = lines.current.clone();
            if let Some(rec) = lines.lines.get_mut(&current) {
                rec.working = None;
            }
            self.oplog.publish_lines(&lines)?;
        }
        self.sync_head_pointer(&lines)?;
        Ok(outcome)
    }

    // ------------------------------------------------------------ changes

    /// Mint an identity for a new change.
    fn new_change_id(&mut self) -> Result<String> {
        Ok(change::mint((self.change_id_bits)()?))
    }

    /// Route working-tree paths into a change.
    ///
    /// Paths resolve as the process resolves them, so a relative one is
    /// relative to the working directory — the contract `checkout --into`
    /// already follows.
    ///
    /// `to` names an existing change; `None` means the current one, creating
    /// it if the line has none. `--to` never creates, so there is exactly one
    /// creation path and an unknown id is an error rather than a new change
    /// nobody asked for (ADR-17 §6).
    ///
    /// Records an intent. Not a byte of working state is read or written —
    /// which is what keeps ADR-16 §1's linchpin true, and why the inverse
    /// needs no capture and no materialisation.
    ///
    /// A path this cannot take is REFUSED, never silently dropped, and a
    /// refusal is not an error: the call succeeds carrying what it refused and
    /// why. G1.4 draws `assign .` about ten thousand times and counts any
    /// non-zero exit as a failure, so a refusal that exited non-zero would
    /// fail a HARD gate for doing the right thing.
    pub fn assign(&mut self, paths: &[PathBuf], to: Option<&str>) -> Result<AssignOutcome> {
        let rescued = self.complete_pending_switch()?;
        let (candidates, mut refused) = self.locate(paths)?;
        let mut lines = self.line_state()?;
        let line = lines.current.clone();
        let record = lines.lines.entry(line.clone()).or_default();
        let from_current = record.current_change.clone();

        // Which change holds each path now, so a previous owner is a lookup
        // rather than a scan of every change for every path.
        let mut owner_of: std::collections::HashMap<Vec<u8>, String> =
            std::collections::HashMap::new();
        for (id, held) in record.changes.iter() {
            for path in &held.assigned {
                owner_of.insert(path.clone(), id.clone());
            }
        }

        // The target is settled before anything moves, so a `--to` that names
        // no open change leaves the repository exactly as it found it.
        let (change, created) = match to {
            Some(typed) => (resolve_change(typed, record, &line, &self.oplog)?, false),
            None => match &record.current_change {
                Some(current) => (current.clone(), false),
                // A bare assign with no current change starts one — even if
                // every path is then refused, which leaves an empty change
                // that `undo` removes. One rule; the alternative is an entry
                // naming a change that was never created.
                None => (self.new_change_id()?, true),
            },
        };

        let record = lines.lines.entry(line.clone()).or_default();
        let mut room = MAX_ASSIGNED_PATHS
            .saturating_sub(record.changes.get(&change).map_or(0, |c| c.assigned.len()));
        let mut moved: Vec<Vec<u8>> = Vec::new();
        let mut displaced: Vec<(Vec<u8>, String)> = Vec::new();
        for path in candidates {
            let held_by = owner_of.get(&path);
            // Already where it is being sent: nothing moved, so nothing is
            // recorded and nothing would be reversed.
            if held_by.is_some_and(|owner| *owner == change) {
                continue;
            }
            if room == 0 {
                refused.push(Refusal::capped(&path));
                continue;
            }
            if let Some(owner) = held_by {
                if let Some(previous) = record.changes.get_mut(owner) {
                    previous.assigned.remove(&path);
                }
                displaced.push((path.clone(), owner.clone()));
            }
            record
                .changes
                .entry(change.clone())
                .or_default()
                .assigned
                .insert(path.clone());
            moved.push(path);
            room -= 1;
        }
        record.current_change = Some(change.clone());
        let short = change::abbreviate(
            &change,
            &record
                .changes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );

        // One entry however many paths moved. G1.3's undo budget is
        // `applied * 3 + 8`, so a k-path assign decomposing into k entries
        // would exhaust it (ADR-17 §6).
        let entry = self.oplog.commit(
            Operation::Assign {
                change: change.clone(),
                line: line.clone(),
                paths: moved.clone(),
                created,
                from_current,
                displaced,
            },
            Some(lines),
        )?;

        Ok(AssignOutcome {
            change,
            short,
            created,
            line,
            assigned: moved.iter().map(|p| display_path(p)).collect(),
            refused,
            oplog_seq: entry.seq,
            rescued_working_state: rescued,
        })
    }

    /// Every change open on the current line.
    ///
    /// Carries no timestamp, no counter and no op-log position, so the
    /// document is invariant under apply-a-batch-then-undo-all — which is what
    /// G1.3 compares it for.
    pub fn changes(&self) -> Result<Vec<ChangeView>> {
        let lines = self.line_state()?;
        let Some(record) = lines.lines.get(&lines.current) else {
            return Ok(Vec::new());
        };
        let ids: Vec<&str> = record.changes.keys().map(String::as_str).collect();
        Ok(record
            .changes
            .iter()
            .map(|(id, held)| ChangeView {
                short: change::abbreviate(id, &ids),
                id: id.clone(),
                assigned: held.assigned.iter().map(|p| display_path(p)).collect(),
                current: record.current_change.as_deref() == Some(id.as_str()),
            })
            .collect())
    }

    /// Expand what the user named into working-tree paths, with a reason for
    /// each one that cannot be taken.
    ///
    /// The repository's own directory is skipped at the root and nowhere else,
    /// and no symlink is followed while walking — both the same rules
    /// `snapshot_dir` follows, because these are the paths a tree will name.
    fn locate(&self, given: &[PathBuf]) -> Result<(Vec<Vec<u8>>, Vec<Refusal>)> {
        let root = resolve_as_far_as_it_exists(&self.root)?;
        let mut found: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
        let mut refused = Vec::new();
        for path in given {
            // A named symlink resolves to what it points at, so `assign link`
            // assigns the file rather than the link. Walking a DIRECTORY does
            // not resolve: symlinks found inside are assigned as themselves,
            // exactly as a tree records them.
            let resolved = resolve_as_far_as_it_exists(path)?;
            if !resolved.starts_with(&root) {
                refused.push(Refusal::outside(path));
                continue;
            }
            match fs::symlink_metadata(&resolved) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                    self.collect_under(&resolved, &root, &mut found, &mut refused)?;
                }
                Ok(_) => {
                    if let Some(relative) = resolved
                        .strip_prefix(&root)
                        .ok()
                        .and_then(relative_path_bytes)
                    {
                        found.insert(relative);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    refused.push(Refusal::absent(path))
                }
                Err(e) => refused.push(Refusal::unreadable(path, &e)),
            }
        }
        Ok((found.into_iter().collect(), refused))
    }

    fn collect_under(
        &self,
        dir: &Path,
        root: &Path,
        found: &mut std::collections::BTreeSet<Vec<u8>>,
        refused: &mut Vec<Refusal>,
    ) -> Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            // An unreadable directory is refused with its own path rather than
            // failing the command: the rest of what was named is still
            // assignable, and §4.3 forbids the dead end.
            Err(e) => {
                refused.push(Refusal::unreadable(dir, &e));
                return Ok(());
            }
        };
        for entry in entries {
            let entry = entry?;
            let raw = entry.file_name().as_encoded_bytes().to_vec();
            // The repository's own directory is skipped, but ONLY at the root:
            // a user directory named `.lattice` nested below is real content.
            if dir == root && raw == REPO_DIR.as_bytes() {
                continue;
            }
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() && !meta.file_type().is_symlink() {
                self.collect_under(&path, root, found, refused)?;
            } else if let Some(relative) =
                path.strip_prefix(root).ok().and_then(relative_path_bytes)
            {
                found.insert(relative);
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- lines

    /// Create a line and make it current; if it already exists, this is a
    /// switch. The postcondition is uniform — the line exists and is current —
    /// which is why `start <existing>` succeeds rather than erroring (ADR-16 §3).
    pub fn start_line(&mut self, name: &str) -> Result<LineOutcome> {
        let rescued = self.complete_pending_switch()?;
        let name = validate_line_name(name)?;
        let mut lines = self.line_state()?;
        let from = lines.current.clone();
        if lines.lines.contains_key(&name) {
            let mut out = self.switch_to(lines, &from, &name, true)?;
            out.rescued_working_state = rescued;
            return Ok(out);
        }
        // A new line inherits the current tip AND the on-disk bytes, so there
        // is nothing to materialise. Inheriting the tip is forced: G1.1 asserts
        // no baseline checkpoint disappears after a crash on a pool including
        // `start`, and a line with no tip would empty that array.
        let captured = self.capture_working_tree()?;
        let tip = lines.lines.get(&from).and_then(|r| r.tip.clone());
        // The new line inherits the assignments too, for the same reason it
        // inherits the bytes (ADR-17 §4): the files are still on disk, and a
        // labelling that vanished across `start` would be a silent loss of
        // user intent — the work would still be there, but which unit of work
        // it belonged to would not.
        let (changes, current_change) = lines
            .lines
            .get(&from)
            .map(|r| (r.changes.clone(), r.current_change.clone()))
            .unwrap_or_default();
        if let Some(rec) = lines.lines.get_mut(&from) {
            rec.working = Some(captured);
        }
        lines.lines.insert(
            name.clone(),
            LineRecord {
                tip: tip.clone(),
                working: None,
                changes,
                current_change,
            },
        );
        lines.current = name.clone();
        let entry = self.oplog.commit(
            Operation::StartLine {
                name: name.clone(),
                from,
                created: true,
            },
            Some(lines.clone()),
        )?;
        self.sync_head_pointer(&lines)?;
        Ok(LineOutcome {
            line: name,
            created: true,
            now_at: tip,
            oplog_seq: entry.seq,
            rescued_working_state: rescued,
        })
    }

    /// Make an existing line current, preserving this line's working state and
    /// restoring the target's. Refuses only an unknown or invalid name — §4.3
    /// forbids a dirty-tree refusal, and state is preserved per line.
    pub fn switch_line(&mut self, name: &str) -> Result<LineOutcome> {
        let rescued = self.complete_pending_switch()?;
        let name = validate_line_name(name)?;
        let lines = self.line_state()?;
        let from = lines.current.clone();
        if !lines.lines.contains_key(&name) {
            return Err(Error::NoSuchLine(format!("there is no line named {name}")));
        }
        let mut out = self.switch_to(lines, &from, &name, false)?;
        out.rescued_working_state = rescued;
        Ok(out)
    }

    fn switch_to(
        &mut self,
        mut lines: LineState,
        from: &str,
        to: &str,
        as_start: bool,
    ) -> Result<LineOutcome> {
        // Self-switch short-circuits: the target state IS the current state, so
        // no capture and no file is touched. G1.4 draws `switch main` while
        // already on `main` thousands of times, and a full capture-and-rewrite
        // there is O(repo) work for no benefit.
        if from == to {
            let entry = self.oplog.commit(
                if as_start {
                    Operation::StartLine {
                        name: to.to_string(),
                        from: from.to_string(),
                        created: false,
                    }
                } else {
                    Operation::Switch {
                        from: from.to_string(),
                        to: to.to_string(),
                    }
                },
                None,
            )?;
            return Ok(LineOutcome {
                line: to.to_string(),
                created: false,
                now_at: lines.lines.get(to).and_then(|r| r.tip.clone()),
                oplog_seq: entry.seq,
                rescued_working_state: None,
            });
        }

        // Capture first: no materialisation ever happens without the current
        // working tree already durable in the store.
        let captured = self.capture_working_tree()?;
        if let Some(rec) = lines.lines.get_mut(from) {
            rec.working = Some(captured);
        }
        let target = lines.lines.entry(to.to_string()).or_default();
        // The preserved address is deliberately LEFT IN PLACE across the
        // publish. It is the marker that materialisation is still pending: if
        // we crash between publishing and writing the files, the next command
        // sees the current line holding preserved state and finishes the job.
        // Clearing it first would drop the only reference to that work.
        let restored = target.working.clone();
        let tip = target.tip.clone();
        lines.current = to.to_string();

        // Publish BEFORE materialising: a crash then leaves us unambiguously on
        // the target with a re-runnable idempotent materialisation, rather than
        // on the source holding the target's files. No work is lost either way,
        // because the capture is durable content first.
        let op = if as_start {
            Operation::StartLine {
                name: to.to_string(),
                from: from.to_string(),
                created: false,
            }
        } else {
            Operation::Switch {
                from: from.to_string(),
                to: to.to_string(),
            }
        };
        let entry = self.oplog.commit(op, Some(lines.clone()))?;
        self.materialise_working_tree(restored.as_deref())?;
        // Materialisation done: the bytes on disk are now the truth for this
        // line, so the preserved copy is consumed and the invariant that the
        // current line holds no preserved state is restored.
        if let Some(rec) = lines.lines.get_mut(to) {
            rec.working = None;
        }
        self.oplog.publish_lines(&lines)?;
        self.sync_head_pointer(&lines)?;
        Ok(LineOutcome {
            line: to.to_string(),
            created: false,
            now_at: tip,
            oplog_seq: entry.seq,
            rescued_working_state: None,
        })
    }

    /// Every line and which one is current.
    pub fn lines(&self) -> Result<LineState> {
        self.line_state()
    }

    /// Finish a switch that was interrupted after publishing but before its
    /// files were written.
    ///
    /// The current line holding preserved state is exactly that residue — the
    /// invariant is that a current line holds none. Completing it here makes
    /// the interrupted switch self-repairing, which is what lets the
    /// self-target short-circuit stay cheap without stranding a half-applied
    /// switch.
    fn complete_pending_switch(&mut self) -> Result<Option<String>> {
        let lines = self.line_state()?;
        let current = lines.current.clone();
        let pending = lines.lines.get(&current).and_then(|r| r.working.clone());
        let Some(pending) = pending else {
            return Ok(None);
        };
        // Capture what is on disk BEFORE pruning to the pending tree. The
        // interrupted switch captured only what existed BEFORE it failed, so
        // anything written since — by a user who fixed the permission error the
        // switch reported and carried on working — has no durable copy at all,
        // and this materialiser would delete it with no way back. ADR-16 §6's
        // rule holds on the repair path too: nothing is materialised over an
        // uncaptured working tree.
        let rescued = self.capture_working_tree()?;
        let mut lines = self.line_state()?;
        if rescued == pending {
            // Nothing was written since the interruption; just consume the
            // marker without touching a single file.
            if let Some(rec) = lines.lines.get_mut(&current) {
                rec.working = None;
            }
            self.oplog.publish_lines(&lines)?;
            return Ok(None);
        }
        self.materialise_working_tree(Some(&pending))?;
        if let Some(rec) = lines.lines.get_mut(&current) {
            rec.working = None;
        }
        self.oplog.publish_lines(&lines)?;
        self.sync_head_pointer(&lines)?;
        // Durable, content-addressed, and named to the caller — the same
        // contract undo-of-start already gives for work it must set aside.
        Ok(Some(rescued))
    }

    /// Snapshot the working tree into the store and return its tree address.
    fn capture_working_tree(&mut self) -> Result<String> {
        let mut packer = PackWriter::new();
        let tree = self.snapshot_dir(&self.root.clone(), &mut packer)?;
        packer.retain_unknown(&self.store);
        self.store.write_pack(packer)?;
        Ok(tree)
    }

    /// Bring the working tree to `tree`, removing what the target does not
    /// name.
    ///
    /// A line with no preserved working state is an ERROR here, not a fall
    /// back to its tip tree. That fallback was safe only while a tip tree
    /// equalled the working tree it came from; a partial save writes a tip
    /// that never existed on disk, so restoring it would delete exactly the
    /// unassigned work the user did not checkpoint (ADR-17, consequences).
    /// "This line preserved nothing" and "materialise its tip" are two
    /// different facts that were only ever equal under an invariant that no
    /// longer holds.
    ///
    /// Unreachable from any caller today — every one passes `Some` — so this
    /// costs a refusal in a case that does not arise, and prevents a silent
    /// deletion in one that would.
    fn materialise_working_tree(&self, tree: Option<&str>) -> Result<()> {
        let Some(target) = tree else {
            return Err(Error::Invalid(
                "this line preserved no working state, so there is nothing to \
                 restore; the working tree has been left exactly as it is"
                    .to_string(),
            ));
        };
        let target = target.to_string();
        let root = self.root.clone();
        let mut report = CheckoutReport {
            checkpoint: String::new(),
            entries_written: 0,
            collisions: Vec::new(),
        };
        // Reconciling, unlike `checkout --into`: switching lines must also
        // REMOVE what the target tree does not name, or the working tree
        // becomes the union of both lines and switch stops being involutive.
        self.prune_to_tree(&target, &root)?;
        self.restore_tree(&target, &root, &mut report)?;
        Ok(())
    }

    /// Delete working-tree entries the target tree does not name.
    fn prune_to_tree(&self, tree_id: &str, dir: &Path) -> Result<()> {
        let Some(id) = ChunkId::from_hex(tree_id) else {
            return Err(Error::Corrupt(format!("{tree_id} is not a tree address")));
        };
        let Some(bytes) = self.store.read(id)? else {
            return Ok(());
        };
        let tree = Tree::from_bytes(&bytes)?;
        let entries: Vec<_> = fs::read_dir(dir)?.collect::<std::result::Result<_, _>>()?;
        for entry in entries {
            let name = entry.file_name();
            let raw = name.as_encoded_bytes().to_vec();
            // The repository's own directory is never part of a tree.
            if dir == self.root.as_path() && raw == REPO_DIR.as_bytes() {
                continue;
            }
            let path = entry.path();
            // NEVER follow a symlink here. Every branch decides on
            // `symlink_metadata`, which does not resolve the final component:
            // descending through a link would delete the LINK TARGET's
            // contents, outside the working tree entirely (CWE-59). A benign
            // `node_modules`-style link plus a same-named directory on another
            // line is enough to trigger it, so this is not a hostile-input
            // case — it is an ordinary one.
            let meta = fs::symlink_metadata(&path)?;
            let real_dir = meta.is_dir() && !meta.file_type().is_symlink();
            match tree.entries.get(&raw) {
                Some(Node::Directory { tree: child }) if real_dir => {
                    self.prune_to_tree(child, &path)?;
                }
                Some(Node::Directory { .. }) => {
                    // A file or a symlink stands where the target names a
                    // directory: remove it and let restore_tree create the
                    // real directory.
                    platform::remove_file_or_symlink(&path)?;
                }
                Some(_) if real_dir => {
                    // A directory stands where the target names a file or a
                    // symlink. restore_tree cannot write over it, so it must go
                    // here or the switch fails half-applied.
                    fs::remove_dir_all(&path)?;
                }
                Some(_) => {}
                None if real_dir => fs::remove_dir_all(&path)?,
                None => platform::remove_file_or_symlink(&path)?,
            }
        }
        Ok(())
    }

    /// Keep the human-readable HEAD cache in step with the published state.
    fn sync_head_pointer(&self, lines: &LineState) -> Result<()> {
        match lines.lines.get(&lines.current).and_then(|r| r.tip.clone()) {
            Some(tip) => self.write_head_pointer(&tip),
            None => Ok(()),
        }
    }

    // ------------------------------------------------------------ snapshot

    fn snapshot_dir(&mut self, dir: &Path, packer: &mut PackWriter) -> Result<String> {
        let at_root = dir == self.root.as_path();
        let mut tree = Tree::new();
        let mut names: Vec<_> = fs::read_dir(dir)?.collect::<std::result::Result<_, _>>()?;
        // Sort by raw name bytes so the tree is canonical regardless of the
        // order the filesystem enumerated.
        names.sort_by_key(|e| e.file_name().into_encoded_bytes());

        for entry in names {
            let name = entry.file_name();
            let raw = name.as_encoded_bytes().to_vec();
            // The repository's own directory is skipped, but ONLY at the root.
            // A user directory named ".lattice" nested anywhere below is real
            // content and must be captured — silently dropping it is data loss.
            if at_root && raw == REPO_DIR.as_bytes() {
                continue;
            }
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            let node = self.node_for(&path, &meta, packer)?;
            tree.entries.insert(raw, node);
        }
        tree.write(packer)
    }

    /// The tree node for what is at `path`, whose metadata the caller already
    /// has.
    ///
    /// One implementation, shared by the full snapshot and the partial save
    /// that splices single paths into a parent tree. Two would be two answers
    /// to "what does this file become in a tree", free to drift on the mode
    /// bits or on whether a symlink is followed.
    ///
    /// The metadata is a parameter rather than fetched here: the full snapshot
    /// already holds it, and re-stating every entry would put an extra syscall
    /// per file into the path G1.5 measures.
    fn node_for(
        &mut self,
        path: &Path,
        meta: &fs::Metadata,
        packer: &mut PackWriter,
    ) -> Result<Node> {
        if meta.file_type().is_symlink() {
            // The target, never the bytes it points at: following it would
            // inline the target and lose the link.
            let target = fs::read_link(path)?;
            Ok(Node::Symlink {
                target: target.as_os_str().as_encoded_bytes().to_vec(),
            })
        } else if meta.is_dir() {
            Ok(Node::Directory {
                tree: self.snapshot_dir(path, packer)?,
            })
        } else {
            let content = fs::read(path)?;
            Ok(tree::file_node(&content, platform::file_mode(meta), packer))
        }
    }

    /// The tree a partial save writes: `base`, with each assigned path
    /// carrying the content it has in the working tree right now, and absent
    /// where it is no longer there (ADR-17 §5).
    ///
    /// A COMPLETE tree, not a fragment. Nothing downstream of
    /// `Checkpoint.tree` — checkout, restore_tree, verify_tree, the parent
    /// walks — learns that changes exist; what stops being true is only that
    /// the tip tree equals the working tree.
    ///
    /// `paths` are component lists, each non-empty, relative to `dir`.
    fn splice_paths(
        &mut self,
        base: Option<&str>,
        dir: &Path,
        paths: &[&[Vec<u8>]],
        packer: &mut PackWriter,
    ) -> Result<String> {
        let mut tree = match base {
            Some(id) => self.read_tree(id)?,
            None => Tree::new(),
        };
        let mut by_name: BTreeMap<&[u8], Vec<&[Vec<u8>]>> = BTreeMap::new();
        for path in paths {
            by_name.entry(path[0].as_slice()).or_default().push(path);
        }
        for (name, group) in by_name {
            let Some(component) = platform::os_string_from_bytes(name) else {
                // A name this platform cannot represent was never written
                // here, so there is nothing at that path to splice in.
                continue;
            };
            let at = dir.join(component);
            let deeper: Vec<&[Vec<u8>]> = group
                .iter()
                .filter(|p| p.len() > 1)
                .map(|p| &p[1..])
                .collect();
            if deeper.is_empty() {
                match fs::symlink_metadata(&at) {
                    Ok(meta) => {
                        let node = self.node_for(&at, &meta, packer)?;
                        tree.entries.insert(name.to_vec(), node);
                    }
                    // Assigned and since deleted. The checkpoint records the
                    // deletion, which is what the working tree says.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        tree.entries.remove(name);
                    }
                    Err(e) => return Err(e.into()),
                }
            } else {
                // Deeper paths under this name, so what stands here is a
                // directory — and a leaf assignment for the same name, if the
                // set holds one, is a path that has since BECOME one.
                let child_base = match tree.entries.get(name) {
                    Some(Node::Directory { tree }) => Some(tree.clone()),
                    _ => None,
                };
                let child = self.splice_paths(child_base.as_deref(), &at, &deeper, packer)?;
                tree.entries
                    .insert(name.to_vec(), Node::Directory { tree: child });
            }
        }
        tree.write(packer)
    }

    fn read_tree(&self, id: &str) -> Result<Tree> {
        let Some(address) = ChunkId::from_hex(id) else {
            return Err(Error::Corrupt(format!("{id} is not a tree address")));
        };
        let Some(bytes) = self.store.read(address)? else {
            return Err(Error::Corrupt(format!(
                "tree {} is not in the store",
                crate::short_id(id)
            )));
        };
        Tree::from_bytes(&bytes)
    }

    // ------------------------------------------------------------ checkout

    pub fn checkout(&self, checkpoint_id: &str, dest: &Path) -> Result<CheckoutReport> {
        let Some(cp) = self.checkpoint(checkpoint_id)? else {
            return Err(Error::NotFound(format!("no checkpoint {checkpoint_id}")));
        };
        // Refuse a destination that is itself a symlink: create_dir_all and
        // every write beneath it would resolve through the link and land
        // outside the directory the caller named (CWE-59). A real directory,
        // or a not-yet-existing path, is fine. A metadata error other than
        // "absent" (a permission or I/O failure) is propagated, not treated as
        // an absent destination. Ancestors of `dest` are NOT checked: `dest` is
        // a path the user chose (`--into`), and its ancestors resolve as their
        // own filesystem dictates — on macOS `/tmp` itself is a symlink — so
        // rejecting symlinked ancestors would break ordinary checkout. The
        // traversal protections guard the checkpoint-controlled paths written
        // BENEATH `dest`, not the user's choice of `dest`.
        match fs::symlink_metadata(dest) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::Invalid(format!(
                    "{} is a symlink; choose a real directory to check out into",
                    dest.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Refuse a destination inside the repository.
        //
        // `restore_tree` overwrites whatever it finds, by the deliberate policy
        // stated there — correct for a directory the caller named, lethal for
        // the working state. And unlike switch, start and undo, checkout holds
        // `&self`: it CANNOT capture the working tree first, so ADR-16 §6's
        // "nothing is materialised over an uncaptured working tree" cannot be
        // satisfied here at all. `ltx checkout --into .` therefore destroyed
        // uncheckpointed work and exited 0 reporting success.
        //
        // Writing a checkpoint OUT is what this command is for. Restoring the
        // working state IN PLACE is a different operation, with the capture
        // obligation that implies, and it does not exist yet — refusing keeps
        // that door open instead of silently doing it wrong.
        let root = resolve_as_far_as_it_exists(&self.root)?;
        if resolve_as_far_as_it_exists(dest)?.starts_with(&root) {
            return Err(Error::Invalid(format!(
                "{} is inside the repository; checkout writes over what it finds \
                 and cannot preserve uncheckpointed work, so it writes only \
                 outside the repository",
                dest.display()
            )));
        }
        fs::create_dir_all(dest)?;
        let mut report = CheckoutReport {
            checkpoint: cp.id.clone(),
            entries_written: 0,
            collisions: Vec::new(),
        };
        self.restore_tree(&cp.tree, dest, &mut report)?;
        Ok(report)
    }

    /// Materialise a checkpoint into `dest`, defaulting to the current one.
    ///
    /// `None` selects the current checkpoint; if nothing has been saved, that
    /// is an error. The default-to-current policy lives here rather than in the
    /// CLI, which §8 (G5.5) requires: the CLI must hold no decision the API
    /// lacks.
    pub fn checkout_into(&self, checkpoint: Option<&str>, dest: &Path) -> Result<CheckoutReport> {
        let id = match checkpoint {
            Some(id) => id.to_string(),
            None => {
                self.head_checkpoint()?
                    .ok_or_else(|| {
                        Error::NotFound(
                            "nothing has been saved yet, so there is nothing to write out".into(),
                        )
                    })?
                    .id
            }
        };
        self.checkout(&id, dest)
    }

    fn restore_tree(&self, tree_id: &str, dest: &Path, report: &mut CheckoutReport) -> Result<()> {
        let Some(id) = ChunkId::from_hex(tree_id) else {
            return Err(Error::Corrupt(format!("{tree_id} is not a tree address")));
        };
        let Some(bytes) = self.store.read(id)? else {
            return Err(Error::NotFound(format!(
                "tree {tree_id} is not present locally"
            )));
        };
        let tree = Tree::from_bytes(&bytes)?;
        // Names this checkout has already written in THIS directory, each with
        // the filesystem identity (inode) of the object it created. Used to
        // tell "the filesystem folded two of our names onto one object" apart
        // from "an unrelated file was already here" — the filesystem, not a
        // guess about which names fold, is the authority.
        let mut written_ids: Vec<(Vec<u8>, (u64, u64))> = Vec::new();

        for (name, node) in &tree.entries {
            // A tree entry name is a single path component by construction.
            // A "..", a separator, or a NUL can only reach here from a corrupt
            // or hostile repository, and joining it would let a checkout write
            // OUTSIDE the destination — arbitrary file write. Refuse it.
            if !is_safe_component(name) {
                report.collisions.push(Collision {
                    path: String::from_utf8_lossy(name).into_owned(),
                    collided_with: String::new(),
                    reason: "refused: not a single path component (a corrupt or \
                             hostile repository cannot escape the destination)"
                        .to_string(),
                });
                continue;
            }

            // Windows names are UTF-16, so a byte sequence that is not valid
            // UTF-8 has no faithful representation there. Reported rather than
            // approximated: a silently renamed file is a lost file.
            let Some(name_os) = platform::os_string_from_bytes(name) else {
                report.collisions.push(Collision {
                    path: String::from_utf8_lossy(name).into_owned(),
                    collided_with: String::new(),
                    reason: format!(
                        "this platform ({}) cannot represent these name bytes",
                        platform::platform_name()
                    ),
                });
                continue;
            };
            let path = dest.join(&name_os);

            // A fold collision is specifically: the path already exists AND it
            // is the SAME filesystem object as a name we already wrote in this
            // directory — the filesystem folded two distinct names into one.
            // An unrelated pre-existing file is not that; checkout overwrites
            // it, which is the point of writing a checkpoint into a directory.
            // The authority is the inode, not a guess about which names fold:
            // an earlier approximation missed real NFC/NFD and non-ASCII case
            // folds and silently overwrote the sibling.
            if let Ok(existing) = fs::symlink_metadata(&path) {
                if let Some(id) = platform::file_identity(&existing) {
                    if let Some((sibling, _)) = written_ids.iter().find(|(_, wid)| *wid == id) {
                        report.collisions.push(Collision {
                            path: String::from_utf8_lossy(name).into_owned(),
                            collided_with: String::from_utf8_lossy(sibling).into_owned(),
                            reason: "this filesystem does not distinguish these names \
                                     (case folding or Unicode normalisation)"
                                .to_string(),
                        });
                        continue;
                    }
                }
                // Otherwise a pre-existing unrelated file (or, where identity is
                // unavailable, a fold this platform cannot detect): overwrite.
            }

            // Never write THROUGH a pre-existing symlink at this path:
            // create_dir_all and fs::write both follow a final symlink, so a
            // link left in the destination (by an earlier checkout, or a
            // hostile actor) could redirect the write outside dest — a
            // path-traversal / link-following hole (CWE-59). Remove it first;
            // checkout replaces whatever is here. A folded sibling we wrote was
            // already caught above, so this only clears an unrelated link.
            if let Ok(meta) = fs::symlink_metadata(&path) {
                if meta.file_type().is_symlink() {
                    fs::remove_file(&path)?;
                }
            }

            match node {
                Node::Directory { tree } => {
                    fs::create_dir_all(&path)?;
                    // The directory itself is an entry checkout created, so it
                    // counts — otherwise the reported total understates a
                    // checkpoint that contains directories.
                    report.entries_written += 1;
                    self.restore_tree(tree, &path, report)?;
                }
                Node::Symlink { target } => {
                    // Replace anything already at this path. A real removal
                    // failure (a directory in the way, a permission error) is
                    // propagated, not swallowed; only a benign "already gone"
                    // is tolerated.
                    if let Err(e) = fs::remove_file(&path) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            return Err(e.into());
                        }
                    }
                    let Some(target_os) = platform::os_string_from_bytes(target) else {
                        report.collisions.push(Collision {
                            path: String::from_utf8_lossy(name).into_owned(),
                            collided_with: String::new(),
                            reason: format!(
                                "this platform ({}) cannot represent the link \
                                 target's bytes",
                                platform::platform_name()
                            ),
                        });
                        continue;
                    };
                    platform::symlink(Path::new(&target_os), &path)?;
                    report.entries_written += 1;
                }
                Node::File { chunks, size, mode } => {
                    let mut content = Vec::with_capacity(*size as usize);
                    for hex in chunks {
                        let Some(cid) = ChunkId::from_hex(hex) else {
                            return Err(Error::Corrupt(format!("{hex} is not a chunk address")));
                        };
                        let Some(part) = self.store.read(cid)? else {
                            return Err(Error::NotFound(format!(
                                "chunk {hex} is not present locally"
                            )));
                        };
                        content.extend_from_slice(&part);
                    }
                    if content.len() as u64 != *size {
                        return Err(Error::Corrupt(format!(
                            "{} reassembles to {} bytes, expected {size}",
                            path.display(),
                            content.len()
                        )));
                    }
                    fs::write(&path, &content)?;
                    platform::set_file_mode(&path, *mode)?;
                    if platform::mode_is_lossy_here(*mode) {
                        // Named, not silent. The lost bits vary — the executable
                        // bit, or the group/other distinctions of a mode like
                        // 0o640 — so the report states the mode that could not
                        // be recorded rather than naming one specific bit.
                        report.collisions.push(Collision {
                            path: String::from_utf8_lossy(name).into_owned(),
                            collided_with: String::new(),
                            reason: format!(
                                "written, but this platform ({}) cannot record \
                                 the permission mode {:04o}",
                                platform::platform_name(),
                                mode & 0o777
                            ),
                        });
                    }
                    report.entries_written += 1;
                }
            }
            // Record the identity of what we just wrote so a later name the
            // filesystem folds onto it is detected as a collision, not
            // silently overwritten. Reached only on a successful write — the
            // unrepresentable-target arm above `continue`s before here.
            if let Ok(meta) = fs::symlink_metadata(&path) {
                if let Some(id) = platform::file_identity(&meta) {
                    written_ids.push((name.clone(), id));
                }
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- verify

    /// Verify the repository.
    ///
    /// `complete = false` verifies the full Merkle spine plus the content that
    /// is present locally, and reports coverage. `complete = true` additionally
    /// requires every referenced chunk to be present, and is the only form that
    /// may be read as an unqualified "verified" (Challenge 4).
    pub fn verify(&self, complete: bool) -> Result<VerifyReport> {
        let mut report = VerifyReport {
            structure_verified: true,
            chunks_verified: 0,
            chunks_absent: 0,
            checkpoints_partial: 0,
            checkpoints: 0,
            oplog_entries: self.oplog.len()?,
            complete,
            errors: Vec::new(),
        };

        if let Some(break_at) = self.oplog.verify_chain()? {
            report.structure_verified = false;
            report.errors.push(format!("operation log: {break_at}"));
        }

        let checkpoints = self.checkpoints()?;
        let known: std::collections::HashSet<&str> =
            checkpoints.iter().map(|c| c.id.as_str()).collect();
        for cp in &checkpoints {
            report.checkpoints += 1;
            if self.change_a_checkpoint_took(&cp.id)?.is_some() {
                report.checkpoints_partial += 1;
            }
            if let Err(e) = self.verify_tree(&cp.tree, &mut report) {
                report.structure_verified = false;
                report
                    .errors
                    .push(format!("checkpoint {}: {e}", crate::short_id(&cp.id)));
            }
        }

        // Every Save the op-log records must resolve to a present, authentic
        // checkpoint. One that does not means its blob is missing — a hole
        // verify must surface rather than silently walk past, since without it
        // a repository missing its checkpoints reports a clean 0 checkpoints.
        for entry in self.oplog.entries()? {
            if let Operation::Save { checkpoint, .. } = &entry.operation {
                if !known.contains(checkpoint.as_str()) {
                    // A missing checkpoint blob is a hole in the history spine.
                    report.structure_verified = false;
                    report.errors.push(format!(
                        "operation {} saved checkpoint {} but its record is not present",
                        entry.seq,
                        crate::short_id(checkpoint)
                    ));
                }
            }
        }

        // Line records are part of the integrity surface now: a tip that does
        // not resolve, or a preserved working tree that is gone, is a hole a
        // user would meet as a broken switch. They are also GC roots — any
        // future thinning must treat them as such or it destroys preserved work.
        let state = self.line_state()?;
        for (name, rec) in &state.lines {
            if let Some(tip) = &rec.tip {
                // A malformed tip is a structural finding to REPORT — erroring
                // out would leave verify with no report at all. A genuine
                // storage failure is a different thing and must propagate, not
                // be flattened into "absent".
                if ChunkId::from_hex(tip).is_none() {
                    report.structure_verified = false;
                    report.errors.push(format!(
                        "line {name} points at {}, which is not a checkpoint address",
                        crate::short_id(tip)
                    ));
                } else if self.checkpoint(tip)?.is_none() {
                    report.structure_verified = false;
                    report.errors.push(format!(
                        "line {name} points at checkpoint {} which is not present",
                        crate::short_id(tip)
                    ));
                }
            }
            if let Some(working) = &rec.working {
                let present = ChunkId::from_hex(working)
                    .map(|id| self.store.contains(id))
                    .unwrap_or(false);
                if !present {
                    report.structure_verified = false;
                    report.errors.push(format!(
                        "line {name} holds working state {} which is not present",
                        crate::short_id(working)
                    ));
                }
            }
        }
        if !state.lines.contains_key(&state.current) {
            report.structure_verified = false;
            report
                .errors
                .push(format!("the current line {} does not exist", state.current));
        }

        if complete && report.chunks_absent > 0 {
            report.errors.push(format!(
                "{} referenced chunks are not present locally",
                report.chunks_absent
            ));
        }
        Ok(report)
    }

    fn verify_tree(&self, tree_id: &str, report: &mut VerifyReport) -> Result<()> {
        let Some(id) = ChunkId::from_hex(tree_id) else {
            return Err(Error::Corrupt(format!("{tree_id} is not a tree address")));
        };
        let Some(bytes) = self.store.read(id)? else {
            // A missing TREE is a hole in the history structure itself, not
            // merely absent file content. It is recorded as an error, and the
            // structure is marked not-verified, so verify cannot report a
            // clean, fully-verified repository when it never read the spine.
            report.structure_verified = false;
            report.chunks_absent += 1;
            report.errors.push(format!(
                "tree {} is not present locally",
                crate::short_id(tree_id)
            ));
            return Ok(());
        };
        report.chunks_verified += 1;
        let tree = Tree::from_bytes(&bytes)?;

        for node in tree.entries.values() {
            match node {
                Node::Directory { tree } => self.verify_tree(tree, report)?,
                Node::Symlink { .. } => {}
                Node::File { chunks, .. } => {
                    for hex in chunks {
                        let Some(cid) = ChunkId::from_hex(hex) else {
                            return Err(Error::Corrupt(format!("{hex} is not a chunk address")));
                        };
                        // `store.read` re-hashes and errors on mismatch, so a
                        // successful read IS the verification.
                        match self.store.read(cid) {
                            Ok(Some(_)) => report.chunks_verified += 1,
                            Ok(None) => report.chunks_absent += 1,
                            Err(e) => {
                                report.structure_verified = false;
                                report.errors.push(e.to_string());
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // ----------------------------------------------------------------- log

    pub fn log(&self) -> Result<Vec<Entry>> {
        self.oplog.entries()
    }

    /// Metadata about the working state, without changing it.
    pub fn status(&self) -> Result<Status> {
        let head = self.head_checkpoint()?;
        let head_change = match head.as_ref() {
            Some(cp) => self.change_a_checkpoint_took(&cp.id)?,
            None => None,
        };
        Ok(Status {
            root: self.root.display().to_string(),
            head: head.as_ref().map(|c| c.id.clone()),
            head_message: head.as_ref().map(|c| c.message.clone()),
            head_change,
            checkpoints: self.checkpoints()?.len() as u64,
            operations: self.oplog.len()?,
            chunks: self.store.chunk_count() as u64,
            packs: self.store.pack_count() as u64,
        })
    }
}

/// A name the filesystem could not represent alongside one already written.
///
/// This is not an engine failure and not a corrupt checkpoint: the checkpoint
/// holds both names correctly. It is the FILESYSTEM refusing to distinguish
/// them — APFS and NTFS fold case, and APFS also folds Unicode normalisation,
/// so `UPPER.txt` and `upper.txt` are one file there.
///
/// It is reported rather than ignored because the alternative is silently
/// overwriting one file with another, which is data loss by any definition, and
/// reported rather than fatal because refusing the whole checkout would trap a
/// user whose repository merely passed through Linux (§4.3: never block, never
/// trap).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Collision {
    pub path: String,
    pub collided_with: String,
    pub reason: String,
}

/// The result of `undo` — what it reversed, or that there was nothing to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UndoOutcome {
    pub nothing_to_undo: bool,
    /// The checkpoint whose creation was reversed, if any.
    pub undone_checkpoint: Option<String>,
    /// The checkpoint now current after the undo, if any.
    pub now_at: Option<String>,
    /// Op-log sequence of the appended Undo record, if one was appended.
    pub undo_seq: Option<u64>,
    /// Work set aside while completing a switch an earlier command left
    /// unfinished. Distinct from `preserved_tree`, which is the working state
    /// captured because undoing a line creation had nowhere to park it.
    pub rescued_working_state: Option<String>,
    /// Tree address of working state captured while reversing a line creation.
    ///
    /// Undoing `start` deletes the line, so there is nowhere in the line state
    /// to park the capture — it is written to the store as durable content and
    /// named here instead, so the work is never destroyed even though no
    /// command yet retrieves it (that is the ephemeral tier's job).
    pub preserved_working_state: Option<String>,
    /// Remote effects an undo could not reverse (Challenge 8 / SPEC §4.3).
    /// Always empty for a purely local undo; present so a future sync-undo can
    /// name its residue rather than silently leaving it.
    pub remote_effects_not_undone: Vec<String>,
}

impl UndoOutcome {
    fn nothing() -> Self {
        UndoOutcome {
            nothing_to_undo: true,
            undone_checkpoint: None,
            now_at: None,
            undo_seq: None,
            rescued_working_state: None,
            preserved_working_state: None,
            remote_effects_not_undone: Vec::new(),
        }
    }
}

/// What `start` or `switch` did.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LineOutcome {
    pub line: String,
    /// Whether this call created the line (a `start` of a new name) rather than
    /// switching to one that already existed.
    pub created: bool,
    /// The checkpoint the line now points at, if any.
    pub now_at: Option<String>,
    /// Position of the recorded operation. Every state-changing command reports
    /// one so a concurrent history can be checked for linearizability.
    pub oplog_seq: u64,
    /// Work set aside while completing a switch that an earlier command left
    /// unfinished. Durable, content-addressed, and named here so it is never
    /// captured-but-unmentioned.
    pub rescued_working_state: Option<String>,
}

/// What `save` did.
///
/// A struct rather than a bare `Checkpoint` because a save can also finish a
/// switch an earlier command left half-applied, and the work it sets aside
/// while doing so has to be NAMED (ADR-16 §6). `save` was the one caller of
/// `complete_pending_switch` that dropped that address on the floor: the work
/// was durable in the store and referenced by nothing a user could reach.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SaveOutcome {
    pub checkpoint: Checkpoint,
    /// The change this save checkpointed and consumed, if it was a partial
    /// one. `None` for a save of the whole working state.
    pub change: Option<String>,
    /// The whole working tree as it stood, content-addressed. For a partial
    /// save this is the only durable address of the part that was NOT
    /// checkpointed, which is why it is named rather than merely written.
    pub working_state: String,
    /// Work set aside while completing a switch an earlier command left
    /// unfinished. Durable and content-addressed, so naming it here is what
    /// makes it retrievable rather than merely stored.
    pub rescued_working_state: Option<String>,
}

/// What `assign` did.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssignOutcome {
    /// The change the paths went into, as recorded in history.
    pub change: String,
    /// The shortest prefix that names it among the changes open here — what a
    /// user types, and what prose displays.
    pub short: String,
    /// Whether this call created the change rather than adding to one.
    pub created: bool,
    pub line: String,
    /// The paths this call moved, which is not what was named: a path already
    /// in the change is not moved, and a refused one never was.
    pub assigned: Vec<String>,
    /// What it would not take, and why. Reported as data so a caller cannot
    /// miss it by not reading prose.
    pub refused: Vec<Refusal>,
    /// Position of the recorded operation. Every state-changing command
    /// reports one so a concurrent history can be checked for linearizability.
    pub oplog_seq: u64,
    /// Work set aside while completing a switch an earlier command left
    /// unfinished.
    pub rescued_working_state: Option<String>,
}

/// A path `assign` would not take, and why.
///
/// Refusals are reported, never silent, and never fatal: the command exits 0
/// carrying them (ADR-17 §6). Paths are rendered lossily because JSON cannot
/// hold arbitrary bytes — the same compromise `Collision` makes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Refusal {
    pub path: String,
    pub reason: String,
}

impl Refusal {
    fn absent(path: &Path) -> Self {
        Refusal {
            path: path.display().to_string(),
            reason: "no such path in the working state".into(),
        }
    }

    fn outside(path: &Path) -> Self {
        Refusal {
            path: path.display().to_string(),
            reason: "outside the working state; a change holds paths in this \
                     repository"
                .into(),
        }
    }

    fn unreadable(path: &Path, why: &std::io::Error) -> Self {
        Refusal {
            path: path.display().to_string(),
            reason: format!("unreadable: {why}"),
        }
    }

    fn capped(path: &[u8]) -> Self {
        Refusal {
            path: display_path(path),
            reason: format!(
                "this change already holds {MAX_ASSIGNED_PATHS} paths, the most one \
                 change may hold; save it or assign to another change"
            ),
        }
    }
}

/// One open change, as `change list` reports it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangeView {
    pub id: String,
    pub short: String,
    pub assigned: Vec<String>,
    /// Whether a bare `ltx assign` adds to this one.
    pub current: bool,
}

/// A path relative to the root, as its components joined by `/`.
///
/// NOT `as_os_str().as_encoded_bytes()` over the whole path: that carries the
/// platform's separator, so the same assignment would read `sub/a.txt` on one
/// machine and `sub\a.txt` on another, and splitting it again would need to
/// know which machine wrote it. Neither platform allows a separator inside a
/// single component, so `/` is a lossless join everywhere — and it is what a
/// tree walk splits on, since a tree entry name IS one component.
///
/// `None` for anything that is not a plain relative path. Nothing else can
/// come out of `strip_prefix` against the root, and guessing at one would put
/// a `..` into a path a checkpoint later writes.
fn relative_path_bytes(relative: &Path) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return None;
        };
        if !out.is_empty() {
            out.push(b'/');
        }
        out.extend_from_slice(name.as_encoded_bytes());
    }
    (!out.is_empty()).then_some(out)
}

/// A working-tree path as a human reads it.
///
/// Lossy, because a path is raw bytes and JSON is text. The bytes stay exact
/// in the change record and in the op-log; only what is shown is approximate.
fn display_path(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

/// Resolve a change reference a user typed against the changes open on a line.
///
/// A reference that matches nothing open is checked against the changes a
/// standing checkpoint has already consumed, so a user who has just saved one
/// is told that rather than that it never existed. The scope limit is worded
/// as one (ADR-17 §1): reopening a checkpointed change is deferred, not
/// rejected, and widening it later should read as a continuation.
fn resolve_change(typed: &str, record: &LineRecord, line: &str, oplog: &OpLog) -> Result<String> {
    let ids: Vec<&str> = record.changes.keys().map(String::as_str).collect();
    match change::resolve(typed, &ids) {
        change::Resolution::One(id) => Ok(id),
        change::Resolution::Ambiguous(matched) => {
            let shown: Vec<String> = matched
                .iter()
                .map(|id| change::abbreviate(id, &ids))
                .collect();
            Err(Error::InvalidChange(format!(
                "{typed} names {} open changes ({})",
                matched.len(),
                shown.join(", ")
            )))
        }
        change::Resolution::Unknown => match checkpointed_change(typed, line, oplog)? {
            Some(id) => Err(Error::InvalidChange(format!(
                "change {} is already checkpointed, and reopening one is not in v1",
                crate::short_id(&id)
            ))),
            None => Err(Error::NoSuchChange(format!(
                "no change {typed} is open on this line"
            ))),
        },
    }
}

/// A change on this line that a standing save has checkpointed, if `typed`
/// names one.
///
/// Read from the op-log rather than kept in the line state: consuming a change
/// REMOVES it from that state, which is what makes it stop being pending.
/// Only walked when a reference has already failed to resolve, so this is on
/// an error path and never on the path a command takes when it works.
fn checkpointed_change(typed: &str, line: &str, oplog: &OpLog) -> Result<Option<String>> {
    let entries = oplog.entries()?;
    let undone: std::collections::HashSet<u64> = entries
        .iter()
        .filter_map(|e| match &e.operation {
            Operation::Undo { undone_seq } => Some(*undone_seq),
            _ => None,
        })
        .collect();
    Ok(entries
        .iter()
        .filter(|e| !undone.contains(&e.seq))
        .find_map(|e| match &e.operation {
            Operation::Save {
                line: on,
                change: Some(c),
                ..
            } if on == line && c.id.starts_with(typed) => Some(c.id.clone()),
            _ => None,
        }))
}

/// Resolve `path` through symlinks as far as it exists, appending the part
/// that does not exist yet verbatim.
///
/// `fs::canonicalize` fails outright on a path whose tail is absent, but a
/// checkout destination is usually a directory about to be created. Comparing
/// unresolved paths would be defeated by `.`, `..`, or a symlinked ancestor —
/// on macOS `/tmp` is itself a symlink — so the containment check needs the
/// resolved form of whatever prefix is real.
fn resolve_as_far_as_it_exists(path: &Path) -> Result<PathBuf> {
    use std::path::Component;

    // Left to right, resolving each prefix as soon as it is real. Walking
    // right to left instead would step over a `..`: for a repository at
    // `/r`, the destination `/r/../missing/../r/out` would compare as
    // "outside" while the filesystem puts it back inside. `..` cannot be
    // folded away lexically up front either, because `link/..` goes to the
    // link TARGET's parent, not to `link`'s — so a prefix must be resolved
    // before its `..` is applied, which is exactly the order below.
    let mut resolved = PathBuf::new();
    for component in std::path::absolute(path)?.components() {
        match component {
            Component::CurDir => continue,
            // Beyond what exists, nothing can be a symlink, so popping is
            // the same answer the filesystem gives.
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
        match fs::canonicalize(&resolved) {
            Ok(real) => resolved = real,
            // Not there yet: keep building the path literally.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(resolved)
}

/// A line name, parsed once at the boundary.
///
/// Kept to a conservative set so a name is never mistaken for a path segment
/// and never needs escaping. Names are values in the line state, never redb
/// keys, so no reserved word is required.
fn validate_line_name(name: &str) -> Result<String> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with(['/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if ok {
        Ok(name.to_string())
    } else {
        Err(Error::InvalidLine(format!(
            "{name:?} is not a valid line name (letters, digits, . _ - /; up to 128 bytes)"
        )))
    }
}

/// What a checkout managed to write.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckoutReport {
    /// The checkpoint that was materialised (its authenticated address).
    pub checkpoint: String,
    pub entries_written: u64,
    /// Names this filesystem cannot hold alongside one already written. Empty
    /// on a case-sensitive filesystem.
    pub collisions: Vec<Collision>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub root: String,
    pub head: Option<String>,
    pub head_message: Option<String>,
    /// The change the current checkpoint took, when it took only one.
    ///
    /// Present exactly when the head was written by a partial save, and so
    /// records only part of the working state that stood at the time. This is
    /// where a user looks before `switch` or `undo` replaces their bytes, and
    /// printing a head with no qualifier after a partial save is a claim the
    /// engine can no longer support (ADR-17, consequences).
    pub head_change: Option<String>,
    pub checkpoints: u64,
    pub operations: u64,
    pub chunks: u64,
    pub packs: u64,
}

/// Is this tree entry name a single, safe path component?
///
/// Names come from `snapshot_dir`, which reads one `file_name()` per entry, so
/// a legitimate name is never `..`, `.`, empty, or separator-bearing. Any of
/// those reaching `restore_tree` means the tree is corrupt or hostile, and
/// joining it onto the destination would escape that directory — a checkout
/// writing to an arbitrary path. This is the guard that makes checking out an
/// untrusted repository safe.
fn is_safe_component(name: &[u8]) -> bool {
    if name.is_empty() || name == b".." || name == b"." {
        return false;
    }
    // '/' and NUL separate or terminate a path on every platform Lattice runs
    // on; '\\' and ':' do so on Windows (directory separator and drive/stream
    // marker — "C:evil" is drive-relative and escapes the destination) but are
    // legal bytes in a Unix filename, so they are rejected only where they are
    // actually significant.
    if name.iter().any(|&b| b == b'/' || b == 0) {
        return false;
    }
    if cfg!(windows) && name.iter().any(|&b| b == b'\\' || b == b':') {
        return false;
    }
    true
}

/// Unused import guard for BTreeMap in non-test builds.
#[allow(dead_code)]
fn _btree_marker(_: BTreeMap<Vec<u8>, Node>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> (tempfile::TempDir, Repo) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init(dir.path()).unwrap();
        (dir, repo)
    }

    /// A checkout destination outside the repository.
    ///
    /// `checkout` refuses to write anywhere under the root — it holds `&self`
    /// and so can never capture the working state first. Callers bind the
    /// returned `TempDir`, which must outlive the path.
    fn outside(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(name);
        (dir, dest)
    }

    #[test]
    fn init_records_an_operation_and_creates_the_store() {
        let (dir, repo) = repo();
        assert!(dir.path().join(".lattice/packs").is_dir());
        assert_eq!(repo.oplog().len().unwrap(), 1);
        assert_eq!(
            repo.oplog().head().unwrap().unwrap().operation,
            Operation::Init
        );
    }

    #[test]
    fn init_twice_is_refused_with_a_recovery_action() {
        let dir = tempfile::tempdir().unwrap();
        Repo::init(dir.path()).unwrap();
        let err = Repo::init(dir.path()).unwrap_err();
        assert!(!err.recovery().is_empty());
    }

    #[test]
    fn save_then_checkout_round_trips_bytes_exactly() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"hello\r\nworld\r\n").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b.bin"), [0u8, 1, 2, 0xff, 0xfe]).unwrap();
        fs::write(dir.path().join("empty.txt"), b"").unwrap();

        let cp = repo.save("first", None).unwrap().checkpoint;
        let (_hold, out) = outside("restored");
        repo.checkout(&cp.id, &out).unwrap();

        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"hello\r\nworld\r\n");
        assert_eq!(
            fs::read(out.join("sub/b.bin")).unwrap(),
            vec![0u8, 1, 2, 0xff, 0xfe]
        );
        assert_eq!(fs::read(out.join("empty.txt")).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn crlf_and_invalid_utf8_survive_a_round_trip() {
        // Two of G1.2's named cases, asserted at unit scale.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("crlf.txt"), b"a\r\nb\r").unwrap();
        fs::write(dir.path().join("bad.bin"), [0x41, 0xC3, 0x28, 0xA0]).unwrap();
        let cp = repo.save("adversarial", None).unwrap().checkpoint;
        let (_hold, out) = outside("out");
        repo.checkout(&cp.id, &out).unwrap();
        assert_eq!(fs::read(out.join("crlf.txt")).unwrap(), b"a\r\nb\r");
        assert_eq!(
            fs::read(out.join("bad.bin")).unwrap(),
            vec![0x41, 0xC3, 0x28, 0xA0]
        );
    }

    #[test]
    // Creating a symlink on Windows needs administrator rights or
    // Developer Mode, so this runs where the platform permits it. The
    // engine's symlink SUPPORT is cross-platform; only the test fixture
    // is not.
    #[cfg(unix)]
    fn a_symlink_round_trips_as_a_symlink() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("target.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("target.txt", dir.path().join("link")).unwrap();
        let cp = repo.save("with a link", None).unwrap().checkpoint;
        let (_hold, out) = outside("out");
        repo.checkout(&cp.id, &out).unwrap();
        let meta = fs::symlink_metadata(out.join("link")).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "a symlink must not be followed on save"
        );
        assert_eq!(
            fs::read_link(out.join("link")).unwrap(),
            Path::new("target.txt")
        );
    }

    #[test]
    // Creating a symlink on Windows needs administrator rights or
    // Developer Mode, so this runs where the platform permits it. The
    // engine's symlink SUPPORT is cross-platform; only the test fixture
    // is not.
    #[cfg(unix)]
    fn a_dangling_symlink_round_trips() {
        let (dir, mut repo) = repo();
        std::os::unix::fs::symlink("/nowhere/at/all", dir.path().join("dangling")).unwrap();
        let cp = repo.save("dangling", None).unwrap().checkpoint;
        let (_hold, out) = outside("out");
        repo.checkout(&cp.id, &out).unwrap();
        assert!(fs::symlink_metadata(out.join("dangling"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    // The executable bit is not representable on Windows; platform.rs names
    // that as a lossy edge and checkout reports it per file.
    #[cfg(unix)]
    fn the_executable_bit_round_trips() {
        let (dir, mut repo) = repo();
        let script = dir.path().join("run.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        crate::platform::set_file_mode(&script, 0o755).unwrap();
        let cp = repo.save("executable", None).unwrap().checkpoint;
        let (_hold, out) = outside("out");
        repo.checkout(&cp.id, &out).unwrap();
        let mode = crate::platform::file_mode(&fs::metadata(out.join("run.sh")).unwrap());
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn verify_reports_coverage_and_only_complete_claims_completeness() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f.txt"), b"content").unwrap();
        repo.save("one", None).unwrap();

        let partial = repo.verify(false).unwrap();
        assert!(partial.structure_verified);
        assert!(
            !partial.complete,
            "the default form must not claim completeness"
        );
        assert!(partial.chunks_verified > 0);

        let full = repo.verify(true).unwrap();
        assert!(full.complete);
        assert!(
            full.errors.is_empty(),
            "a healthy repo verifies clean: {:?}",
            full.errors
        );
        let _ = dir;
    }

    #[test]
    fn unsafe_component_names_are_rejected() {
        assert!(!is_safe_component(b""));
        assert!(!is_safe_component(b".."));
        assert!(!is_safe_component(b"."));
        assert!(!is_safe_component(b"a/b"));
        assert!(!is_safe_component(&[b'a', 0, b'b']));
        assert!(is_safe_component(b"normal.txt"));
        assert!(is_safe_component("café".as_bytes()));
        // Drive-relative and backslash-separated names escape only on Windows,
        // and a backslash is a legal byte in a Unix filename, so these are
        // checked where they are actually significant.
        #[cfg(windows)]
        {
            assert!(!is_safe_component(b"C:evil"), "drive-relative escapes dest");
            assert!(
                !is_safe_component(b"a\\b"),
                "backslash separates on Windows"
            );
        }
    }

    #[test]
    fn checkout_refuses_to_escape_the_destination() {
        let (dir, mut repo) = repo();
        // A tree whose entry name is "..", pointing at a file — the shape a
        // hostile or corrupt repository would use to write outside dest.
        let mut packer = PackWriter::new();
        let node = tree::file_node(b"pwned", 0o644, &mut packer);
        let mut t = Tree::new();
        t.entries.insert(b"..".to_vec(), node);
        let tree_id = t.write(&mut packer).unwrap();
        repo.store.write_pack(packer).unwrap();

        let out = dir.path().join("dest");
        fs::create_dir_all(&out).unwrap();
        let mut report = CheckoutReport {
            checkpoint: String::new(),
            entries_written: 0,
            collisions: Vec::new(),
        };
        repo.restore_tree(&tree_id, &out, &mut report).unwrap();

        assert!(
            !out.parent().unwrap().join("pwned").exists(),
            "checkout must not write outside the destination directory"
        );
        assert_eq!(report.entries_written, 0);
        assert!(report.collisions.iter().any(|c| c.path == ".."));
    }

    #[test]
    fn checkout_into_a_populated_directory_overwrites_rather_than_skipping() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"one").unwrap();
        fs::write(dir.path().join("b.txt"), b"two").unwrap();
        let cp = repo.save("two files", None).unwrap().checkpoint;

        let (_hold, out) = outside("dest");
        fs::create_dir_all(&out).unwrap();
        // A pre-existing unrelated file, and a stale copy of one of ours.
        fs::write(out.join("pre-existing.txt"), b"keep").unwrap();
        fs::write(out.join("a.txt"), b"STALE").unwrap();

        let report = repo.checkout(&cp.id, &out).unwrap();
        assert_eq!(
            report.entries_written, 2,
            "both checkpoint files must be written, not skipped as collisions"
        );
        assert!(
            report.collisions.is_empty(),
            "a pre-existing unrelated file is not a fold collision: {:?}",
            report.collisions
        );
        assert_eq!(
            fs::read(out.join("a.txt")).unwrap(),
            b"one",
            "a stale file is overwritten with checkpoint content"
        );
        assert_eq!(
            fs::read(out.join("pre-existing.txt")).unwrap(),
            b"keep",
            "an unrelated file is left alone"
        );
    }

    // Fold detection uses platform::file_identity, which is inode-based on
    // Unix and unavailable on Windows (a documented deferral, see the review
    // doc), so the no-silent-overwrite invariant is enforced where it holds.
    #[cfg(unix)]
    #[test]
    fn checkout_never_silently_drops_a_folded_sibling() {
        // Two distinct byte-names that a folding filesystem merges into one:
        // 'É' (C3 89) and 'é' (C3 A9) — a non-ASCII case pair. On a
        // case-sensitive filesystem they are two files. Whatever the filesystem
        // does, every tree entry must be either written or reported — never
        // silently overwritten with entries_written claiming both landed. This
        // is the case the earlier ASCII-only fold guess missed.
        let (dir, mut repo) = repo();
        let mut packer = PackWriter::new();
        let n1 = tree::file_node(b"content-of-upper", 0o644, &mut packer);
        let n2 = tree::file_node(b"content-of-lower", 0o644, &mut packer);
        let mut t = Tree::new();
        t.entries.insert("É.txt".as_bytes().to_vec(), n1);
        t.entries.insert("é.txt".as_bytes().to_vec(), n2);
        let tree_id = t.write(&mut packer).unwrap();
        repo.store.write_pack(packer).unwrap();

        let out = dir.path().join("dest");
        fs::create_dir_all(&out).unwrap();
        let mut report = CheckoutReport {
            checkpoint: String::new(),
            entries_written: 0,
            collisions: Vec::new(),
        };
        repo.restore_tree(&tree_id, &out, &mut report).unwrap();

        assert_eq!(
            report.entries_written as usize + report.collisions.len(),
            2,
            "every entry must be written or reported, never silently dropped"
        );
        let on_disk = fs::read_dir(&out).unwrap().count() as u64;
        assert_eq!(
            report.entries_written, on_disk,
            "entries_written must equal the files actually present, so a folded \
             sibling is never counted as written when it overwrote another"
        );
    }

    /// A repository whose change ids come from a counter rather than the
    /// machine's entropy, so a test can name the changes it made. The counter
    /// sits in the third byte, so two ids share their first four characters —
    /// which is what makes an abbreviation grow past the floor.
    fn counted_ids(dir_and_repo: (tempfile::TempDir, Repo)) -> (tempfile::TempDir, Repo) {
        let (dir, repo) = dir_and_repo;
        let mut n = 0u8;
        let repo = repo.with_change_id_bits(move || {
            n += 1;
            let mut bits = [0u8; 16];
            bits[2] = n;
            Ok(bits)
        });
        (dir, repo)
    }

    #[test]
    fn assigning_a_directory_takes_everything_under_it_but_never_the_repository() {
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b.txt"), b"b").unwrap();
        repo.save("seed", None).unwrap();

        let out = repo.assign(&[dir.path().to_path_buf()], None).unwrap();

        assert!(out.created, "there was no change open, so this started one");
        assert_eq!(
            out.assigned,
            vec!["a.txt".to_string(), "sub/b.txt".to_string()],
            "a directory assigns what is under it, and .lattice is not content; \
             the separator is `/` on every platform, since a change record has \
             to read the same wherever it was written"
        );
        assert!(out.refused.is_empty());
    }

    #[test]
    fn a_second_bare_assign_adds_to_the_change_the_first_one_started() {
        // G1.4 draws `assign .` about ten thousand times against one
        // repository. Ten thousand bare assigns must not leave ten thousand
        // changes behind, which is the whole reason a line holds a CURRENT
        // change rather than assign minting one per call.
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        repo.save("seed", None).unwrap();

        let first = repo.assign(&[dir.path().join("a.txt")], None).unwrap();
        fs::write(dir.path().join("b.txt"), b"b").unwrap();
        let second = repo.assign(&[dir.path().join("b.txt")], None).unwrap();

        assert_eq!(first.change, second.change);
        assert!(!second.created, "the second call added, it did not start");
        assert_eq!(repo.changes().unwrap().len(), 1);
        assert_eq!(
            repo.changes().unwrap()[0].assigned,
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );
    }

    #[test]
    fn assign_writes_no_byte_of_working_state() {
        // ADR-17 §2's invariant, and the reason the inverse needs neither a
        // capture nor a materialisation.
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"exactly this").unwrap();
        repo.save("seed", None).unwrap();

        repo.assign(&[dir.path().to_path_buf()], None).unwrap();

        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"exactly this");
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            2,
            "a.txt and .lattice — assign created nothing"
        );
    }

    #[test]
    fn a_path_that_is_not_there_is_refused_and_the_command_still_succeeds() {
        // G1.4 counts any non-zero exit as a failure across ~10,000 draws, so
        // a path that cannot be taken must be REPORTED rather than raised.
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        repo.save("seed", None).unwrap();

        let out = repo.assign(&[dir.path().join("gone.txt")], None).unwrap();

        assert!(out.assigned.is_empty());
        assert_eq!(out.refused.len(), 1);
        assert!(
            out.refused[0].reason.contains("no such path"),
            "the refusal says why: {}",
            out.refused[0].reason
        );
    }

    #[test]
    fn a_path_outside_the_working_state_is_refused() {
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        repo.save("seed", None).unwrap();
        let (_elsewhere, outside) = outside("theirs.txt");
        fs::write(&outside, b"not ours").unwrap();

        let out = repo.assign(&[outside], None).unwrap();

        assert!(
            out.assigned.is_empty(),
            "a change holds paths in ITS repository"
        );
        assert_eq!(out.refused.len(), 1);
        assert!(out.refused[0].reason.contains("outside the working state"));
    }

    #[test]
    fn assigning_to_a_change_that_is_not_open_changes_nothing() {
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        repo.save("seed", None).unwrap();
        let before = repo.oplog().len().unwrap();

        let err = repo
            .assign(&[dir.path().join("a.txt")], Some("deadbeef"))
            .unwrap_err();

        assert_eq!(
            err.concept(),
            crate::error::Concept::Change,
            "the §4.2 noun involved"
        );
        assert!(!err.recovery().is_empty());
        assert!(repo.changes().unwrap().is_empty(), "--to never creates");
        assert_eq!(
            repo.oplog().len().unwrap(),
            before,
            "and a refused target appends nothing"
        );
    }

    #[test]
    fn an_ambiguous_change_reference_names_the_changes_it_could_mean() {
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::write(dir.path().join("b.txt"), b"b").unwrap();
        repo.save("seed", None).unwrap();
        let first = repo.assign(&[dir.path().join("a.txt")], None).unwrap();
        // A second change, started by hand because `--to` never creates one.
        let mut lines = repo.line_state().unwrap();
        lines.lines.get_mut(DEFAULT_LINE).unwrap().current_change = None;
        repo.oplog.publish_lines(&lines).unwrap();
        let second = repo.assign(&[dir.path().join("b.txt")], None).unwrap();
        let shared = &first.change[..4];
        assert_eq!(
            shared,
            &second.change[..4],
            "the premise: they share a prefix"
        );

        let err = repo
            .assign(&[dir.path().join("a.txt")], Some(shared))
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains(&first.short), "names one: {message}");
        assert!(message.contains(&second.short), "and the other: {message}");
        assert_eq!(err.category(), crate::error::Category::Invalid);
    }

    #[test]
    fn assigning_a_path_to_another_change_moves_it_and_undo_moves_it_back() {
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::write(dir.path().join("b.txt"), b"b").unwrap();
        repo.save("seed", None).unwrap();
        let first = repo.assign(&[dir.path().join("a.txt")], None).unwrap();
        let mut lines = repo.line_state().unwrap();
        lines.lines.get_mut(DEFAULT_LINE).unwrap().current_change = None;
        repo.oplog.publish_lines(&lines).unwrap();
        let second = repo.assign(&[dir.path().join("b.txt")], None).unwrap();

        repo.assign(&[dir.path().join("a.txt")], Some(&second.short))
            .unwrap();

        let moved = repo.changes().unwrap();
        assert_eq!(
            moved
                .iter()
                .find(|c| c.id == first.change)
                .unwrap()
                .assigned,
            Vec::<String>::new(),
            "a path belongs to one change at a time"
        );
        assert_eq!(
            moved
                .iter()
                .find(|c| c.id == second.change)
                .unwrap()
                .assigned,
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );

        repo.undo().unwrap();

        let back = repo.changes().unwrap();
        assert_eq!(
            back.iter().find(|c| c.id == first.change).unwrap().assigned,
            vec!["a.txt".to_string()],
            "undo returns the path to the change it was taken from"
        );
        assert_eq!(
            back.iter()
                .find(|c| c.id == second.change)
                .unwrap()
                .assigned,
            vec!["b.txt".to_string()]
        );
    }

    #[test]
    fn a_change_stops_taking_paths_at_its_cap_and_says_so() {
        // The assignment set is published into the line state on every
        // operation, so an uncapped one would grow every write in the
        // repository. Reaching the cap is a refusal, not an error: what is
        // already assigned stays assigned.
        let (dir, mut repo) = counted_ids(repo());
        for i in 0..MAX_ASSIGNED_PATHS + 2 {
            fs::write(dir.path().join(format!("f{i:05}.txt")), b"x").unwrap();
        }
        repo.save("seed", None).unwrap();

        let out = repo.assign(&[dir.path().to_path_buf()], None).unwrap();

        assert_eq!(out.assigned.len(), MAX_ASSIGNED_PATHS);
        assert_eq!(out.refused.len(), 2);
        assert!(
            out.refused[0]
                .reason
                .contains(&MAX_ASSIGNED_PATHS.to_string()),
            "the refusal names the cap: {}",
            out.refused[0].reason
        );
    }

    #[test]
    fn a_partial_save_writes_a_complete_tree_with_only_the_assigned_paths_advanced() {
        // ADR-17 §5: the parent's tree with each assigned path replaced by its
        // current working content. Nothing downstream of `Checkpoint.tree`
        // learns that changes exist — a checkout of this checkpoint is an
        // ordinary checkout of an ordinary tree.
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("assigned.txt"), b"before").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/deep.txt"), b"before").unwrap();
        fs::write(dir.path().join("untouched.txt"), b"before").unwrap();
        repo.save("seed", None).unwrap();

        fs::write(dir.path().join("assigned.txt"), b"after").unwrap();
        fs::write(dir.path().join("sub/deep.txt"), b"after").unwrap();
        fs::write(dir.path().join("untouched.txt"), b"after").unwrap();
        let assigned = repo
            .assign(
                &[
                    dir.path().join("assigned.txt"),
                    dir.path().join("sub/deep.txt"),
                ],
                None,
            )
            .unwrap();

        let out = repo
            .save("only what was assigned", Some(&assigned.short))
            .unwrap();

        let (_holder, dest) = outside("partial");
        repo.checkout(&out.checkpoint.id, &dest).unwrap();
        assert_eq!(fs::read(dest.join("assigned.txt")).unwrap(), b"after");
        assert_eq!(fs::read(dest.join("sub/deep.txt")).unwrap(), b"after");
        assert_eq!(
            fs::read(dest.join("untouched.txt")).unwrap(),
            b"before",
            "an unassigned path stays at the parent's content — the tree is \
             complete, not a fragment"
        );
    }

    #[test]
    fn a_partial_save_consumes_the_change_it_checkpointed() {
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        repo.save("seed", None).unwrap();
        let assigned = repo.assign(&[dir.path().join("a.txt")], None).unwrap();

        repo.save("saved it", Some(&assigned.short)).unwrap();

        assert!(
            repo.changes().unwrap().is_empty(),
            "a checkpointed change is no longer pending"
        );
        let after = repo.line_state().unwrap();
        assert_eq!(
            after.lines[DEFAULT_LINE].current_change, None,
            "and a bare assign must not go on adding to it"
        );
    }

    #[test]
    fn a_partial_save_names_the_whole_working_tree_it_did_not_checkpoint() {
        // ADR-17 §5: the save already walks the whole tree, so the unassigned
        // remainder becomes content-addressed at no extra cost — and is NAMED,
        // because content nothing refers to is content nobody can reach.
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        repo.save("seed", None).unwrap();
        fs::write(dir.path().join("unassigned.txt"), b"not in the change").unwrap();
        let assigned = repo.assign(&[dir.path().join("a.txt")], None).unwrap();

        let out = repo.save("partial", Some(&assigned.short)).unwrap();

        assert_ne!(
            out.working_state, out.checkpoint.tree,
            "the working tree and the checkpointed tree genuinely differ here"
        );
        assert!(repo.store().contains(ChunkId::of(b"not in the change")));
    }

    #[test]
    fn a_partial_save_records_an_assigned_path_that_is_gone_as_gone() {
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("doomed.txt"), b"here").unwrap();
        fs::write(dir.path().join("kept.txt"), b"here").unwrap();
        repo.save("seed", None).unwrap();
        let assigned = repo.assign(&[dir.path().join("doomed.txt")], None).unwrap();
        fs::remove_file(dir.path().join("doomed.txt")).unwrap();

        let out = repo.save("removed it", Some(&assigned.short)).unwrap();

        let (_holder, dest) = outside("deleted");
        repo.checkout(&out.checkpoint.id, &dest).unwrap();
        assert!(
            !dest.join("doomed.txt").exists(),
            "the checkpoint records what the working tree says"
        );
        assert!(dest.join("kept.txt").exists(), "and nothing else moved");
    }

    #[test]
    fn saving_a_change_that_holds_nothing_is_refused_and_appends_nothing() {
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        repo.save("seed", None).unwrap();
        // An assign whose every path is refused starts the change but puts
        // nothing in it — the batch reaches the same state through
        // `assign c f; undo`.
        let empty = repo.assign(&[dir.path().join("gone.txt")], None).unwrap();
        let before = repo.oplog().len().unwrap();

        let err = repo
            .save("nothing to save", Some(&empty.short))
            .unwrap_err();

        assert_eq!(err.concept(), crate::error::Concept::Change);
        assert!(!err.recovery().is_empty());
        assert_eq!(
            repo.oplog().len().unwrap(),
            before,
            "a save that refuses appends nothing"
        );
    }

    #[test]
    fn a_change_a_checkpoint_already_took_is_refused_as_a_scope_limit() {
        // ADR-17 §1 words this as a scope limit rather than a flat rejection,
        // so that widening `assign` to reach back into checkpointed work later
        // reads as a continuation instead of a reversal. And it must not read
        // as "no such change": the user typed an id they were just shown.
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        repo.save("seed", None).unwrap();
        let assigned = repo.assign(&[dir.path().join("a.txt")], None).unwrap();
        repo.save("took it", Some(&assigned.short)).unwrap();

        let err = repo
            .assign(&[dir.path().join("a.txt")], Some(&assigned.short))
            .unwrap_err();

        assert!(
            err.to_string().contains("already checkpointed"),
            "got: {err}"
        );
        assert_eq!(err.category(), crate::error::Category::Invalid);
        assert_eq!(err.concept(), crate::error::Concept::Change);
    }

    #[test]
    fn undoing_a_partial_save_moves_the_tip_back_and_reopens_the_change() {
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        let seed = repo.save("seed", None).unwrap().checkpoint;
        fs::write(dir.path().join("a.txt"), b"changed").unwrap();
        let assigned = repo.assign(&[dir.path().join("a.txt")], None).unwrap();
        repo.save("partial", Some(&assigned.short)).unwrap();

        repo.undo().unwrap();

        let after = repo.line_state().unwrap();
        assert_eq!(
            after.lines[DEFAULT_LINE].tip.as_deref(),
            Some(seed.id.as_str())
        );
        assert_eq!(
            repo.changes().unwrap().len(),
            1,
            "the change comes back, since the save was the only thing holding it"
        );
        assert_eq!(
            after.lines[DEFAULT_LINE].current_change.as_deref(),
            Some(assigned.change.as_str()),
            "including its having been the current one"
        );
        assert_eq!(
            fs::read(dir.path().join("a.txt")).unwrap(),
            b"changed",
            "and undo of a save touches no working state, partial or not"
        );
    }

    #[test]
    fn materialising_with_no_preserved_working_state_refuses_rather_than_restoring_the_tip() {
        // Called directly: no caller reaches this today, and an unreachable
        // branch shipped without a test is how it stays wrong. Falling back to
        // the tip was safe only while a tip tree equalled the working tree it
        // came from — a partial save writes one that never existed on disk, so
        // the fallback would delete the unassigned work.
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("unassigned.txt"), b"not checkpointed").unwrap();
        repo.save("seed", None).unwrap();

        let err = repo.materialise_working_tree(None).unwrap_err();

        assert!(!err.recovery().is_empty());
        assert_eq!(
            fs::read(dir.path().join("unassigned.txt")).unwrap(),
            b"not checkpointed",
            "and it refused before touching anything"
        );
    }

    #[test]
    fn status_and_verify_say_when_a_checkpoint_holds_only_part_of_the_working_state() {
        // `status` is where a user looks before an operation that replaces
        // their bytes, and after a partial save a head printed with no
        // qualifier — or a verify that just says "verified" — is a claim the
        // engine can no longer support (ADR-17, consequences).
        let (dir, mut repo) = counted_ids(repo());
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::write(dir.path().join("b.txt"), b"b").unwrap();
        repo.save("seed", None).unwrap();
        assert_eq!(
            repo.status().unwrap().head_change,
            None,
            "a whole-working-state save carries no qualifier"
        );

        let assigned = repo.assign(&[dir.path().join("a.txt")], None).unwrap();
        repo.save("partial", Some(&assigned.short)).unwrap();

        assert_eq!(
            repo.status().unwrap().head_change.as_deref(),
            Some(assigned.change.as_str()),
            "the head holds one change, and says which"
        );
        let report = repo.verify(true).unwrap();
        assert!(report.errors.is_empty(), "and this is not damage");
        assert_eq!(report.checkpoints_partial, 1);
    }

    #[test]
    fn a_repository_with_no_changes_lists_none() {
        let (_dir, repo) = repo();
        assert!(repo.changes().unwrap().is_empty());
    }

    #[test]
    fn a_new_line_inherits_the_changes_open_on_the_line_it_starts_from() {
        // ADR-17 §4. `start` inherits the on-disk bytes, so it must inherit
        // the labelling of those bytes too: work that is still present but
        // whose unit of work silently vanished is a loss of user intent, and
        // nothing would report it.
        //
        // Seeded through the line state directly because `assign` does not
        // exist yet — the inheritance is otherwise unreachable, and an
        // unreachable branch shipped without a test is how it stays wrong.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"work").unwrap();
        repo.save("seed", None).unwrap();

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.changes.insert(
            "7f3a".into(),
            crate::oplog::ChangeRecord {
                assigned: [b"a.txt".to_vec()].into_iter().collect(),
            },
        );
        rec.current_change = Some("7f3a".into());
        repo.oplog.publish_lines(&lines).unwrap();

        repo.start_line("feature").unwrap();

        let after = repo.line_state().unwrap();
        let started = after.lines.get("feature").expect("the line exists");
        assert_eq!(
            started.current_change.as_deref(),
            Some("7f3a"),
            "the current change must survive `start`"
        );
        assert_eq!(
            started.changes.get("7f3a").map(|c| &c.assigned),
            Some(&[b"a.txt".to_vec()].into_iter().collect()),
            "the assignment must survive `start`, since the bytes did"
        );
        assert!(
            after.lines[DEFAULT_LINE].changes.contains_key("7f3a"),
            "and the line started FROM keeps its own copy"
        );
    }

    /// A change holding exactly these paths.
    fn change_of(paths: &[&[u8]]) -> crate::oplog::ChangeRecord {
        crate::oplog::ChangeRecord {
            assigned: paths.iter().map(|p| p.to_vec()).collect(),
        }
    }

    #[test]
    fn undoing_an_assign_removes_the_change_it_created_and_leaves_the_bytes_alone() {
        // The entry and the line state beside it are built by hand because
        // `assign` has not shipped: this is exactly the pair that command will
        // append. Building them here rather than waiting means the inverse
        // exists before the operation that needs it — an operation shipped
        // without its inverse is what silently truncates undo-all.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"work").unwrap();
        repo.save("seed", None).unwrap();

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.changes.insert("c1".into(), change_of(&[b"a.txt"]));
        rec.current_change = Some("c1".into());
        repo.oplog
            .commit(
                Operation::Assign {
                    change: "c1".into(),
                    line: DEFAULT_LINE.into(),
                    paths: vec![b"a.txt".to_vec()],
                    created: true,
                    from_current: None,
                    displaced: Vec::new(),
                },
                Some(lines),
            )
            .unwrap();

        let out = repo.undo().unwrap();
        assert!(!out.nothing_to_undo, "the assign was there to undo");

        let after = repo.line_state().unwrap();
        let rec = &after.lines[DEFAULT_LINE];
        assert!(
            rec.changes.is_empty(),
            "the change this assign created must go with it"
        );
        assert_eq!(rec.current_change, None, "and nothing is current again");
        assert_eq!(
            fs::read(dir.path().join("a.txt")).unwrap(),
            b"work",
            "and not a byte of working state moved: assign labels, it does not write"
        );
    }

    #[test]
    fn undoing_an_assign_returns_each_path_to_the_change_it_was_taken_from() {
        // Without the displaced record, undoing `assign --to c2 f` after
        // `assign --to c1 f` leaves `f` in no change at all rather than back
        // in c1 — the class of bug `StartLine::created` exists to prevent, one
        // level down.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"work").unwrap();
        repo.save("seed", None).unwrap();

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.changes.insert("c1".into(), change_of(&[b"a.txt"]));
        rec.current_change = Some("c1".into());
        repo.oplog
            .commit(
                Operation::Assign {
                    change: "c1".into(),
                    line: DEFAULT_LINE.into(),
                    paths: vec![b"a.txt".to_vec()],
                    created: true,
                    from_current: None,
                    displaced: Vec::new(),
                },
                Some(lines),
            )
            .unwrap();

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.changes.get_mut("c1").unwrap().assigned.clear();
        rec.changes.insert("c2".into(), change_of(&[b"a.txt"]));
        rec.current_change = Some("c2".into());
        repo.oplog
            .commit(
                Operation::Assign {
                    change: "c2".into(),
                    line: DEFAULT_LINE.into(),
                    paths: vec![b"a.txt".to_vec()],
                    created: true,
                    from_current: Some("c1".into()),
                    displaced: vec![(b"a.txt".to_vec(), "c1".into())],
                },
                Some(lines),
            )
            .unwrap();

        repo.undo().unwrap();

        let after = repo.line_state().unwrap();
        let rec = &after.lines[DEFAULT_LINE];
        assert_eq!(
            rec.changes.get("c1").map(|c| &c.assigned),
            Some(&change_of(&[b"a.txt"]).assigned),
            "the path must go back to the change it was taken from"
        );
        assert!(
            !rec.changes.contains_key("c2"),
            "and c2 went with its assign"
        );
        assert_eq!(
            rec.current_change.as_deref(),
            Some("c1"),
            "and the change that was current before is current again"
        );
    }

    #[test]
    fn undoing_an_assign_into_a_standing_change_leaves_the_change_itself_alone() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"work").unwrap();
        fs::write(dir.path().join("b.txt"), b"more").unwrap();
        repo.save("seed", None).unwrap();

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.changes.insert("c1".into(), change_of(&[b"a.txt"]));
        rec.current_change = Some("c1".into());
        repo.oplog
            .commit(
                Operation::Assign {
                    change: "c1".into(),
                    line: DEFAULT_LINE.into(),
                    paths: vec![b"a.txt".to_vec()],
                    created: true,
                    from_current: None,
                    displaced: Vec::new(),
                },
                Some(lines),
            )
            .unwrap();

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.changes
            .insert("c1".into(), change_of(&[b"a.txt", b"b.txt"]));
        repo.oplog
            .commit(
                Operation::Assign {
                    change: "c1".into(),
                    line: DEFAULT_LINE.into(),
                    paths: vec![b"b.txt".to_vec()],
                    created: false,
                    from_current: Some("c1".into()),
                    displaced: Vec::new(),
                },
                Some(lines),
            )
            .unwrap();

        repo.undo().unwrap();

        let after = repo.line_state().unwrap();
        let rec = &after.lines[DEFAULT_LINE];
        assert_eq!(
            rec.changes.get("c1").map(|c| &c.assigned),
            Some(&change_of(&[b"a.txt"]).assigned),
            "only the second assign's path leaves; the change the FIRST assign \
             created stands, with what that one put in it"
        );
    }

    #[test]
    fn an_assign_a_standing_checkpoint_consumed_is_not_undone_underneath_it() {
        // ADR-17 §8, and G1.3 structurally cannot catch it: the harness always
        // seeds a save first, so every checkpoint it makes has a parent. Here
        // there is no seed save, so the checkpoint sits at the root floor and
        // is permanently ineligible — and `next_undo_target` scans PAST an
        // ineligible entry. Without the guard, undo reaches the assign and
        // reverses it underneath the standing checkpoint that consumed it: the
        // change loses the assignments the line tip was derived from.
        //
        // The checkpoint is made by a plain save and the entry that consumes
        // the change is appended by hand, because `save --change` has not
        // shipped; that entry is what it will append.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"work").unwrap();

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.changes.insert("c1".into(), change_of(&[b"a.txt"]));
        rec.current_change = Some("c1".into());
        repo.oplog
            .commit(
                Operation::Assign {
                    change: "c1".into(),
                    line: DEFAULT_LINE.into(),
                    paths: vec![b"a.txt".to_vec()],
                    created: true,
                    from_current: None,
                    displaced: Vec::new(),
                },
                Some(lines),
            )
            .unwrap();

        let checkpoint = repo.save("x", None).unwrap().checkpoint;
        assert!(
            checkpoint.parent.is_none(),
            "the premise: this checkpoint is at the root floor"
        );
        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        let consumed = rec.changes.remove("c1").unwrap();
        rec.current_change = None;
        repo.oplog
            .commit(
                Operation::Save {
                    message: "x".into(),
                    checkpoint: checkpoint.id.clone(),
                    line: DEFAULT_LINE.into(),
                    change: Some(crate::oplog::CheckpointedChange {
                        id: "c1".into(),
                        record: consumed,
                        was_current: true,
                    }),
                },
                Some(lines),
            )
            .unwrap();

        let out = repo.undo().unwrap();

        assert!(
            out.nothing_to_undo,
            "at the root floor there is nothing left to undo, and saying so is \
             the honest answer"
        );
        assert!(
            repo.line_state().unwrap().lines[DEFAULT_LINE]
                .changes
                .is_empty(),
            "the assignment stays consumed by the checkpoint that took it"
        );
    }

    #[test]
    fn an_assign_is_not_undone_back_into_a_change_a_standing_checkpoint_consumed() {
        // The other half of the same floor (ADR-17 §8 states the rule for the
        // assign's own change; this is its displaced owners). Reversing this
        // assign would put a.txt back into c1 — but a standing checkpoint has
        // consumed c1, so it would come back as pending while its content is
        // already checkpointed: a change simultaneously checkpointed and
        // pending, which is the ambiguity §4.2's noun collapse removes.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"work").unwrap();
        fs::write(dir.path().join("b.txt"), b"more").unwrap();

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.changes
            .insert("c1".into(), change_of(&[b"a.txt", b"b.txt"]));
        rec.current_change = Some("c1".into());
        repo.oplog
            .commit(
                Operation::Assign {
                    change: "c1".into(),
                    line: DEFAULT_LINE.into(),
                    paths: vec![b"a.txt".to_vec(), b"b.txt".to_vec()],
                    created: true,
                    from_current: None,
                    displaced: Vec::new(),
                },
                Some(lines),
            )
            .unwrap();

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.changes.insert("c1".into(), change_of(&[b"b.txt"]));
        rec.changes.insert("c2".into(), change_of(&[b"a.txt"]));
        rec.current_change = Some("c2".into());
        repo.oplog
            .commit(
                Operation::Assign {
                    change: "c2".into(),
                    line: DEFAULT_LINE.into(),
                    paths: vec![b"a.txt".to_vec()],
                    created: true,
                    from_current: Some("c1".into()),
                    displaced: vec![(b"a.txt".to_vec(), "c1".into())],
                },
                Some(lines),
            )
            .unwrap();

        let checkpoint = repo.save("x", None).unwrap().checkpoint;
        assert!(
            checkpoint.parent.is_none(),
            "the premise: this checkpoint is at the root floor, so undo scans past it"
        );
        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        let consumed = rec.changes.remove("c1").unwrap();
        repo.oplog
            .commit(
                Operation::Save {
                    message: "x".into(),
                    checkpoint: checkpoint.id.clone(),
                    line: DEFAULT_LINE.into(),
                    change: Some(crate::oplog::CheckpointedChange {
                        id: "c1".into(),
                        record: consumed,
                        was_current: false,
                    }),
                },
                Some(lines),
            )
            .unwrap();

        let out = repo.undo().unwrap();

        assert!(out.nothing_to_undo, "everything left is below the floor");
        let after = repo.line_state().unwrap();
        assert!(
            !after.lines[DEFAULT_LINE].changes.contains_key("c1"),
            "the consumed change must not come back as pending"
        );
    }

    #[test]
    fn undoing_a_partial_save_gives_back_the_change_it_consumed() {
        // The consumed change is removed from the line state, so the save's
        // own entry is the only place its assignments survive.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"work").unwrap();
        repo.save("seed", None).unwrap();
        let checkpoint = repo.save("second", None).unwrap().checkpoint;
        assert!(
            checkpoint.parent.is_some(),
            "so the save is above the floor"
        );

        let mut lines = repo.line_state().unwrap();
        let rec = lines.lines.get_mut(DEFAULT_LINE).unwrap();
        rec.current_change = None;
        repo.oplog
            .commit(
                Operation::Save {
                    message: "second".into(),
                    checkpoint: checkpoint.id.clone(),
                    line: DEFAULT_LINE.into(),
                    change: Some(crate::oplog::CheckpointedChange {
                        id: "c1".into(),
                        record: change_of(&[b"a.txt"]),
                        was_current: true,
                    }),
                },
                Some(lines),
            )
            .unwrap();

        repo.undo().unwrap();

        let after = repo.line_state().unwrap();
        let rec = &after.lines[DEFAULT_LINE];
        assert_eq!(
            rec.changes.get("c1").map(|c| &c.assigned),
            Some(&change_of(&[b"a.txt"]).assigned),
            "the change comes back exactly as the save took it"
        );
        assert_eq!(
            rec.current_change.as_deref(),
            Some("c1"),
            "including its having been the current one"
        );
    }

    #[test]
    fn a_repository_outside_the_readable_format_range_is_refused_with_a_way_forward() {
        for (written, why) in [
            (MIN_READABLE_FORMAT - 1, "predates the per-entry format tag"),
            (FORMAT_VERSION + 1, "was written by a newer build"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            {
                let repo = Repo::init(dir.path()).unwrap();
                repo.oplog.set_format_version(written).unwrap();
            }
            let err = Repo::open(dir.path()).unwrap_err();
            assert_eq!(
                err.category(),
                crate::error::Category::Invalid,
                "format {written} ({why}) must be refused"
            );
            assert!(
                !err.recovery().is_empty(),
                "a refusal must carry a way back to safety"
            );
        }
    }

    #[test]
    fn checkout_refuses_a_destination_inside_the_repository() {
        // `checkout` overwrites what it finds and holds `&self`, so it cannot
        // capture the working state first the way switch, start and undo do.
        // Pointed at the root it therefore destroyed uncheckpointed work and
        // exited 0 reporting success. The bytes below exist nowhere else.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"checkpointed").unwrap();
        let cp = repo.save("one", None).unwrap().checkpoint;
        fs::write(dir.path().join("a.txt"), b"UNSAVED WORK").unwrap();

        let err = repo.checkout(&cp.id, dir.path()).unwrap_err();
        assert_eq!(err.category(), crate::error::Category::Invalid);
        assert_eq!(
            fs::read(dir.path().join("a.txt")).unwrap(),
            b"UNSAVED WORK",
            "uncheckpointed work must survive a refused checkout"
        );

        // A subdirectory is working state too — snapshot_dir walks the whole
        // root — so it is refused on the same grounds, not just the root.
        let err = repo.checkout(&cp.id, &dir.path().join("sub")).unwrap_err();
        assert_eq!(err.category(), crate::error::Category::Invalid);

        // A `..` that walks back INTO the repository must be refused too. The
        // first shape hides the return behind a component that does not exist
        // yet, so nothing on the way can be canonicalised in one step; the
        // second is a plain round trip. Both must resolve as "inside", and
        // must say so rather than failing with an unrelated I/O error.
        for sneaky in [
            dir.path().join("missing/../a.txt"),
            dir.path().join("../").join(dir.path().file_name().unwrap()),
        ] {
            let err = repo.checkout(&cp.id, &sneaky).unwrap_err();
            assert_eq!(
                err.category(),
                crate::error::Category::Invalid,
                "{} must be refused as inside the repository, not fail obscurely",
                sneaky.display()
            );
        }
        assert_eq!(
            fs::read(dir.path().join("a.txt")).unwrap(),
            b"UNSAVED WORK",
            "no traversal shape may reach the working state"
        );

        // Outside the repository it still works, which is what it is for.
        let (_hold, out) = outside("out");
        repo.checkout(&cp.id, &out).unwrap();
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"checkpointed");
    }

    #[cfg(unix)]
    #[test]
    fn checkout_refuses_a_symlinked_destination() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f.txt"), b"x").unwrap();
        let cp = repo.save("one", None).unwrap().checkpoint;
        // Both the link and its target sit outside the repository, so the
        // only guard that can fire here is the symlink one this test is for.
        let (hold, dest) = outside("dest_link");
        let target = hold.path().join("target");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &dest).unwrap();

        let err = repo.checkout(&cp.id, &dest).unwrap_err();
        assert_eq!(err.category(), crate::error::Category::Invalid);
        assert!(
            !target.join("f.txt").exists(),
            "a symlinked destination must be refused, not written through"
        );
    }

    #[cfg(unix)]
    #[test]
    fn checkout_does_not_write_through_a_destination_symlink() {
        let (dir, mut repo) = repo();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/f.txt"), b"safe").unwrap();
        let cp = repo.save("one", None).unwrap().checkpoint;

        // Destination pre-seeded with a symlink "sub" pointing OUTSIDE dest.
        let (hold, out) = outside("dest");
        fs::create_dir_all(&out).unwrap();
        let target = hold.path().join("target");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, out.join("sub")).unwrap();

        repo.checkout(&cp.id, &out).unwrap();

        assert!(
            out.join("sub/f.txt").exists(),
            "content must be written under dest"
        );
        assert!(
            !target.join("f.txt").exists(),
            "checkout must not follow a destination symlink and write outside dest"
        );
        assert!(
            !fs::symlink_metadata(out.join("sub"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the destination symlink must be replaced by a real directory"
        );
    }

    #[test]
    fn a_nested_dot_lattice_directory_is_captured() {
        let (dir, mut repo) = repo();
        fs::create_dir_all(dir.path().join("sub/.lattice")).unwrap();
        fs::write(dir.path().join("sub/.lattice/config"), b"nested").unwrap();
        let cp = repo.save("nested dot-lattice", None).unwrap().checkpoint;
        let (_hold, out) = outside("dest");
        repo.checkout(&cp.id, &out).unwrap();
        assert_eq!(
            fs::read(out.join("sub/.lattice/config")).unwrap(),
            b"nested",
            "a .lattice directory nested below the root must round-trip"
        );
    }

    #[test]
    fn verify_does_not_claim_clean_when_history_is_missing() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f.txt"), b"content").unwrap();
        repo.save("one", None).unwrap();
        drop(repo);

        // Remove every pack index: the checkpoint and its tree are now
        // unreadable, but the op-log still records the Save. A damaged
        // repository like this must NOT verify clean.
        let packs = dir.path().join(".lattice/packs");
        for e in fs::read_dir(&packs).unwrap() {
            let p = e.unwrap().path();
            if p.extension().and_then(|x| x.to_str()) == Some("idx") {
                fs::remove_file(p).unwrap();
            }
        }

        let repo = Repo::open(dir.path()).unwrap();
        let report = repo.verify(false).unwrap();
        assert!(
            !report.errors.is_empty(),
            "a repository missing its checkpoints must not verify clean"
        );
    }

    #[test]
    fn discover_walks_upward_from_a_subdirectory() {
        let (dir, repo) = repo();
        let deep = dir.path().join("a/b/c");
        fs::create_dir_all(&deep).unwrap();
        // redb holds an exclusive lock, so a second handle on the same
        // repository cannot be opened while the first is alive. That is a real
        // constraint on the workspace design (G1.4 runs 8 concurrently) and is
        // recorded here rather than worked around: workspaces will share one
        // handle rather than each opening the store.
        drop(repo);
        let found = Repo::discover(&deep).unwrap();
        assert_eq!(
            found.root().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn discover_outside_a_repository_names_the_way_back() {
        let dir = tempfile::tempdir().unwrap();
        let err = Repo::discover(dir.path()).unwrap_err();
        assert_eq!(err.category(), crate::error::Category::NotARepository);
        assert!(err.recovery().contains("ltx init"));
    }

    #[test]
    fn a_second_save_links_to_the_first() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"1").unwrap();
        let a = repo.save("one", None).unwrap().checkpoint;
        fs::write(dir.path().join("f"), b"2").unwrap();
        let b = repo.save("two", None).unwrap().checkpoint;
        assert_eq!(b.parent.as_deref(), Some(a.id.as_str()));
        assert_ne!(
            a.tree, b.tree,
            "changed content must change the tree address"
        );
    }

    #[test]
    fn a_small_edit_stores_only_the_changed_content() {
        let (dir, mut repo) = repo();
        for i in 0..5u32 {
            let content: Vec<u8> = (0..200_000u32)
                .map(|n| (n.wrapping_mul(2_654_435_761).wrapping_add(i)) as u8)
                .collect();
            fs::write(dir.path().join(format!("f{i}.bin")), &content).unwrap();
        }
        repo.save("one", None).unwrap();
        let after_first = repo.store().chunk_count();

        // Flip one byte in one file.
        let path = dir.path().join("f0.bin");
        let mut content = fs::read(&path).unwrap();
        content[100_000] ^= 0xFF;
        fs::write(&path, &content).unwrap();
        repo.save("two", None).unwrap();
        let added = repo.store().chunk_count() - after_first;

        // The second save must add only the changed file's affected chunks plus
        // the new tree and checkpoint blobs — not another copy of everything.
        // Without dedup the second save re-stores the whole tree, adding
        // roughly `after_first` again. With it, only the changed file's
        // affected chunks plus the new tree and checkpoint blobs are added.
        assert!(
            added < after_first / 2,
            "a one-byte edit added {added} chunks on top of {after_first}; \
             cross-pack dedup is not working"
        );
    }

    #[test]
    fn undo_moves_the_head_to_the_parent_and_stops_at_the_root() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"1").unwrap();
        let a = repo.save("one", None).unwrap().checkpoint; // root: parent is None
        fs::write(dir.path().join("f"), b"2").unwrap();
        let b = repo.save("two", None).unwrap().checkpoint;
        fs::write(dir.path().join("f"), b"3").unwrap();
        let c = repo.save("three", None).unwrap().checkpoint;

        let ids = |r: &Repo| {
            r.reachable_checkpoints()
                .unwrap()
                .into_iter()
                .map(|k| k.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&repo), vec![c.id.clone(), b.id.clone(), a.id.clone()]);

        let u1 = repo.undo().unwrap();
        assert!(!u1.nothing_to_undo);
        assert_eq!(u1.undone_checkpoint.as_deref(), Some(c.id.as_str()));
        assert_eq!(u1.now_at.as_deref(), Some(b.id.as_str()));
        assert!(u1.remote_effects_not_undone.is_empty());
        assert_eq!(ids(&repo), vec![b.id.clone(), a.id.clone()]);

        repo.undo().unwrap();
        assert_eq!(ids(&repo), vec![a.id.clone()]);

        // At the root there is nothing to undo, and no op is appended.
        let before = repo.oplog().len().unwrap();
        let u3 = repo.undo().unwrap();
        assert!(u3.nothing_to_undo);
        assert_eq!(u3.now_at, None);
        assert_eq!(
            repo.oplog().len().unwrap(),
            before,
            "nothing_to_undo must append no op"
        );
        assert_eq!(
            repo.head_checkpoint().unwrap().unwrap().id,
            a.id,
            "the head stays at the root"
        );
    }

    #[test]
    fn a_stale_head_file_does_not_override_the_durable_redb_state() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"1").unwrap();
        let a = repo.save("one", None).unwrap().checkpoint;
        fs::write(dir.path().join("f"), b"2").unwrap();
        let b = repo.save("two", None).unwrap().checkpoint;

        // The residue of a crash mid-undo: the durable redb line state moved
        // back to `a`, but the HEAD file still names `b` because the cache
        // write had not run. redb is the authority (rung 1); the file is a
        // cache (rung 3) and must not resurrect the newer id.
        let mut state = repo.line_state().unwrap();
        state.lines.get_mut(DEFAULT_LINE).unwrap().tip = Some(a.id.clone());
        repo.oplog().publish_lines(&state).unwrap();
        fs::write(repo.head_pointer_path(), &b.id).unwrap();

        assert_eq!(
            repo.head_checkpoint().unwrap().unwrap().id,
            a.id,
            "the durable redb line state must win over a staler HEAD file"
        );
    }

    #[test]
    fn init_resumes_an_interrupted_init_instead_of_bricking() {
        // A crash after `.lattice` was created but before anything was
        // committed leaves a directory `open` refuses (no format recorded) and
        // that `init` would also refuse (it exists) — no way forward at all.
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".lattice/packs")).unwrap();

        let mut repo = Repo::init(dir.path()).expect("init must resume an empty .lattice");
        fs::write(dir.path().join("f.txt"), b"x").unwrap();
        repo.save("after resume", None).unwrap();
        // redb holds one writer at a time, so release this handle before
        // asking for another — the constraint `discover` already documents.
        drop(repo);

        // But a directory with real history is still refused.
        let err = Repo::init(dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("already contains"),
            "a repository with history must still be refused, got: {err}"
        );
    }

    #[test]
    fn init_creates_the_default_line_below_the_undo_floor() {
        let (_dir, repo) = repo();
        let state = repo.lines().unwrap();
        assert_eq!(state.current, DEFAULT_LINE);
        assert!(state.lines.contains_key(DEFAULT_LINE));
        // main must not come from a StartLine entry, or undo-all would delete it.
        for entry in repo.oplog().entries().unwrap() {
            assert!(
                !matches!(entry.operation, Operation::StartLine { .. }),
                "the default line must not be created by an undoable entry"
            );
        }
    }

    #[test]
    fn start_creates_a_line_and_makes_it_current() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"1").unwrap();
        let seed = repo.save("seed", None).unwrap().checkpoint;

        let out = repo.start_line("feature").unwrap();
        assert!(out.created);
        assert_eq!(out.line, "feature");
        let state = repo.lines().unwrap();
        assert_eq!(state.current, "feature");
        // A new line inherits the tip, so no baseline checkpoint disappears.
        assert_eq!(
            state.lines["feature"].tip.as_deref(),
            Some(seed.id.as_str())
        );
    }

    #[test]
    fn start_of_an_existing_line_switches_and_succeeds() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"1").unwrap();
        repo.save("seed", None).unwrap();
        repo.start_line("feature").unwrap();
        repo.switch_line(DEFAULT_LINE).unwrap();

        // G1.4 draws `start <name>` thousands of times against one repository
        // and requires exit 0 every time.
        let out = repo.start_line("feature").unwrap();
        assert!(!out.created, "an existing line is not created again");
        assert_eq!(repo.lines().unwrap().current, "feature");
    }

    #[test]
    fn switch_preserves_this_lines_work_and_restores_the_others() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("shared.txt"), b"shared").unwrap();
        repo.save("seed", None).unwrap();

        repo.start_line("feature").unwrap();
        fs::write(dir.path().join("only-on-feature.txt"), b"work").unwrap();
        repo.save("feature work", None).unwrap();

        repo.switch_line(DEFAULT_LINE).unwrap();
        assert!(
            !dir.path().join("only-on-feature.txt").exists(),
            "the other line's file must not linger after switching away"
        );
        assert!(dir.path().join("shared.txt").exists());

        repo.switch_line("feature").unwrap();
        assert!(
            dir.path().join("only-on-feature.txt").exists(),
            "switching back must restore that line's working state"
        );
    }

    #[test]
    fn switching_to_an_unknown_line_is_refused_by_name() {
        let (_dir, mut repo) = repo();
        let err = repo.switch_line("nope").unwrap_err();
        assert_eq!(err.category(), crate::error::Category::NotFound);
        assert_eq!(err.concept(), crate::error::Concept::Line);
        let bad = repo.start_line("has space").unwrap_err();
        assert_eq!(bad.concept(), crate::error::Concept::Line);
    }

    #[cfg(unix)]
    #[test]
    fn work_written_after_an_interrupted_switch_is_captured_not_destroyed() {
        // A switch that publishes and then fails mid-materialise (here: an
        // unwritable directory) leaves a pending marker. The user fixes the
        // cause and keeps working. The next command completes the switch — and
        // must capture what is on disk BEFORE replacing it, or that work is
        // gone with no durable copy anywhere.
        use std::os::unix::fs::PermissionsExt;
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        repo.save("seed", None).unwrap();

        repo.start_line("other").unwrap();
        fs::create_dir(dir.path().join("blocked")).unwrap();
        fs::write(dir.path().join("blocked/b.txt"), b"b").unwrap();
        repo.save("other work", None).unwrap();

        fs::set_permissions(
            dir.path().join("blocked"),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        let failed = repo.switch_line(DEFAULT_LINE);
        fs::set_permissions(
            dir.path().join("blocked"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        // Root ignores the mode bits, so the switch legitimately succeeds
        // there and the scenario this test needs never arises.
        if failed.is_ok() {
            return;
        }

        fs::write(dir.path().join("URGENT.txt"), b"urgent").unwrap();
        let out = repo.save("later", None).unwrap();

        assert!(
            repo.store().contains(ChunkId::of(b"urgent")),
            "work written after an interrupted switch must be captured before \
             the switch is completed over it"
        );
        assert!(
            out.rescued_working_state.is_some(),
            "and the address must be NAMED in the result, not merely stored: \
             content nothing refers to is content a user cannot reach \
             (ADR-16 §6). `save` was the one caller that dropped it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn switch_never_deletes_through_a_symlinked_directory() {
        // A benign symlink (a shared assets dir, node_modules, a link to home)
        // plus a same-named directory on another line must never let switch
        // delete the LINK TARGET's contents, which are outside the repository.
        let (dir, mut repo) = repo();
        fs::create_dir(dir.path().join("data")).unwrap();
        fs::write(dir.path().join("data/keep.txt"), b"keep").unwrap();
        repo.save("seed", None).unwrap();

        repo.start_line("other").unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("precious.txt"), b"precious").unwrap();
        fs::create_dir(outside.path().join("nested")).unwrap();
        fs::write(outside.path().join("nested/deep.txt"), b"deep").unwrap();
        fs::remove_dir_all(dir.path().join("data")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("data")).unwrap();
        repo.save("linked", None).unwrap();

        repo.switch_line(DEFAULT_LINE).unwrap();

        assert!(
            outside.path().join("precious.txt").exists(),
            "switch deleted through a symlink, outside the working tree"
        );
        assert!(
            outside.path().join("nested/deep.txt").exists(),
            "switch deleted a subtree through a symlink"
        );
        // And the switch still did its job on this side of the link.
        assert!(dir.path().join("data/keep.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn switch_reconciles_a_path_that_changes_between_file_and_directory() {
        // `thing` is a file on main and a directory on the other line. Both
        // directions must materialise cleanly rather than failing half-applied.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("thing"), b"a file").unwrap();
        repo.save("seed", None).unwrap();

        repo.start_line("other").unwrap();
        fs::remove_file(dir.path().join("thing")).unwrap();
        fs::create_dir(dir.path().join("thing")).unwrap();
        fs::write(dir.path().join("thing/inner.txt"), b"inner").unwrap();
        repo.save("now a directory", None).unwrap();

        repo.switch_line(DEFAULT_LINE).unwrap();
        assert!(
            dir.path().join("thing").is_file(),
            "must become a file again"
        );

        repo.switch_line("other").unwrap();
        assert!(
            dir.path().join("thing/inner.txt").exists(),
            "must become a directory again"
        );
    }

    #[test]
    fn undo_of_start_removes_the_line_and_returns_to_the_previous_one() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"1").unwrap();
        repo.save("seed", None).unwrap();
        repo.start_line("feature").unwrap();
        assert_eq!(repo.lines().unwrap().current, "feature");

        let out = repo.undo().unwrap();
        assert!(!out.nothing_to_undo);
        let state = repo.lines().unwrap();
        assert_eq!(state.current, DEFAULT_LINE);
        assert!(
            !state.lines.contains_key("feature"),
            "undoing a start removes the line it created"
        );
    }

    #[test]
    fn undo_of_start_does_not_orphan_a_checkpoint_only_that_line_holds() {
        // With no seed, a save on a new line is a ROOT checkpoint that undo
        // cannot reverse. Undoing the start would delete the line and drop that
        // checkpoint out of every view, so the start is not undoable yet — the
        // floor is where it must be, not one step past it.
        let (dir, mut repo) = repo();
        repo.start_line("solo").unwrap();
        fs::write(dir.path().join("f"), b"1").unwrap();
        let cp = repo.save("only on solo", None).unwrap().checkpoint;

        let out = repo.undo().unwrap();
        assert!(
            out.nothing_to_undo,
            "a start still holding an unreversible save is at the floor"
        );
        let graph: Vec<String> = repo
            .log_view(true, None)
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert!(
            graph.contains(&cp.id),
            "the checkpoint must stay reachable, not be orphaned"
        );
    }

    #[test]
    fn undo_of_switch_returns_to_the_previous_line() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"1").unwrap();
        repo.save("seed", None).unwrap();
        repo.start_line("feature").unwrap();
        repo.switch_line(DEFAULT_LINE).unwrap();
        assert_eq!(repo.lines().unwrap().current, DEFAULT_LINE);

        repo.undo().unwrap();
        assert_eq!(
            repo.lines().unwrap().current,
            "feature",
            "undoing a switch returns to the line it left"
        );
    }

    #[test]
    fn a_zero_limit_returns_nothing_in_both_log_views() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"0").unwrap();
        repo.save("seed", None).unwrap();
        assert!(repo.log_view(false, Some(0)).unwrap().is_empty());
        assert!(
            repo.log_view(true, Some(0)).unwrap().is_empty(),
            "the forensic view must agree with the default one at the boundary"
        );
    }

    #[test]
    fn a_limited_forensic_view_returns_the_newest_across_all_lines() {
        // Walking one line to the limit and stopping would return that line's
        // newest N while omitting NEWER checkpoints on another line. The limit
        // has to select over the whole history, not the first line visited.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"0").unwrap();
        repo.save("seed", None).unwrap();

        // An older line, then a newer one; `zzz` sorts last by name but holds
        // the newest checkpoints, so name order and recency disagree.
        repo.start_line("aaa").unwrap();
        fs::write(dir.path().join("f"), b"1").unwrap();
        let a1 = repo.save("older line", None).unwrap().checkpoint;
        repo.switch_line(DEFAULT_LINE).unwrap();
        repo.start_line("zzz").unwrap();
        fs::write(dir.path().join("f"), b"2").unwrap();
        let z1 = repo.save("newer line", None).unwrap().checkpoint;
        fs::write(dir.path().join("f"), b"3").unwrap();
        let z2 = repo.save("newest", None).unwrap().checkpoint;

        let ids: Vec<String> = repo
            .log_view(true, Some(2))
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(
            ids,
            vec![z2.id.clone(), z1.id.clone()],
            "a limited forensic view must be the newest across every line"
        );
        assert!(
            !ids.contains(&a1.id),
            "an older line must not displace newer work"
        );
    }

    #[test]
    fn undo_all_with_lines_returns_to_the_seed_state() {
        // The G1.3 property in miniature, now with the `lines` domain live:
        // a batch of start/switch/save must undo-all back to the post-seed
        // state — same current line, same lines, same reachable graph.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("seed.txt"), b"seed\n").unwrap();
        repo.save("seed", None).unwrap();
        let lines_before = repo.lines().unwrap();
        let graph_before: Vec<String> = repo
            .log_view(true, None)
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();

        repo.start_line("probe-line").unwrap();
        repo.save("on probe", None).unwrap();
        repo.switch_line(DEFAULT_LINE).unwrap();
        repo.save("on main", None).unwrap();
        repo.start_line("probe-line").unwrap();

        for _ in 0..64 {
            if repo.undo().unwrap().nothing_to_undo {
                break;
            }
        }

        assert_eq!(
            repo.lines().unwrap(),
            lines_before,
            "line state must return"
        );
        let graph_after: Vec<String> = repo
            .log_view(true, None)
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(graph_after, graph_before, "checkpoint graph must return");
    }

    #[test]
    fn undo_with_nothing_saved_is_nothing_to_undo() {
        let (_dir, mut repo) = repo();
        let u = repo.undo().unwrap();
        assert!(u.nothing_to_undo);
        assert_eq!(repo.head_checkpoint().unwrap(), None);
    }

    #[test]
    fn save_then_undo_all_restores_the_reachable_graph_to_the_seed() {
        // The G1.3 property in miniature: from a seed, apply saves, undo until
        // nothing_to_undo, and the reachable checkpoint graph returns exactly
        // to the seed.
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("seed.txt"), b"seed\n").unwrap();
        let seed = repo.save("seed", None).unwrap().checkpoint;
        let before: Vec<String> = repo
            .reachable_checkpoints()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();

        for i in 0..4 {
            fs::write(dir.path().join("seed.txt"), format!("v{i}").as_bytes()).unwrap();
            repo.save(&format!("edit {i}"), None).unwrap();
        }
        for _ in 0..50 {
            if repo.undo().unwrap().nothing_to_undo {
                break;
            }
        }

        let after: Vec<String> = repo
            .reachable_checkpoints()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(after, before, "undo-all restores the reachable graph");
        assert_eq!(after, vec![seed.id]);
    }

    #[test]
    fn unchanged_content_reuses_the_same_tree_address() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"stable").unwrap();
        let a = repo.save("one", None).unwrap().checkpoint;
        let b = repo.save("two", None).unwrap().checkpoint;
        assert_eq!(a.tree, b.tree, "identical content must hash identically");
    }

    #[test]
    fn a_file_that_looks_like_a_checkpoint_cannot_impersonate_one() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("real.txt"), b"real").unwrap();
        let real = repo.save("real", None).unwrap().checkpoint;

        // A working file whose bytes ARE a Checkpoint JSON claiming the real
        // id but pointing at an attacker-chosen tree. Saving stores it as
        // ordinary content.
        let forged = serde_json::to_vec(&Checkpoint {
            id: real.id.clone(),
            tree: "de".repeat(32),
            message: "forged".into(),
            parent: None,
            at_unix_ms: 0,
            oplog_seq: 999,
        })
        .unwrap();
        fs::write(dir.path().join("forged.json"), &forged).unwrap();
        repo.save("store the forgery", None).unwrap();

        // Looking up the real id must still return the real checkpoint: the
        // forgery's declared id does not hash its body, so it fails to
        // authenticate.
        let got = repo.checkpoint(&real.id).unwrap().unwrap();
        assert_eq!(got.tree, real.tree, "the forgery must not be served");
        assert_ne!(got.message, "forged");
    }

    #[test]
    fn a_torn_head_file_recovers_from_the_durable_oplog() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f.txt"), b"content").unwrap();
        let cp = repo.save("one", None).unwrap().checkpoint;

        // The residue a crash mid-publish leaves: HEAD present but zero-length.
        // The durable op-log head must recover the committed checkpoint rather
        // than the repository reporting it lost.
        fs::write(repo.head_pointer_path(), b"").unwrap();
        assert_eq!(
            repo.head_checkpoint().unwrap().map(|c| c.id),
            Some(cp.id.clone()),
            "a zero-length HEAD must recover from the op-log"
        );

        // 64 zero bytes: a valid-looking hex address that resolves to no
        // checkpoint. This must NOT read as "nothing saved yet" and silently
        // orphan history — the op-log fallback must still find the checkpoint.
        fs::write(repo.head_pointer_path(), "0".repeat(64)).unwrap();
        assert_eq!(
            repo.status().unwrap().head.as_deref(),
            Some(cp.id.as_str()),
            "a zero-filled HEAD must not orphan committed history"
        );
    }
}
