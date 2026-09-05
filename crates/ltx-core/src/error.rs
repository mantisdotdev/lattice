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
    /// Nothing is wrong; the repository was in use. Its own category because
    /// it is the only one where the answer is to try the SAME command again,
    /// and a caller told `invalid` would rightly not.
    Busy,
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

    /// A change that is not open on this line. Distinct from NotFound so the
    /// §4.2 concept G2.4 reports is `change` rather than `checkpoint` — and
    /// `Concept::Change` had no error variant producing it until now, so no
    /// change-concept error path had ever been measured.
    #[error("{0}")]
    NoSuchChange(String),

    /// A change reference that names several open changes at once.
    #[error("{0}")]
    InvalidChange(String),

    /// A change a checkpoint has already taken. Its own variant because the
    /// way back is different in kind: there is nothing to list and nothing to
    /// disambiguate — that unit of work is closed, and further work starts a
    /// new one.
    #[error("{0}")]
    ChangeAlreadyCheckpointed(String),

    /// A change with no paths in it, asked to be checkpointed. Again its own
    /// variant for its own way back: the change is open and unambiguous, it
    /// simply has nothing in it yet.
    #[error("{0}")]
    ChangeHoldsNothing(String),

    /// Another command holds the repository and did not let go in time.
    ///
    /// Its own variant because the way back is unlike any other: nothing is
    /// wrong, nothing is damaged, and the answer is to let the other command
    /// finish rather than to inspect or repair anything.
    #[error("{0}")]
    Busy(String),

    /// The repository was written in an on-disk format this build cannot read.
    #[error("{0}")]
    UnsupportedFormat(String),

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
            Error::Invalid(_) | Error::InvalidLine(_) | Error::UnsupportedFormat(_) => {
                Category::Invalid
            }
            Error::NoSuchLine(_) | Error::NoSuchChange(_) => Category::NotFound,
            // Neither `Io` nor `Invalid`: nothing failed, and the command was
            // not wrong. The repository was in use, which is a state the model
            // allows — and the only one where retrying unchanged is right.
            Error::Busy(_) => Category::Busy,
            Error::InvalidChange(_)
            | Error::ChangeAlreadyCheckpointed(_)
            | Error::ChangeHoldsNothing(_) => Category::Invalid,
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
            Error::NoSuchChange(_)
            | Error::InvalidChange(_)
            | Error::ChangeAlreadyCheckpointed(_)
            | Error::ChangeHoldsNothing(_) => Concept::Change,
            Error::Busy(_) | Error::UnsupportedFormat(_) => Concept::Workspace,
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
            Error::NoSuchChange(_) => "run `ltx change list` to see which changes are open",
            Error::InvalidChange(_) => {
                "run `ltx change list` and name enough characters to pick out the \
                 one you mean"
            }
            // NOT "run `ltx change list`": the change is not in it, which is
            // the whole point, and sending a user to a list that cannot
            // contain what they asked for is the dead end §4.3 forbids.
            Error::ChangeAlreadyCheckpointed(_) => {
                "that unit of work is checkpointed; run `ltx assign <path>` to \
                 start a new change for what comes next"
            }
            Error::ChangeHoldsNothing(_) => {
                "run `ltx assign <path>` to put something in it, or `ltx save \
                 \"<message>\"` to checkpoint the whole working state"
            }
            Error::Busy(_) => {
                "another command is using this repository; run `ltx status` once \
                 it finishes, or find the process still holding it"
            }
            Error::UnsupportedFormat(_) => {
                "this repository predates the current on-disk format; start a fresh \
                 one with `ltx init` and re-save your work into it"
            }
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
    fn a_busy_repository_is_neither_a_failure_nor_a_bad_command() {
        // The category is what a caller reads to decide whether to retry, and
        // it is the whole reason this variant is not folded into Io or
        // Invalid: nothing failed, and the command was right. Retrying it
        // unchanged is the correct response, and no other category means that.
        let busy = Error::Busy("held".into());
        assert_eq!(busy.category(), Category::Busy);
        assert_eq!(busy.concept(), Concept::Workspace);
        assert!(
            !busy.recovery().contains("verify"),
            "nothing is damaged, so the way back is not an inspection: {}",
            busy.recovery()
        );
    }

    #[test]
    fn recovery_actions_name_only_implemented_commands() {
        // A user who runs the suggested command must not hit "unrecognized
        // subcommand". This keeps the advice tracking the actual CLI surface,
        // so adopt/sync/undo cannot be advertised before they are built.
        const IMPLEMENTED: &[&str] = &[
            "init", "save", "status", "log", "verify", "checkout", "undo", "start", "switch",
            "line", "assign", "change",
        ];
        let cases: Vec<Error> = vec![
            Error::NotARepository(PathBuf::from("/tmp")),
            Error::NotFound("x".into()),
            Error::Corrupt("x".into()),
            Error::Invalid("x".into()),
            Error::Io(std::io::Error::other("x")),
            Error::NoSuchLine("x".into()),
            Error::InvalidLine("x".into()),
            Error::NoSuchChange("x".into()),
            Error::InvalidChange("x".into()),
            Error::ChangeAlreadyCheckpointed("x".into()),
            Error::ChangeHoldsNothing("x".into()),
            Error::Busy("x".into()),
            Error::UnsupportedFormat("x".into()),
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
