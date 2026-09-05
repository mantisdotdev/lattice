//! G1.3's in-process property harness.
//!
//! For each of N seeded sequences: initialise a fresh repository, save a seed,
//! snapshot the user-visible state, apply a random batch of state-changing
//! operations from the surface that exists today (save, undo, start, switch),
//! undo everything, and assert the user-visible state returns exactly to the
//! seed snapshot.
//!
//! The equality domain is the one the gate names — working-tree bytes, the
//! checkpoint graph, and the lines — explicitly EXCLUDING the op-log, which
//! grows on every operation including undo, and the preserved working state,
//! which is the ephemeral tier the gate omits.
//!
//! A file is written after a line is CREATED, never on the default line before
//! any line exists. That is deliberate: it makes the lines diverge, so `switch`
//! genuinely materialises and DELETES, which is the only way this harness can
//! catch a materialiser that forgets to remove what the target does not name.
//! A write not covered by a start/switch capture would legitimately survive
//! undo-all (undo reverses operations, not your edits) and is therefore never
//! made here.
//!
//! Prints, as its last line, `{"sequences", "failures", "emitted", "seed"}`,
//! which the G1.3 harness parses (harness/g1/g1_3_universal_undo.py).

use std::collections::BTreeMap;
use std::path::Path;

use ltx_core::{Checkpoint, ChunkId, LineState, Repo};

/// A deterministic xorshift so the run is reproducible from `--seed` alone,
/// with no external RNG dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Avoid the all-zero state, which xorshift cannot leave.
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// The user-visible state the gate compares: working-tree file hashes plus the
/// reachable checkpoint graph. The op-log is deliberately absent.
#[derive(PartialEq, Eq)]
struct Snapshot {
    tree: BTreeMap<Vec<u8>, [u8; 32]>,
    checkpoints: Vec<Checkpoint>,
    current_line: String,
    /// Line name -> tip. The preserved working state is deliberately absent:
    /// it is the ephemeral tier the gate's equality domain excludes.
    lines: BTreeMap<String, Option<String>>,
}

fn snapshot(root: &Path, repo: &Repo) -> Snapshot {
    let mut tree = BTreeMap::new();
    collect_tree(root, root, &mut tree);
    let checkpoints = repo
        .log_view(true, None)
        .expect("log_view must not fail on a healthy repo");
    let state: LineState = repo.lines().expect("lines must not fail on a healthy repo");
    let lines = state
        .lines
        .iter()
        .map(|(k, v)| (k.clone(), v.tip.clone()))
        .collect();
    Snapshot {
        tree,
        checkpoints,
        current_line: state.current,
        lines,
    }
}

fn collect_tree(root: &Path, dir: &Path, out: &mut BTreeMap<Vec<u8>, [u8; 32]>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.as_encoded_bytes() == b".lattice" {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            collect_tree(root, &path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .as_os_str()
                .as_encoded_bytes()
                .to_vec();
            out.insert(rel, *ChunkId::of(&bytes).as_bytes());
        }
    }
}

/// A tmpfs directory to build the ephemeral repos in, when one exists.
///
/// The property check needs correctness, not durability, so running the 100,000
/// throwaway repos on RAM-backed storage (Linux `/dev/shm`) turns every fsync
/// into a no-op and is the difference between minutes and hours. This lives in
/// the harness binary, not the engine, so it is not the engine detecting that it
/// is measured. On a machine without tmpfs it falls back to the default tempdir.
fn ephemeral_base() -> Option<std::path::PathBuf> {
    let shm = std::path::PathBuf::from("/dev/shm");
    shm.is_dir().then_some(shm)
}

/// Run one sequence; return true on a round-trip failure and record emissions.
fn run_sequence(base: Option<&Path>, rng: &mut Rng, emitted: &mut BTreeMap<String, u64>) -> bool {
    // Prefer the ephemeral (tmpfs) base, but fall back to the default tempdir
    // if it is present-but-unusable (e.g. a read-only /dev/shm), so the harness
    // still runs and emits its JSON rather than aborting.
    let dir = base
        .and_then(|b| tempfile::tempdir_in(b).ok())
        .or_else(|| tempfile::tempdir().ok())
        .expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("seed.txt"), b"seed\n").expect("seed write");

    let mut repo = Repo::init(root).expect("init");
    repo.save("seed", None).expect("seed save");
    let initial = snapshot(root, &repo);

    // Apply a random batch across the surface that exists today.
    let length = 1 + rng.below(12);
    let mut applied = 0u64;
    for i in 0..length {
        match rng.below(4) {
            0 => {
                *emitted.entry("undo".into()).or_default() += 1;
                // A `nothing_to_undo` here is still a legitimate emission.
                repo.undo().expect("undo");
            }
            1 => {
                *emitted.entry("start".into()).or_default() += 1;
                let name = format!("probe-{}", rng.below(3));
                let out = repo.start_line(&name).expect("start");
                if out.created {
                    // Make the new line diverge so a later switch has real work
                    // to do — including deleting this file again.
                    std::fs::write(root.join(format!("on-{name}-{i}.txt")), b"work\n")
                        .expect("line file");
                }
            }
            2 => {
                *emitted.entry("switch".into()).or_default() += 1;
                let state = repo.lines().expect("lines");
                let names: Vec<String> = state.lines.keys().cloned().collect();
                let pick = names[(rng.below(names.len() as u64)) as usize].clone();
                repo.switch_line(&pick).expect("switch");
            }
            _ => {
                *emitted.entry("save".into()).or_default() += 1;
                repo.save("probe", None).expect("save");
            }
        }
        applied += 1;
    }

    // Undo everything, bounded exactly as the gate bounds it.
    for _ in 0..(applied * 3 + 8) {
        if repo.undo().expect("undo-all").nothing_to_undo {
            break;
        }
    }

    let final_state = snapshot(root, &repo);
    final_state != initial
}

fn main() {
    let mut sequences: u64 = 1000;
    let mut seed: u64 = 0;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--sequences" => {
                sequences = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(sequences);
                i += 2;
            }
            "--seed" => {
                seed = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(seed);
                i += 2;
            }
            "--json" => i += 1,
            _ => i += 1,
        }
    }

    let mut rng = Rng::new(seed);
    let base = ephemeral_base();
    let mut emitted: BTreeMap<String, u64> = BTreeMap::new();
    let mut failures: u64 = 0;
    for _ in 0..sequences {
        if run_sequence(base.as_deref(), &mut rng, &mut emitted) {
            failures += 1;
        }
    }

    let out = serde_json::json!({
        "sequences": sequences,
        "failures": failures,
        "emitted": emitted,
        "seed": seed,
    });
    println!("{}", serde_json::to_string(&out).expect("serialize result"));
}
