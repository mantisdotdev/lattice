//! The `ltx-daemon` API shell.
//!
//! ADR-4: the daemon is an accelerator, never a requirement. No command may be
//! unavailable without it, and every timing gate reports both the resident and
//! the daemonless figure so a catastrophic degradation is visible rather than
//! hidden.
//!
//! Not yet implemented. It is a binary target so the workspace shape is fixed
//! from the start — G5.5 requires API/CLI parity, and adding the crate later
//! invites the CLI growing logic the API lacks in the meantime.

use std::process::ExitCode;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "ltx-daemon",
    version,
    about = "Lattice daemon (accelerator; never required)"
)]
struct Cli {
    /// Repository to watch.
    #[arg(long)]
    repo: Option<std::path::PathBuf>,
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let payload = serde_json::json!({
        "ok": false,
        "error": "the daemon is not implemented yet",
        "category": "not-found",
        "concept": "workspace",
        // ADR-4's commitment, stated by the binary itself: nothing depends on
        // this process existing.
        "recovery": "every command works without the daemon; run `ltx status` \
                     normally. The daemon only makes them faster.",
    });
    if cli.json {
        println!("{}", serde_json::to_string(&payload).unwrap_or_default());
    } else {
        eprintln!("error: the daemon is not implemented yet");
        eprintln!("  try: every command works without it; the daemon only makes them faster");
    }
    ExitCode::from(1u8)
}
