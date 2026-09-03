//! The repository: the one public entry point to everything.
//!
//! §8 constrains the shape — "the CLI contains no logic the API lacks" — so
//! every operation lives here as a method and the CLI is a thin translation of
//! argv into these calls. G5.5 measures that as a HARD gate, and the way to
//! pass it is to never write logic anywhere else.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::chunk::ChunkId;
use crate::error::{Error, Result};
use crate::oplog::{Entry, OpLog, Operation};
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
    /// The op-log sequence that created this. Ties history to the audit record.
    pub oplog_seq: u64,
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
        let body = serde_json::to_vec(&(
            &checkpoint.tree,
            &checkpoint.message,
            &checkpoint.parent,
            at,
        ))?;
        checkpoint.id = ChunkId::of(&body).to_hex();

        // Content durable BEFORE the metadata referencing it (ADR-3). A crash
        // between these two leaves unreferenced chunks, never a checkpoint
        // pointing at content that was never written.
        let checkpoint_bytes = serde_json::to_vec(&checkpoint)?;
        packer.add(ChunkId::of(&checkpoint_bytes), &checkpoint_bytes);
        self.store.write_pack(packer)?;

        let entry = self.oplog.append(Operation::Save {
            message: message.to_string(),
            checkpoint: checkpoint.id.clone(),
        })?;
        checkpoint.oplog_seq = entry.seq;

        // Re-store with the sequence filled in, then move the head.
        let mut packer = PackWriter::new();
        let final_bytes = serde_json::to_vec(&checkpoint)?;
        packer.add(ChunkId::of(&final_bytes), &final_bytes);
        self.store.write_pack(packer)?;
        self.oplog.set_head("checkpoint", entry.seq)?;
        self.write_head_pointer(&checkpoint.id)?;

        Ok(checkpoint)
    }

    fn head_pointer_path(&self) -> PathBuf {
        Self::repo_dir(&self.root).join("HEAD")
    }

    fn write_head_pointer(&self, checkpoint_id: &str) -> Result<()> {
        let path = self.head_pointer_path();
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, checkpoint_id)?;
        // Atomic replace, then sync the directory so the rename itself is
        // durable — a rename is only committed once the containing directory
        // is fsynced, which is the barrier G1.1's replayer models.
        fs::rename(&tmp, &path)?;
        let dir = fs::File::open(Self::repo_dir(&self.root))?;
        dir.sync_all()?;
        Ok(())
    }

    pub fn head_checkpoint(&self) -> Result<Option<Checkpoint>> {
        let path = self.head_pointer_path();
        if !path.exists() {
            return Ok(None);
        }
        let id = fs::read_to_string(&path)?;
        self.checkpoint(id.trim())
    }

    pub fn checkpoint(&self, id: &str) -> Result<Option<Checkpoint>> {
        let Some(chunk_id) = ChunkId::from_hex(id) else {
            return Err(Error::Invalid(format!("{id} is not a checkpoint address")));
        };
        // A checkpoint is content-addressed like everything else, but its own
        // address is over its body rather than its serialised form, so the
        // lookup is by scanning the addresses we know. Small and adequate for
        // the current history sizes; a checkpoint index is a later refinement.
        let _ = chunk_id;
        for candidate in self.store.all_chunk_ids() {
            if let Some(bytes) = self.store.read(candidate)? {
                if let Ok(cp) = serde_json::from_slice::<Checkpoint>(&bytes) {
                    if cp.id == id {
                        return Ok(Some(cp));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Every checkpoint, newest first.
    pub fn checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let mut out: Vec<Checkpoint> = Vec::new();
        for candidate in self.store.all_chunk_ids() {
            if let Some(bytes) = self.store.read(candidate)? {
                if let Ok(cp) = serde_json::from_slice::<Checkpoint>(&bytes) {
                    if !cp.id.is_empty() && cp.oplog_seq > 0 {
                        out.push(cp);
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
        let mut tree = Tree::new();
        let mut names: Vec<_> = fs::read_dir(dir)?.collect::<std::result::Result<_, _>>()?;
        // Sort by raw name bytes so the tree is canonical regardless of the
        // order the filesystem enumerated.
        names.sort_by_key(|e| e.file_name().into_encoded_bytes());

        for entry in names {
            let name = entry.file_name();
            let raw = name.as_encoded_bytes().to_vec();
            if raw == REPO_DIR.as_bytes() {
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
                let mode = meta.permissions().mode();
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
            entries_written: 0,
            collisions: Vec::new(),
        };
        self.restore_tree(&cp.tree, dest, &mut report)?;
        Ok(report)
    }

    fn restore_tree(&self, tree_id: &str, dest: &Path, report: &mut CheckoutReport) -> Result<u64> {
        let Some(id) = ChunkId::from_hex(tree_id) else {
            return Err(Error::Corrupt(format!("{tree_id} is not a tree address")));
        };
        let Some(bytes) = self.store.read(id)? else {
            return Err(Error::NotFound(format!(
                "tree {tree_id} is not present locally"
            )));
        };
        let tree = Tree::from_bytes(&bytes)?;
        let mut written = 0u64;
        // Names this checkout has already placed in THIS directory, keyed by
        // the exact bytes. Used to tell "the filesystem folded two names
        // together" apart from "we are overwriting our own earlier write".
        let mut placed: Vec<Vec<u8>> = Vec::new();

        for (name, node) in &tree.entries {
            let path = dest.join(bytes_to_os(name));

            // If the target already exists but this checkout has not written
            // that exact name, the filesystem has folded two distinct names
            // into one. Writing anyway would destroy the sibling we already
            // placed.
            if !placed.contains(name) && fs::symlink_metadata(&path).is_ok() {
                let sibling = placed
                    .iter()
                    .find(|prior| folds_together(prior, name))
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .unwrap_or_else(|| "an earlier entry".to_string());
                report.collisions.push(Collision {
                    path: String::from_utf8_lossy(name).into_owned(),
                    collided_with: sibling,
                    reason: "this filesystem does not distinguish these names \
                             (case folding or Unicode normalisation)"
                        .to_string(),
                });
                continue;
            }
            placed.push(name.clone());
            match node {
                Node::Directory { tree } => {
                    fs::create_dir_all(&path)?;
                    written += self.restore_tree(tree, &path, report)?;
                }
                Node::Symlink { target } => {
                    if path.exists() || fs::symlink_metadata(&path).is_ok() {
                        fs::remove_file(&path).ok();
                    }
                    std::os::unix::fs::symlink(bytes_to_os(target), &path)?;
                    written += 1;
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
                    fs::set_permissions(&path, fs::Permissions::from_mode(*mode))?;
                    written += 1;
                }
            }
        }
        Ok(written)
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

        for cp in self.checkpoints()? {
            report.checkpoints += 1;
            if let Err(e) = self.verify_tree(&cp.tree, &mut report) {
                report.structure_verified = false;
                report
                    .errors
                    .push(format!("checkpoint {}: {e}", &cp.id[..12.min(cp.id.len())]));
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
            report.chunks_absent += 1;
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

/// Would these two names be the same file on a folding filesystem?
///
/// ASCII case folding plus a coarse NFC/NFD equivalence. Deliberately
/// approximate: it only decides which sibling to NAME in a collision report,
/// never whether a collision occurred — that is decided by asking the
/// filesystem, which is the only authority on what it folds.
fn folds_together(a: &[u8], b: &[u8]) -> bool {
    let norm = |s: &[u8]| -> Vec<u8> {
        s.iter()
            .map(|c| c.to_ascii_lowercase())
            .filter(|c| *c != 0xcc && *c != 0x81) // strip a common combining mark
            .collect()
    };
    norm(a) == norm(b)
}

fn bytes_to_os(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(bytes.to_vec())
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
    fn the_executable_bit_round_trips() {
        let (dir, mut repo) = repo();
        let script = dir.path().join("run.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let cp = repo.save("executable").unwrap();
        let out = dir.path().join("out");
        repo.checkout(&cp.id, &out).unwrap();
        let mode = fs::metadata(out.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
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
    fn unchanged_content_reuses_the_same_tree_address() {
        let (dir, mut repo) = repo();
        fs::write(dir.path().join("f"), b"stable").unwrap();
        let a = repo.save("one").unwrap();
        let b = repo.save("two").unwrap();
        assert_eq!(a.tree, b.tree, "identical content must hash identically");
    }
}
