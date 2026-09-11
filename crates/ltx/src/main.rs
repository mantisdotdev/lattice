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
        /// Checkpoint only the paths assigned to this change, and consume it.
        /// Without it, the whole working state is saved — plain `save` never
        /// becomes implicitly partial.
        #[arg(long = "change", value_name = "CHANGE")]
        change: Option<String>,
    },
    /// Show what is here and what has happened.
    Status,
    /// Show history.
    Log {
        /// Show every line's history, including anything a lens would hide.
        /// Until lenses exist there is nothing hidden, so this differs from the
        /// default view only by covering lines other than the current one.
        // A `///` here is what `ltx log --help` prints, so it says what the
        // flag does for the person reading it. The measurement note belongs in
        // a comment: G1.1 and G1.3 both consume this view, and naming their
        // harnesses in help text puts the project's own scaffolding in front of
        // a user who has no idea what a gate is.
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
    /// Put working-tree paths into a change.
    Assign {
        /// The change to add to. It must already be open: this never creates
        /// one, so a mistyped id cannot mint a change. Without it, paths go
        /// to the current change, and a line with none starts one.
        #[arg(long = "to", value_name = "CHANGE")]
        to: Option<String>,
        /// What to assign. A directory assigns everything under it.
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
    },
    /// Work with changes.
    #[command(subcommand)]
    Change(ChangeCmd),
    /// Work with workspaces.
    #[command(subcommand)]
    Workspace(WorkspaceCmd),
    /// Work with lines.
    #[command(subcommand)]
    Line(LineCmd),
    /// Destroy a file's content everywhere history holds it. Cannot be undone.
    Redact {
        /// The file whose content must go.
        path: PathBuf,
        /// Name the irreversibility: without this, redact reports what it
        /// would destroy and does nothing.
        #[arg(long)]
        confirm_destroy: bool,
    },
    /// Bring another line's history onto this one.
    Merge {
        /// The line to take history from.
        line: String,
    },
    /// Split the current change so each top-level path is a change of its own.
    Split,
    /// Look through a lens, or see which exist.
    #[command(subcommand)]
    Lens(LensCmd),
    /// Exchange history with a remote.
    Sync {
        /// Report what would be sent and received, and move nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Collect content nothing references.
    Thin,
    /// Plumbing. Never required on a normal path.
    #[command(subcommand)]
    Internals(Internals),
}

#[derive(Subcommand)]
enum LensCmd {
    /// Look through a lens.
    Use {
        /// The lens by name; `ltx lens list` shows them.
        name: String,
    },
    /// Show every lens, and which one this workspace looks through.
    List,
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Start another workspace, with its own working state.
    New {
        /// Where to put it. A new or empty directory.
        path: PathBuf,
    },
    /// Show every workspace over this repository.
    List,
}

#[derive(Subcommand)]
enum ChangeCmd {
    /// Show every change open on this line.
    List,
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
    /// Archive the operation-log entries written since the last archive.
    Compact,
    /// The same collection `ltx thin` performs. Here as well because §4.2
    /// puts maintenance behind `internals`, and the user-facing verb exists
    /// because the undo contract names `thin` at the top level; one
    /// operation, reachable by both spellings.
    Thin,
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

        Command::Save { message, change } => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.save(message, change.as_deref())?;
            let cp = &out.checkpoint;
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
                        // The change this consumed, if it was a partial save,
                        // and the address of the WHOLE working tree — which
                        // for a partial save is the only durable name the
                        // unsaved remainder has.
                        "change": out.change,
                        "working_state": out.working_state,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || match &out.change {
                    Some(id) => format!(
                        "saved change {} as {} — {}",
                        ltx_core::short_id(id),
                        ltx_core::short_id(&cp.id),
                        cp.message
                    ),
                    None => format!("saved {} — {}", ltx_core::short_id(&cp.id), cp.message),
                },
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
                    (Some(h), Some(m)) => {
                        let mut out = format!(
                            "{} — {} checkpoints, {} operations\ncurrent: {} — {}",
                            s.root,
                            s.checkpoints,
                            s.operations,
                            ltx_core::short_id(h),
                            m
                        );
                        // A head written by a partial save does not hold the
                        // whole working state, and this is where a user looks
                        // before `switch` or `undo` replaces their bytes.
                        if let Some(change) = &s.head_change {
                            out.push_str(&format!(
                                "\n  this checkpoint holds change {} only; the rest of \
                                 your working state is not in it",
                                ltx_core::short_id(change)
                            ));
                        }
                        out
                    }
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
                // FLAT, not nested under "report". G1.1's frozen harness reads
                // `complete` and `errors` from the top level of this document
                // (harness/g1/g1_1_crash_safety.py:124), and `log --forensic`
                // already answers at the top level with `checkpoints`. Nesting
                // made every one of G1.1's 1,683 trials fail identically:
                // `doc.get("complete")` was None, so no crash trial could ever
                // pass, and `doc.get("errors", [])` returned the default `[]`,
                // which is why the failures carried an empty reason.
                || {
                    let mut doc =
                        serde_json::to_value(&report).unwrap_or_else(|_| serde_json::json!({}));
                    if let Some(map) = doc.as_object_mut() {
                        map.insert("ok".into(), serde_json::json!(healthy));
                    }
                    doc
                },
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
                    // Not damage: a partial checkpoint is exactly what was
                    // asked for. It is said out loud because "verified" reads
                    // as "everything I have is safe in history", which after a
                    // partial save is not what the word can mean.
                    let partial = if report.checkpoints_partial > 0 {
                        format!(
                            "\n{} checkpoint(s) hold only part of the working state that \
                             stood when they were written; `ltx status` says whether the \
                             current one does",
                            report.checkpoints_partial
                        )
                    } else {
                        String::new()
                    };
                    if *complete {
                        format!(
                            "verified {} checkpoints and {} chunks; {} operations chained{partial}",
                            report.checkpoints, report.chunks_verified, report.oplog_entries
                        )
                    } else {
                        // Challenge 4: the default form reports coverage rather
                        // than claiming completeness.
                        format!(
                            "verified history structure; content verified for {} chunks; \
                         {} not present locally{partial}\nrun `ltx verify --complete` for \
                         the full check",
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
                        "preserved_working_state": outcome.preserved_working_state,
                        "rescued_working_state": outcome.rescued_working_state,
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
                    if let Some(tree) = &outcome.preserved_working_state {
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
                        "now_at": out.now_at, "rescued_working_state": out.rescued_working_state,
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
                        "oplog_seq": out.oplog_seq, "rescued_working_state": out.rescued_working_state,
                    })
                },
                || format!("now on line {}", out.line),
            );
            Ok(EXIT_OK)
        }

        Command::Assign { to, paths } => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.assign(paths, to.as_deref())?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        // A refusal is reported, not raised: this exits 0 with
                        // `refused` populated. G1.4 counts a non-zero exit as a
                        // failure across ~10,000 draws of `assign .`, and a
                        // path that cannot be taken is not a failed command.
                        "ok": true,
                        "change": out.change, "short": out.short,
                        "created": out.created, "line": out.line,
                        "assigned": out.assigned, "refused": out.refused,
                        "oplog_seq": out.oplog_seq,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || {
                    let mut text = if out.created {
                        format!(
                            "started change {} with {} path(s)",
                            out.short,
                            out.assigned.len()
                        )
                    } else {
                        format!(
                            "assigned {} path(s) to change {}",
                            out.assigned.len(),
                            out.short
                        )
                    };
                    for r in &out.refused {
                        text.push_str(&format!("\n  not assigned: {} — {}", r.path, r.reason));
                    }
                    text
                },
            );
            Ok(EXIT_OK)
        }

        Command::Workspace(WorkspaceCmd::New { path }) => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.new_workspace(path)?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "workspace": out.id, "root": out.root,
                        "entries": out.entries_written,
                        "oplog_seq": out.oplog_seq,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || {
                    format!(
                        "new workspace at {} — {} paths of working state",
                        out.root, out.entries_written
                    )
                },
            );
            Ok(EXIT_OK)
        }

        Command::Workspace(WorkspaceCmd::List) => {
            let repo = Repo::discover(&cwd)?;
            let spaces = repo.workspaces()?;
            emit(
                cli,
                // No timestamp and no counter, so this document is invariant
                // under apply-a-batch-then-undo-all, as `line list` and
                // `change list` are.
                || serde_json::json!({ "ok": true, "version": 1, "workspaces": spaces }),
                || {
                    if spaces.is_empty() {
                        return "no workspaces".to_string();
                    }
                    let mut out = String::new();
                    for w in &spaces {
                        out.push_str(&format!(
                            "{}  {}{}\n",
                            w.short,
                            w.root,
                            if w.present { "" } else { "  (missing)" }
                        ));
                    }
                    out.trim_end().to_string()
                },
            );
            Ok(EXIT_OK)
        }

        Command::Change(ChangeCmd::List) => {
            let repo = Repo::discover(&cwd)?;
            let changes = repo.changes()?;
            emit(
                cli,
                // No timestamp, no counter, no op-log position — so this
                // document is invariant under apply-a-batch-then-undo-all,
                // which is what G1.3 compares it for.
                || serde_json::json!({ "ok": true, "version": 1, "changes": changes }),
                || {
                    if changes.is_empty() {
                        return "no changes open".to_string();
                    }
                    let mut out = String::new();
                    for c in &changes {
                        let mark = if c.current { "*" } else { " " };
                        out.push_str(&format!(
                            "{mark} {}  {} path(s)\n",
                            c.short,
                            c.assigned.len()
                        ));
                    }
                    out.trim_end().to_string()
                },
            );
            Ok(EXIT_OK)
        }

        Command::Line(LineCmd::List) => {
            let repo = Repo::discover(&cwd)?;
            let state = repo.lines()?;
            // Workspace-relative: two workspaces legitimately disagree about
            // which line is current, and that is the feature (ADR-7).
            let current = repo.current_line()?;
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
                        "current": current, "lines": rows,
                    })
                },
                || {
                    let mut out = String::new();
                    for name in state.lines.keys() {
                        let mark = if *name == current { "*" } else { " " };
                        out.push_str(&format!("{mark} {name}\n"));
                    }
                    out.trim_end().to_string()
                },
            );
            Ok(EXIT_OK)
        }

        Command::Redact {
            path,
            confirm_destroy,
        } => {
            let mut repo = Repo::discover(&cwd)?;
            // Who did it is part of the record. The environment is read here,
            // at the edge, and handed in.
            let redactor = std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_else(|_| "unknown".to_string());
            let out = repo.redact(path, &redactor, *confirm_destroy)?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "target": out.target,
                        "chunks_destroyed": out.chunks_destroyed,
                        "places_in_history": out.places_in_history,
                        "oplog_seq": out.oplog_seq,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || {
                    format!(
                        "destroyed the content of {} in {} place(s) in history; \
                         undo will not restore it",
                        out.target, out.places_in_history
                    )
                },
            );
            Ok(EXIT_OK)
        }

        Command::Merge { line } => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.merge_line(line)?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "line": out.line, "from": out.from,
                        "fast_forward": out.fast_forward, "now_at": out.now_at,
                        "oplog_seq": out.oplog_seq,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || {
                    if out.fast_forward {
                        format!("{} now holds everything on {}", out.line, out.from)
                    } else {
                        format!("{} already holds everything on {}", out.line, out.from)
                    }
                },
            );
            Ok(EXIT_OK)
        }

        Command::Split => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.split()?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "change": out.change, "into": out.into,
                        "moved": out.moved, "oplog_seq": out.oplog_seq,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || match (&out.change, out.into.len()) {
                    (None, _) => "nothing is current, so nothing to split".to_string(),
                    (Some(_), 0) => {
                        "the current change holds one group; nothing to split".to_string()
                    }
                    (Some(_), n) => format!("split {} path(s) into {} new change(s)", out.moved, n),
                },
            );
            Ok(EXIT_OK)
        }

        Command::Lens(LensCmd::Use { name }) => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.use_lens(name)?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "lens": out.lens, "oplog_seq": out.oplog_seq,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || format!("looking through lens {}", out.lens),
            );
            Ok(EXIT_OK)
        }

        Command::Lens(LensCmd::List) => {
            let repo = Repo::discover(&cwd)?;
            let lenses = repo.lenses()?;
            emit(
                cli,
                || serde_json::json!({ "ok": true, "version": 1, "lenses": lenses }),
                || {
                    lenses
                        .iter()
                        .map(|l| {
                            format!(
                                "{} {}  hides {}",
                                if l.active { "*" } else { " " },
                                l.name,
                                l.hides
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                },
            );
            Ok(EXIT_OK)
        }

        Command::Sync { dry_run } => {
            let mut repo = Repo::discover(&cwd)?;
            let out = if *dry_run {
                repo.sync_dry_run()?
            } else {
                repo.sync()?
            };
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "dry_run": out.dry_run, "remote": out.remote,
                        "would_send": out.would_send, "would_receive": out.would_receive,
                        "oplog_seq": out.oplog_seq,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || {
                    "dry run: no remote is configured; nothing to send, nothing to receive"
                        .to_string()
                },
            );
            Ok(EXIT_OK)
        }

        Command::Thin | Command::Internals(Internals::Thin) => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.thin()?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "collected": out.collected, "packs_removed": out.packs_removed,
                        "oplog_seq": out.oplog_seq,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || {
                    format!(
                        "collected {} unreferenced chunk(s) from {} pack(s)",
                        out.collected, out.packs_removed
                    )
                },
            );
            Ok(EXIT_OK)
        }

        Command::Internals(Internals::Compact) => {
            let mut repo = Repo::discover(&cwd)?;
            let out = repo.compact()?;
            emit(
                cli,
                || {
                    serde_json::json!({
                        "ok": true, "from_seq": out.from_seq, "to_seq": out.to_seq,
                        "archived": out.archived, "oplog_seq": out.oplog_seq,
                        "rescued_working_state": out.rescued_working_state,
                    })
                },
                || {
                    format!(
                        "archived {} operation(s), {}..{}",
                        out.archived, out.from_seq, out.to_seq
                    )
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
                    // `seed.txt` is the one path G1.3's batches guarantee
                    // exists. The emission counter increments before the
                    // return code is checked, so args naming a path that does
                    // not exist would satisfy the coverage bar while testing
                    // nothing (ADR-17 §6).
                    { "name": "assign", "state_changing": true, "undoable": true, "sample_args": ["seed.txt"] },
                    { "name": "change list", "state_changing": false, "undoable": false, "sample_args": [] },
                    // Undoable:false is a decision, not an omission — ADR-7 §4
                    // makes undo repository-scoped, so undoing this could
                    // remove the workspace another person is working in.
                    // sample_args is absent because the path must not exist
                    // yet, and a fixed one would fail on its second draw.
                    // `undoable: false` is a decision, not an omission — ADR-7
                    // §4 makes undo repository-scoped, so undoing this could
                    // remove the workspace another person is working in. The
                    // sample path is a sibling of the repository, matching how
                    // a workspace is actually made; drawn twice in one sequence
                    // the second refuses, which is the behaviour under test.
                    { "name": "workspace new", "state_changing": true, "undoable": false, "sample_args": ["../probe-workspace"] },
                    { "name": "workspace list", "state_changing": false, "undoable": false, "sample_args": [] },
                    // Bare: splits whatever change is current, and succeeds
                    // with nothing to split — the same rule as a refused
                    // assign, so a batch that draws it before any assign
                    // still counts a command that ran.
                    { "name": "split", "state_changing": true, "undoable": true, "sample_args": [] },
                    // `main` always exists. From probe-line it is already
                    // contained and the attempt is recorded; after `switch
                    // main` it is a REAL fast-forward onto probe-line's
                    // saves, whose undo rewrites the working tree back.
                    { "name": "merge", "state_changing": true, "undoable": true, "sample_args": ["main"] },
                    // Recorded and not undoable (Challenge 12, ADR-20): the
                    // content is destroyed, and the gate excludes what a
                    // redaction destroyed from what undo must bring back.
                    // `seed.txt` is the path every batch guarantees, so the
                    // destruction is real on every draw.
                    { "name": "redact", "state_changing": true, "undoable": false, "sample_args": ["seed.txt", "--confirm-destroy"] },
                    { "name": "lens use", "state_changing": true, "undoable": true, "sample_args": ["clean"] },
                    { "name": "lens list", "state_changing": false, "undoable": false, "sample_args": [] },
                    { "name": "sync", "state_changing": true, "undoable": true, "sample_args": ["--dry-run"] },
                    // Recorded and not undoable (Challenge 12): collected
                    // content is gone. It changes nothing the equality domain
                    // holds, because nothing referenced is ever collected.
                    { "name": "thin", "state_changing": true, "undoable": false, "sample_args": [] },
                    { "name": "internals compact", "state_changing": true, "undoable": false, "sample_args": [] },
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
