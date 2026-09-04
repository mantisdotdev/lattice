//! Lattice engine (LTX).
//!
//! All logic lives in this crate. `ltx` is a CLI shell over it and `ltx-daemon`
//! is an API shell over it; §8 requires that "the CLI contains no logic the API
//! lacks", and the only way to keep that true is to give both the same one
//! entry point.
//!
//! Nothing here can detect that it is being measured. §0.3 makes that a
//! grep-able rule, and `harness/lib/check_no_harness_leak.py` fails CI on any
//! reference to a harness, bench path, or test-mode environment variable in
//! this crate.

pub mod chunk;
pub mod error;
pub mod oplog;
pub mod platform;
pub mod repo;
pub mod store;
pub mod tree;

pub use chunk::{Chunk, ChunkId};
pub use error::{Category, Concept, Error, Result};
pub use oplog::{Entry, LineRecord, LineState, OpLog, Operation, DEFAULT_LINE};
pub use repo::{Checkpoint, LineOutcome, Repo, Status, UndoOutcome, VerifyReport};
pub use store::Store;
pub use tree::{Node, Tree};

/// A short, display-only prefix of an id, truncated on a UTF-8 char boundary.
///
/// Ids the engine mints are 64-hex and always sliceable at 12, but the same
/// slicing runs over `prev`/`id`/`checkpoint`/`tree` strings deserialised from
/// a possibly-tampered op-log or tree, where byte 12 can fall inside a
/// multibyte codepoint. A raw `&s[..12]` panics there — in the very code paths
/// meant to REPORT the tampering — so every such truncation goes through here.
pub fn short_id(s: &str) -> &str {
    let mut end = s.len().min(12);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The seven user-facing nouns (§4.2), as a machine-readable list.
///
/// G2.5 requires the concept model to ship as a schema the contract docs
/// reference, and G2.3's vocabulary lint checks help text and errors against
/// exactly this set. Keeping it here means the lint and the product cannot
/// drift apart.
pub const CONCEPTS: [&str; 7] = [
    "working state",
    "change",
    "checkpoint",
    "line",
    "lens",
    "workspace",
    "remote",
];
