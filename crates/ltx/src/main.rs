//! The `ltx` command-line shell.
//!
//! §8: "the CLI contains no logic the API lacks." Every subcommand here is a
//! translation of argv into one `ltx_core` call and a rendering of the result.
//! G5.5 measures that as a HARD gate, and it only stays true if logic is never
//! written here in the first place.
//!
//! Two surface rules, both machine-checked later:
//!
//! **`--json` everywhere** (§4.3, G2.5). Every command accepts it and emits a
//! stable, versioned object. Exit codes are a contract.
//!
//! **Every error names a way back** (§4.3, G2.4). The recovery text comes from
//! the error type itself, so a new error variant cannot ship without one.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ltx_core::{Repo, Result};

/// Exit codes are part of the contract, not incidental.
const EXIT_OK: u8 = 0;
const EXIT_ERROR: u8 = 1;
/// The request was well-formed but the repository was not where one was needed.
const EXIT_NO_REPOSITORY: u8 = 3;

#[derive(Parser)]
#[command(
    name = "ltx",
    version,
    about = "Lattice — version control for human and agent authorship",
    disable_help_subcommand = true
)]
struct Cli {
    /// Emit a stable JSON object instead of prose.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a repository here.
    Init,
    /// Save the working state as a checkpoint.
    Save {
        /// What this checkpoint is for.
        message: String,
    },
    /// Show what is here and what has happened.
    Status,
    /// Show history.
    Log {
        /// Show every operation, including anything a lens would hide. Until
        /// lenses exist there is nothing hidden, so this is the full history —
        /// the forensic view is simply already complete. Consumed by the G1.1
        /// and G1.3 harnesses.
        #[arg(long)]
        forensic: bool,
        /// Most recent N entries.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Check the repository against its own hashes.
    Verify {
        /// Fetch anything missing and verify everything. Only this form may be
        /// read as an unqualified "verified".
        #[arg(long)]
        complete: bool,
    },
    /// Write a checkpoint's contents into a directory.
    Checkout {
        /// Checkpoint address. Defaults to the current one.
        #[arg(long)]
        checkpoint: Option<String>,
        /// Where to write it.
        #[arg(long)]
        into: PathBuf,
    },
    /// Plumbing. Never required on a normal path.
    #[command(subcommand)]
    Internals(Internals),
}

#[derive(Subcommand)]
enum Internals {
    /// The machine-readable command surface, for tooling and conformance tests.
    CommandSurface,
    /// Chunk store statistics.
    Store,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let code = match e.category() {
                ltx_core::Category::NotARepository => EXIT_NO_REPOSITORY,
                _ => EXIT_ERROR,
            };
            if cli.json {
                let payload = serde_json::json!({
                    "ok": false,
                    "error": e.to_string(),
                    // G2.4 requires all three on every error path. They are
                    // fields rather than prose so the gate can check them.
                    "category": e.category(),
                    "concept": e.concept(),
                    "recovery": e.recovery(),
                });
                println!("{}", serde_json::to_string(&payload).unwrap_or_default());
            } else {
                eprintln!("error: {e}");
                eprintln!("  try: {}", e.recovery());
            }
            ExitCode::from(code)
        }
    }
}

fn run(cli: &Cli) -> Result<u8> {
    let cwd = std::env::current_dir()?;

    match &cli.command {
        Command::Init => {
            let repo = Repo::init(&cwd)?;
            emit(
                cli,
                || serde_json::json!({ "ok": true, "root": repo.root().display().to_string() }),
                || format!("started a repository in {}", repo.root().display()),
            );
            Ok(EXIT_OK)
        }

        Command::Save { message } => {
            let mut repo = Repo::discover(&cwd)?;
            let cp = repo.save(message)?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true,
                        "checkpoint": cp.id,
                        "tree": cp.tree,
                        "message": cp.message,
                        "parent": cp.parent,
                        "oplog_seq": cp.oplog_seq,
                    })
                },
                || format!("saved {} — {}", ltx_core::short_id(&cp.id), cp.message),
            );
            Ok(EXIT_OK)
        }

        Command::Status => {
            let repo = Repo::discover(&cwd)?;
            let s = repo.status()?;
            emit(
                cli,
                || serde_json::json!({ "ok": true, "status": s }),
                || match (&s.head, &s.head_message) {
                    (Some(h), Some(m)) => format!(
                        "{} — {} checkpoints, {} operations\ncurrent: {} — {}",
                        s.root,
                        s.checkpoints,
                        s.operations,
                        ltx_core::short_id(h),
                        m
                    ),
                    _ => format!("{} — nothing saved yet", s.root),
                },
            );
            Ok(EXIT_OK)
        }

        Command::Log { forensic, limit } => {
            let repo = Repo::discover(&cwd)?;
            let mut entries = repo.log()?;
            entries.reverse();
            if let Some(n) = limit {
                entries.truncate(*n);
            }
            let checkpoints = repo.checkpoints()?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true,
                        "forensic": forensic,
                        "operations": entries,
                        "checkpoints": checkpoints,
                    })
                },
                || {
                    let mut out = String::new();
                    for e in &entries {
                        out.push_str(&format!(
                            "{:>5}  {:<8} {}\n",
                            e.seq,
                            e.operation.name(),
                            ltx_core::short_id(&e.id)
                        ));
                    }
                    out.trim_end().to_string()
                },
            );
            Ok(EXIT_OK)
        }

        Command::Verify { complete } => {
            let repo = Repo::discover(&cwd)?;
            let report = repo.verify(*complete)?;
            let healthy = report.structure_verified && report.errors.is_empty();
            emit(
                cli,
                || serde_json::json!({ "ok": healthy, "report": report }),
                || {
                    // An unhealthy report must never render as a success
                    // sentence: the words the user reads have to match the
                    // exit code. The problems are listed rather than hidden.
                    if !healthy {
                        let mut out =
                            format!("NOT verified — {} problem(s) found:", report.errors.len());
                        for e in &report.errors {
                            out.push_str(&format!("\n  - {e}"));
                        }
                        return out;
                    }
                    if *complete {
                        format!(
                            "verified {} checkpoints and {} chunks; {} operations chained",
                            report.checkpoints, report.chunks_verified, report.oplog_entries
                        )
                    } else {
                        // Challenge 4: the default form reports coverage rather
                        // than claiming completeness.
                        format!(
                            "verified history structure; content verified for {} chunks; \
                         {} not present locally\nrun `ltx verify --complete` for the full check",
                            report.chunks_verified, report.chunks_absent
                        )
                    }
                },
            );
            Ok(if healthy { EXIT_OK } else { EXIT_ERROR })
        }

        Command::Checkout { checkpoint, into } => {
            let repo = Repo::discover(&cwd)?;
            let id = match checkpoint {
                Some(id) => id.clone(),
                None => match repo.head_checkpoint()? {
                    Some(cp) => cp.id,
                    None => {
                        return Err(ltx_core::Error::NotFound(
                            "nothing has been saved yet, so there is nothing to write out".into(),
                        ))
                    }
                },
            };
            let report = repo.checkout(&id, into)?;
            let n = report.entries_written;
            let collisions = report.collisions.clone();
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "checkpoint": id, "into": into.display().to_string(),
                        "entries": n,
                        // Names this filesystem could not hold. Reported as data, so a
                        // caller cannot miss them by not reading prose.
                        "collisions": collisions,
                    })
                },
                || {
                    let mut out = format!("wrote {n} entries into {}", into.display());
                    for c in &report.collisions {
                        out.push_str(&format!(
                            "\n  not written: {} — this filesystem does not distinguish \
                         it from {}",
                            c.path, c.collided_with
                        ));
                    }
                    out
                },
            );
            Ok(EXIT_OK)
        }

        Command::Internals(Internals::CommandSurface) => {
            // G1.3's coverage contract requires the state-changing surface to
            // be DISCOVERABLE rather than hand-listed in the harness, so the
            // product publishes it.
            let surface = serde_json::json!({
                "ok": true,
                "version": 1,
                "concepts": ltx_core::CONCEPTS,
                "commands": [
                    { "name": "init", "state_changing": true, "undoable": true, "sample_args": [] },
                    { "name": "save", "state_changing": true, "undoable": true, "sample_args": ["probe"] },
                    { "name": "status", "state_changing": false, "undoable": false, "sample_args": [] },
                    { "name": "log", "state_changing": false, "undoable": false, "sample_args": [] },
                    { "name": "verify", "state_changing": false, "undoable": false, "sample_args": [] },
                    { "name": "checkout", "state_changing": false, "undoable": false, "sample_args": [] },
                ],
            });
            println!("{}", serde_json::to_string(&surface)?);
            Ok(EXIT_OK)
        }

        Command::Internals(Internals::Store) => {
            let repo = Repo::discover(&cwd)?;
            let s = repo.status()?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "chunks": s.chunks, "packs": s.packs,
                    })
                },
                || format!("{} chunks in {} packs", s.chunks, s.packs),
            );
            Ok(EXIT_OK)
        }
    }
}

/// Render one result in whichever form the caller asked for.
fn emit<J, T>(cli: &Cli, json: J, text: T)
where
    J: FnOnce() -> serde_json::Value,
    T: FnOnce() -> String,
{
    if cli.json {
        println!("{}", serde_json::to_string(&json()).unwrap_or_default());
    } else {
        println!("{}", text());
    }
}
