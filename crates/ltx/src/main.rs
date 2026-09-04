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
    /// Return to the previous checkpoint.
    Undo,
    /// Begin a new line here, and switch to it.
    Start {
        /// What to call the line.
        name: String,
    },
    /// Continue on another line, preserving this one's working state.
    Switch {
        /// The line to continue on.
        name: String,
    },
    /// Work with lines.
    #[command(subcommand)]
    Line(LineCmd),
    /// Plumbing. Never required on a normal path.
    #[command(subcommand)]
    Internals(Internals),
}

#[derive(Subcommand)]
enum LineCmd {
    /// Show every line, and which one is current.
    List,
}

#[derive(Subcommand)]
enum Internals {
    /// The machine-readable command surface, for tooling and conformance tests.
    CommandSurface,
    /// The raw append-only operation log (the audit record).
    Oplog,
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
            // History is the checkpoint graph reachable from the current head,
            // which undo moves — so this view is invariant under undo-all
            // (ADR-15). The raw op-log is not history; it lives at
            // `ltx internals oplog`. The default-and-limit policy is in
            // ltx-core (§8); the CLI only renders, and `--limit` applies to
            // both renderings so they never disagree.
            let checkpoints = repo.log_view(*forensic, *limit)?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true,
                        "forensic": forensic,
                        "checkpoints": checkpoints,
                    })
                },
                || {
                    let mut out = String::new();
                    for c in &checkpoints {
                        out.push_str(&format!("{}  {}\n", ltx_core::short_id(&c.id), c.message));
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
            // The default-to-current policy lives in ltx-core (§8); the CLI only
            // passes the optional argument through and renders the result.
            let report = repo.checkout_into(checkpoint.as_deref(), into)?;
            let id = report.checkpoint.clone();
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

        Command::Undo => {
            let mut repo = Repo::discover(&cwd)?;
            let outcome = repo.undo()?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true,
                        // G1.3 reads this exact key to know when to stop.
                        "nothing_to_undo": outcome.nothing_to_undo,
                        "undone_checkpoint": outcome.undone_checkpoint,
                        "now_at": outcome.now_at,
                        "undo_seq": outcome.undo_seq,
                        "oplog_seq": outcome.undo_seq,
                        "preserved_tree": outcome.preserved_tree,
                        "rescued_tree": outcome.rescued_tree,
                        // Challenge 8 / §4.3: undo names any remote residue it
                        // could not reverse. Empty for a purely local undo.
                        "remote_effects_not_undone": outcome.remote_effects_not_undone,
                    })
                },
                || {
                    if outcome.nothing_to_undo {
                        return "nothing to undo".to_string();
                    }
                    // Reversing a start or a switch undoes no checkpoint, so
                    // keying the message on `undone_checkpoint` alone reported
                    // "nothing to undo" for work that had in fact been undone.
                    let mut out = match (&outcome.undone_checkpoint, &outcome.now_at) {
                        (Some(undone), Some(now)) => format!(
                            "undid {}; now at {}",
                            ltx_core::short_id(undone),
                            ltx_core::short_id(now)
                        ),
                        (Some(undone), None) => {
                            format!("undid {}", ltx_core::short_id(undone))
                        }
                        (None, Some(now)) => {
                            format!("undone; now at {}", ltx_core::short_id(now))
                        }
                        (None, None) => "undone".to_string(),
                    };
                    if let Some(tree) = &outcome.preserved_tree {
                        out.push_str(&format!(
                            "\n  the working state from that line is kept as {}",
                            ltx_core::short_id(tree)
                        ));
                    }
                    out
                },
            );
            Ok(EXIT_OK)
        }

        Command::Start { name } => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.start_line(name)?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "line": out.line, "created": out.created,
                        "now_at": out.now_at, "rescued_tree": out.rescued_tree,
                        // Every state-changing command reports its position so
                        // a concurrent history can be checked for linearizability.
                        "oplog_seq": out.oplog_seq,
                    })
                },
                || {
                    if out.created {
                        format!("started line {} — you are on it now", out.line)
                    } else {
                        format!("line {} already exists — you are on it now", out.line)
                    }
                },
            );
            Ok(EXIT_OK)
        }

        Command::Switch { name } => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.switch_line(name)?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "line": out.line, "now_at": out.now_at,
                        "oplog_seq": out.oplog_seq, "rescued_tree": out.rescued_tree,
                    })
                },
                || format!("now on line {}", out.line),
            );
            Ok(EXIT_OK)
        }

        Command::Line(LineCmd::List) => {
            let repo = Repo::discover(&cwd)?;
            let state = repo.lines()?;
            // Deliberately excludes preserved working state (the ephemeral tier
            // the undo equality domain omits) and any timestamp or count, so
            // this document is invariant under undo-all (ADR-16 §5).
            let rows: Vec<_> = state
                .lines
                .iter()
                .map(|(name, rec)| serde_json::json!({ "name": name, "checkpoint": rec.tip }))
                .collect();
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "version": 1,
                        "current": state.current, "lines": rows,
                    })
                },
                || {
                    let mut out = String::new();
                    for name in state.lines.keys() {
                        let mark = if *name == state.current { "*" } else { " " };
                        out.push_str(&format!("{mark} {name}\n"));
                    }
                    out.trim_end().to_string()
                },
            );
            Ok(EXIT_OK)
        }

        Command::Internals(Internals::CommandSurface) => {
            // G1.3's coverage contract requires the state-changing surface to
            // be DISCOVERABLE rather than hand-listed in the harness, so the
            // product publishes it. `init` is state_changing:false — it
            // establishes the container, not undoable user-visible state, and
            // sits below the undo floor (ADR-15).
            let surface = serde_json::json!({
                "ok": true,
                "version": 1,
                "concepts": ltx_core::CONCEPTS,
                "commands": [
                    { "name": "init", "state_changing": false, "undoable": false, "sample_args": [] },
                    { "name": "save", "state_changing": true, "undoable": true, "sample_args": ["probe"] },
                    // undo is monotonic toward the root; it is not itself
                    // undoable (redo is a separate deferred move — ADR-15).
                    { "name": "undo", "state_changing": true, "undoable": false, "sample_args": [] },
                    // start probe-line always succeeds and leaves the batch ON
                    // probe-line, so a following `switch main` is a REAL switch
                    // that exercises capture and materialisation rather than a
                    // self-switch that counts coverage without testing anything.
                    { "name": "start", "state_changing": true, "undoable": true, "sample_args": ["probe-line"] },
                    { "name": "switch", "state_changing": true, "undoable": true, "sample_args": ["main"] },
                    { "name": "line list", "state_changing": false, "undoable": false, "sample_args": [] },
                    { "name": "status", "state_changing": false, "undoable": false, "sample_args": [] },
                    { "name": "log", "state_changing": false, "undoable": false, "sample_args": [] },
                    { "name": "verify", "state_changing": false, "undoable": false, "sample_args": [] },
                    { "name": "checkout", "state_changing": false, "undoable": false, "sample_args": [] },
                ],
            });
            println!("{}", serde_json::to_string(&surface)?);
            Ok(EXIT_OK)
        }

        Command::Internals(Internals::Oplog) => {
            // The raw append-only audit record. Not history (that is `log`); it
            // grows on every operation, including undo, so it is out of the
            // equality domain G1.3 compares.
            let repo = Repo::discover(&cwd)?;
            let entries = repo.log()?;
            emit(
                cli,
                || serde_json::json!({ "ok": true, "operations": entries }),
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
