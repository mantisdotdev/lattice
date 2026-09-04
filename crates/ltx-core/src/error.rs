//! Errors.
//!
//! §4.3 makes an invariant of this: "No error message without a stated way back
//! to safety." G2.4 turns that into a HARD, machine-checked gate — every error
//! path must carry a recovery action, a causal category, and the §4.2 concept
//! involved. So the recovery text lives on the error type itself rather than
//! being written at each call site, where it would be forgotten exactly once
//! and then forever.

use std::path::PathBuf;

/// The causal category G2.4 requires. Machine-readable so the gate can check it
/// rather than parse prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// The repository is not where the command expected one.
    NotARepository,
    /// The request named something that does not exist.
    NotFound,
    /// Stored bytes do not match what the store promised.
    Corrupt,
    /// The filesystem or OS refused.
    Io,
    /// The caller asked for something the model does not allow.
    Invalid,
}

/// The §4.2 concept an error is about. Kept to the seven nouns, plus `None`
/// for errors that are genuinely about the machine rather than the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Concept {
    WorkingState,
    Change,
    Checkpoint,
    Line,
    Lens,
    Workspace,
    Remote,
    None,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no Lattice repository here (looked in {0} and its parents)")]
    NotARepository(PathBuf),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Corrupt(String),

    #[error("{0}")]
    Invalid(String),

    /// A line that does not exist. Distinct from NotFound so the §4.2 concept
    /// G2.4 reports is `line` rather than `checkpoint`.
    #[error("{0}")]
    NoSuchLine(String),

    /// A name that cannot be a line. Distinct from Invalid for the same reason.
    #[error("{0}")]
    InvalidLine(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Database(#[from] Box<redb::Error>),

    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

impl Error {
    pub fn category(&self) -> Category {
        match self {
            Error::NotARepository(_) => Category::NotARepository,
            Error::NotFound(_) => Category::NotFound,
            Error::Corrupt(_) => Category::Corrupt,
            Error::Invalid(_) | Error::InvalidLine(_) => Category::Invalid,
            Error::NoSuchLine(_) => Category::NotFound,
            Error::Io(_) | Error::Database(_) | Error::Serde(_) => Category::Io,
        }
    }

    pub fn concept(&self) -> Concept {
        match self {
            Error::NotARepository(_) => Concept::Workspace,
            Error::NotFound(_) => Concept::Checkpoint,
            Error::Corrupt(_) => Concept::Checkpoint,
            Error::Invalid(_) => Concept::WorkingState,
            Error::NoSuchLine(_) | Error::InvalidLine(_) => Concept::Line,
            Error::Io(_) | Error::Database(_) | Error::Serde(_) => Concept::None,
        }
    }

    /// The way back to safety. Never empty — G2.4 fails an error without one,
    /// and a user facing a dead end is the thing §4.3 exists to prevent.
    pub fn recovery(&self) -> &'static str {
        // Every command named here MUST exist in the shipped CLI — a user who
        // follows the advice must not hit "unrecognized subcommand". The test
        // `recovery_actions_name_only_implemented_commands` enforces that, so
        // adopt/sync/undo cannot be advertised before they are built.
        match self {
            Error::NotARepository(_) => "run `ltx init` here to start a repository",
            Error::NotFound(_) => "run `ltx log` to see what has been saved so far",
            Error::Corrupt(_) => {
                "run `ltx verify --complete` for the full report of what is damaged"
            }
            Error::Invalid(_) => {
                "run `ltx status` to see the current state, then reissue the command \
                 with a valid argument"
            }
            Error::NoSuchLine(_) => "run `ltx line list` to see which lines exist",
            Error::InvalidLine(_) => {
                "choose a name of letters, digits, dot, underscore, dash or slash; \
                 run `ltx line list` to see the lines that exist"
            }
            Error::Io(_) => {
                "check permissions and free space on the repository directory, then \
                 retry; run `ltx verify` to confirm nothing was lost, since no \
                 checkpoint is written until the operation completes"
            }
            Error::Database(_) => "run `ltx verify` to check the repository for damage",
            Error::Serde(_) => {
                "run `ltx verify` to check whether the repository is intact; if it \
                 reports no damage, this is a bug in Lattice"
            }
        }
    }
}

impl From<redb::Error> for Error {
    fn from(e: redb::Error) -> Self {
        Error::Database(Box::new(e))
    }
}

macro_rules! redb_from {
    ($($t:ty),*) => {$(
        impl From<$t> for Error {
            fn from(e: $t) -> Self { Error::Database(Box::new(e.into())) }
        }
    )*};
}
redb_from!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError
);

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_carries_a_recovery_action() {
        // G2.4 in miniature: asserted here so a new variant without recovery
        // text fails at `cargo test` rather than at the gate.
        let cases: Vec<Error> = vec![
            Error::NotARepository(PathBuf::from("/tmp")),
            Error::NotFound("checkpoint".into()),
            Error::Corrupt("pack".into()),
            Error::Invalid("bad".into()),
            Error::Io(std::io::Error::other("x")),
        ];
        for e in cases {
            assert!(!e.recovery().is_empty(), "{e:?} has no recovery action");
            assert!(
                e.recovery().contains("ltx "),
                "{e:?} recovery names no command to run: {}",
                e.recovery()
            );
        }
    }

    #[test]
    fn recovery_actions_name_only_implemented_commands() {
        // A user who runs the suggested command must not hit "unrecognized
        // subcommand". This keeps the advice tracking the actual CLI surface,
        // so adopt/sync/undo cannot be advertised before they are built.
        const IMPLEMENTED: &[&str] = &[
            "init", "save", "status", "log", "verify", "checkout", "undo", "start", "switch",
            "line",
        ];
        let cases: Vec<Error> = vec![
            Error::NotARepository(PathBuf::from("/tmp")),
            Error::NotFound("x".into()),
            Error::Corrupt("x".into()),
            Error::Invalid("x".into()),
            Error::Io(std::io::Error::other("x")),
            Error::NoSuchLine("x".into()),
            Error::InvalidLine("x".into()),
            Error::Serde(serde_json::from_str::<i32>("nope").unwrap_err()),
        ];
        for e in cases {
            let text = e.recovery();
            let mut rest = text;
            while let Some(pos) = rest.find("ltx ") {
                rest = &rest[pos + 4..];
                let word: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                assert!(
                    IMPLEMENTED.contains(&word.as_str()),
                    "{e:?} recovery names unimplemented command `ltx {word}`: {text}"
                );
            }
        }
    }
}
