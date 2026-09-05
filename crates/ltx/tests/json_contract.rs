//! The JSON shapes the frozen harnesses read.
//!
//! G1.1 and G1.3 parse `ltx --json` output directly, and `harness/FREEZE.json`
//! makes those parsers unamendable (§0.3). So the SHAPE is a contract owned by
//! the harness, not by this crate, and a change here is a gate failure rather
//! than a refactor.
//!
//! This file exists because that contract was broken in exactly the way no
//! unit test could see: `verify --json` nested its body under `"report"` while
//! `harness/g1/g1_1_crash_safety.py:124` reads `doc.get("complete")` from the
//! top level. Every engine test passed, the CLI looked right, and all 1,683 of
//! G1.1's crash trials failed identically — `complete` read as `None`, so no
//! trial could ever pass, and `errors` fell back to its `[]` default, which is
//! why the failures carried an empty reason.
//!
//! Each assertion below names the harness line it protects.

use std::path::Path;
use std::process::Command;

fn ltx(repo: &Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_ltx"))
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .output()
        .expect("ltx runs");
    // BOTH streams. Under `--json` the CLI puts its error document on STDOUT
    // and leaves stderr empty, so reporting stderr alone made every failure
    // here read as "failed: " with no reason — the same silence this file was
    // written to stop.
    assert!(
        out.status.success(),
        "`ltx {} --json` failed ({}).\n  stdout: {}\n  stderr: {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "`ltx {}` emitted unparseable JSON ({e}): {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn seeded_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("seed.txt"), b"seed\n").unwrap();
    ltx(dir.path(), &["init"]);
    ltx(dir.path(), &["save", "seed"]);
    dir
}

#[test]
fn verify_answers_complete_and_errors_at_the_top_level() {
    let dir = seeded_repo();
    let doc = ltx(dir.path(), &["verify", "--complete"]);

    // harness/g1/g1_1_crash_safety.py:124 —
    //     bool(doc.get("complete")) and not doc.get("errors")
    assert_eq!(
        doc.get("complete").and_then(|v| v.as_bool()),
        Some(true),
        "G1.1 reads `complete` from the top level; nesting it fails every \
         crash trial with no stated reason. Got: {doc}"
    );
    assert!(
        doc.get("errors").map(|v| v.is_array()).unwrap_or(false),
        "G1.1 reads `errors` from the top level, and its ABSENCE is \
         indistinguishable from an empty list. Got: {doc}"
    );

    // The harness's predicate, run exactly as it is written.
    let harness_ok = doc
        .get("complete")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && doc
            .get("errors")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
    assert!(
        harness_ok,
        "a healthy repository must satisfy G1.1's verify()"
    );
}

#[test]
fn the_default_verify_does_not_claim_completeness() {
    // Challenge 4: a caller must not be able to mistake the partial form for
    // the complete one without ignoring a field named `complete`. That only
    // works if the field is where the caller looks.
    let dir = seeded_repo();
    let doc = ltx(dir.path(), &["verify"]);
    assert_eq!(doc.get("complete").and_then(|v| v.as_bool()), Some(false));
}

#[test]
fn forensic_log_answers_checkpoints_at_the_top_level() {
    // harness/g1/g1_1_crash_safety.py:113 — doc.get("checkpoints", [])
    // This is how the crash gate counts what was lost, and the default `[]`
    // means a missing key reads as "no checkpoints existed" rather than as an
    // error — silently turning lost data into a clean result.
    let dir = seeded_repo();
    let doc = ltx(dir.path(), &["log", "--forensic"]);
    let checkpoints = doc
        .get("checkpoints")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("`checkpoints` must be a top-level array. Got: {doc}"));
    assert_eq!(checkpoints.len(), 1, "the seed save must be reported");
}

#[test]
fn line_and_change_views_answer_at_the_top_level() {
    // harness/g1/g1_3_universal_undo.py:141-143 puts `line list` and
    // `change list` in the equality domain undo-all is compared against. Both
    // must parse; a command that errors compares equal to itself erroring and
    // makes the domain vacuous.
    let dir = seeded_repo();
    let doc = ltx(dir.path(), &["line", "list"]);
    assert!(
        doc.get("lines").map(|v| v.is_array()).unwrap_or(false),
        "`lines` must be a top-level array. Got: {doc}"
    );

    let doc = ltx(dir.path(), &["change", "list"]);
    assert_eq!(
        doc.get("changes").and_then(|v| v.as_array()),
        Some(&Vec::new()),
        "a repository with no changes lists none, at the top level and \
         exiting 0 — until this command existed the domain compared one \
         error document to another and was vacuously equal. Got: {doc}"
    );
}

#[test]
fn the_change_view_carries_nothing_that_an_undone_batch_would_change() {
    // The document sits in an equality domain compared before a batch and
    // after undoing it, so a timestamp, a counter or an op-log position in it
    // would fail undo-all for a reason that has nothing to do with undo.
    let dir = seeded_repo();
    ltx(dir.path(), &["assign", "seed.txt"]);
    let doc = ltx(dir.path(), &["change", "list"]);

    let keys: Vec<&str> = doc
        .as_object()
        .expect("an object")
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(keys, vec!["changes", "ok", "version"], "got: {doc}");
    let row = doc["changes"][0].as_object().expect("a change row");
    let mut row_keys: Vec<&str> = row.keys().map(|k| k.as_str()).collect();
    row_keys.sort_unstable();
    assert_eq!(
        row_keys,
        vec!["assigned", "current", "id", "short"],
        "got: {doc}"
    );
}

#[test]
fn assign_takes_a_bare_path_and_reports_its_position() {
    // harness/g1/g1_4_concurrency.py:45 draws exactly `["assign", "."]` — a
    // bare path with no change named — and :77 reads `oplog_seq` from the
    // result. An operation with no position is counted as its own failure, so
    // omitting the field cannot be a way to pass.
    let dir = seeded_repo();
    let doc = ltx(dir.path(), &["assign", "."]);
    assert!(
        doc.get("oplog_seq").and_then(|v| v.as_u64()).is_some(),
        "`oplog_seq` must be a top-level number. Got: {doc}"
    );
    assert_eq!(
        doc.get("assigned").and_then(|v| v.as_array()).map(Vec::len),
        Some(1),
        "`.` is the working tree, and seed.txt is in it. Got: {doc}"
    );
}

#[test]
fn assign_exits_zero_when_it_refuses() {
    // harness/g1/g1_4_concurrency.py:66 counts ANY non-zero exit as a failure,
    // across ~10,000 draws against a target of 0. A path that cannot be taken
    // is reported in `refused` at exit 0 — refusing loudly, in JSON.
    let dir = seeded_repo();
    let out = Command::new(env!("CARGO_BIN_EXE_ltx"))
        .args(["assign", "not-here.txt", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("ltx runs");

    assert!(out.status.success(), "refusing is not failing");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(doc["ok"], serde_json::json!(true));
    assert_eq!(
        doc["refused"].as_array().map(Vec::len),
        Some(1),
        "and the refusal is reported as data, never silently. Got: {doc}"
    );
}
