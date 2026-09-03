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

use crate::chunk::ChunkId;
use crate::error::{Error, Result};
use crate::oplog::{Entry, OpLog, Operation};
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
}

impl Repo {
    fn repo_dir(root: &Path) -> PathBuf {
        root.join(REPO_DIR)
    }

    /// Create a repository here.
    pub fn init(root: &Path) -> Result<Self> {
        let dir = Self::repo_dir(root);
        if dir.exists() {
            return Err(Error::Invalid(format!(
                "{} already contains a Lattice repository",
                root.display()
            )));
        }
        fs::create_dir_all(dir.join("packs"))?;
        let store = Store::open(&dir.join("packs"))?;
        let oplog = OpLog::open(&dir.join("meta.redb"))?;
        oplog.append(Operation::Init)?;
        Ok(Repo {
            root: root.to_path_buf(),
            store,
            oplog,
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
        Ok(Repo {
            root: root.to_path_buf(),
            store,
            oplog,
        })
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
    pub fn save(&mut self, message: &str) -> Result<Checkpoint> {
        let mut packer = PackWriter::new();
        let tree_id = self.snapshot_dir(&self.root.clone(), &mut packer)?;

        let parent = self.head_checkpoint()?;
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

        let entry = self.oplog.append(Operation::Save {
            message: message.to_string(),
            checkpoint: checkpoint.id.clone(),
        })?;
        self.oplog.set_head("checkpoint", entry.seq)?;
        self.write_head_pointer(&checkpoint.id)?;

        // The caller's copy carries the real sequence; reads resolve the same
        // value from the op-log, so the stored blob's 0 is never observed.
        checkpoint.oplog_seq = entry.seq;
        Ok(checkpoint)
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

    pub fn head_checkpoint(&self) -> Result<Option<Checkpoint>> {
        // The HEAD file is a convenience cache; the authoritative record of the
        // current checkpoint is the durable op-log head (redb, crash-atomic).
        // If the file is present and names a real checkpoint, use it. If it is
        // missing, empty, or torn — the exact residue of a crash mid-publish —
        // fall back to the op-log rather than reporting "nothing saved", which
        // would silently orphan history that was in fact committed.
        let path = self.head_pointer_path();
        if let Ok(raw) = fs::read_to_string(&path) {
            let id = raw.trim();
            if ChunkId::from_hex(id).is_some() {
                if let Some(cp) = self.checkpoint(id)? {
                    return Ok(Some(cp));
                }
            }
        }
        // File absent or unusable: recover from the durable op-log head.
        self.durable_head_checkpoint()
    }

    /// Resolve the current checkpoint from the durable op-log head alone.
    ///
    /// `save` records `set_head("checkpoint", seq)` in redb before publishing
    /// the HEAD file, so this survives a crash that loses the file. The Save
    /// entry at that sequence carries the checkpoint id.
    fn durable_head_checkpoint(&self) -> Result<Option<Checkpoint>> {
        let Some(seq) = self.oplog.get_head("checkpoint")? else {
            return Ok(None);
        };
        let Some(entry) = self.oplog.get(seq)? else {
            return Ok(None);
        };
        match entry.operation {
            Operation::Save { checkpoint, .. } => self.checkpoint(&checkpoint),
            _ => Ok(None),
        }
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
        for entry in self.oplog.entries()? {
            if let Operation::Save { checkpoint, .. } = &entry.operation {
                if checkpoint == checkpoint_id {
                    return Ok(Some(entry.seq));
                }
            }
        }
        Ok(None)
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

            if meta.file_type().is_symlink() {
                let target = fs::read_link(&path)?;
                tree.entries.insert(
                    raw,
                    Node::Symlink {
                        target: target.as_os_str().as_encoded_bytes().to_vec(),
                    },
                );
            } else if meta.is_dir() {
                let child = self.snapshot_dir(&path, packer)?;
                tree.entries.insert(raw, Node::Directory { tree: child });
            } else {
                let content = fs::read(&path)?;
                let mode = platform::file_mode(&meta);
                tree.entries
                    .insert(raw, tree::file_node(&content, mode, packer));
            }
        }
        tree.write(packer)
    }

    // ------------------------------------------------------------ checkout

    /// Materialise a checkpoint's tree into `dest`.
    pub fn checkout(&self, checkpoint_id: &str, dest: &Path) -> Result<CheckoutReport> {
        let Some(cp) = self.checkpoint(checkpoint_id)? else {
            return Err(Error::NotFound(format!("no checkpoint {checkpoint_id}")));
        };
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
        Ok(Status {
            root: self.root.display().to_string(),
            head: head.as_ref().map(|c| c.id.clone()),
            head_message: head.as_ref().map(|c| c.message.clone()),
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

        let cp = repo.save("first").unwrap();
        let out = dir.path().join("restored");
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
        let cp = repo.save("adversarial").unwrap();
        let out = dir.path().join("out");
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
        let cp = repo.save("with a link").unwrap();
        let out = dir.path().join("out");
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
        let cp = repo.save("dangling").unwrap();
        let out = dir.path().join("out");
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
        let cp = repo.save("executable").unwrap();
        let out = dir.path().join("out");
        repo.checkout(&cp.id, &out).unwrap();
        let mode = crate::platform::file_mode(&fs::metadata(out.join("run.sh")).unwrap());
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn verify_reports_coverage_and_only_complete_claims_completeness() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f.txt"), b"content").unwrap();
        repo.save("one").unwrap();

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
        let cp = repo.save("two files").unwrap();

        let out = dir.path().join("dest");
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

    #[cfg(unix)]
    #[test]
    fn checkout_does_not_write_through_a_destination_symlink() {
        let (dir, mut repo) = repo();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/f.txt"), b"safe").unwrap();
        let cp = repo.save("one").unwrap();

        // Destination pre-seeded with a symlink "sub" pointing OUTSIDE dest.
        let out = dir.path().join("dest");
        fs::create_dir_all(&out).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, out.join("sub")).unwrap();

        repo.checkout(&cp.id, &out).unwrap();

        assert!(
            out.join("sub/f.txt").exists(),
            "content must be written under dest"
        );
        assert!(
            !outside.join("f.txt").exists(),
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
        let cp = repo.save("nested dot-lattice").unwrap();
        let out = dir.path().join("dest");
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
        repo.save("one").unwrap();
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
        let a = repo.save("one").unwrap();
        fs::write(dir.path().join("f"), b"2").unwrap();
        let b = repo.save("two").unwrap();
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
        repo.save("one").unwrap();
        let after_first = repo.store().chunk_count();

        // Flip one byte in one file.
        let path = dir.path().join("f0.bin");
        let mut content = fs::read(&path).unwrap();
        content[100_000] ^= 0xFF;
        fs::write(&path, &content).unwrap();
        repo.save("two").unwrap();
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
    fn unchanged_content_reuses_the_same_tree_address() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"stable").unwrap();
        let a = repo.save("one").unwrap();
        let b = repo.save("two").unwrap();
        assert_eq!(a.tree, b.tree, "identical content must hash identically");
    }

    #[test]
    fn a_file_that_looks_like_a_checkpoint_cannot_impersonate_one() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("real.txt"), b"real").unwrap();
        let real = repo.save("real").unwrap();

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
        repo.save("store the forgery").unwrap();

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
        let cp = repo.save("one").unwrap();

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
