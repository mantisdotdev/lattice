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
pub use oplog::{Entry, OpLog, Operation};
pub use repo::{Checkpoint, Repo, Status, VerifyReport};
pub use store::Store;
pub use tree::{Node, Tree};

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
