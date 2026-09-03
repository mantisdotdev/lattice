//! What does one durable op-log append actually cost, and why?
//!
//! Every `ltx` command must append a record to the operation log and make it
//! durable before reporting success. That cost is a hard floor under G1.5
//! (status p95 < 100 ms) and G1.6 (save p95 < 250 ms), and the storebench
//! result (9.4 ms/append) was alarming enough to isolate.
//!
//! Three hypotheses:
//!   H1  Rust's `sync_data()` on macOS issues F_FULLFSYNC (a true media flush),
//!       while plain fsync(2) on macOS does not. If so the gap is Rust being
//!       correct, not slow, and the number is the real durability cost.
//!   H2  Extending the file on every append forces a metadata flush. If so,
//!       pre-allocating the log removes most of the cost.
//!   H3  Group commit amortises the flush across concurrent commands.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::time::Instant;

const N: usize = 200;
const REC: usize = 256;

fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[(((v.len() as f64) * p).ceil() as usize).saturating_sub(1).min(v.len() - 1)]
}

fn measure(name: &str, prealloc: bool, mode: &str, batch: usize) -> serde_json::Value {
    let dir = std::env::temp_dir().join(format!("fsyncbench-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("oplog.bin");
    let mut f = OpenOptions::new().create(true).read(true).write(true).open(&path).unwrap();
    if prealloc {
        // Reserve the whole log up front so no append extends the file.
        f.set_len((N * (REC + 4) + 4096) as u64).unwrap();
        f.sync_all().unwrap();
    }
    let fd = f.as_raw_fd();
    let rec = vec![0xABu8; REC];
    let mut lat = Vec::with_capacity(N);
    let mut pending = 0usize;

    for _ in 0..N {
        let t = Instant::now();
        f.write_all(&(REC as u32).to_le_bytes()).unwrap();
        f.write_all(&rec).unwrap();
        pending += 1;
        if pending >= batch {
            match mode {
                "fullfsync" => { f.sync_data().unwrap(); }
                "fsync" => unsafe { libc::fsync(fd); },
                "fdatasync_only" => unsafe {
                    #[cfg(target_os = "macos")] libc::fsync(fd);
                    #[cfg(not(target_os = "macos"))] libc::fdatasync(fd);
                },
                _ => {}
            }
            pending = 0;
        }
        lat.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let mean = lat.iter().sum::<f64>() / lat.len() as f64;
    let v = serde_json::json!({
        "variant": name, "prealloc": prealloc, "sync": mode, "batch": batch,
        "mean_us": (mean * 10.0).round() / 10.0,
        "p50_us": (pct(&mut lat, 0.50) * 10.0).round() / 10.0,
        "p95_us": (pct(&mut lat, 0.95) * 10.0).round() / 10.0,
        "p99_us": (pct(&mut lat, 0.99) * 10.0).round() / 10.0,
    });
    let _ = std::fs::remove_dir_all(&dir);
    v
}

fn main() {
    let rows = vec![
        measure("append+F_FULLFSYNC", false, "fullfsync", 1),
        measure("prealloc+F_FULLFSYNC", true, "fullfsync", 1),
        measure("append+fsync(2)", false, "fsync", 1),
        measure("prealloc+fsync(2)", true, "fsync", 1),
        measure("prealloc+F_FULLFSYNC+group8", true, "fullfsync", 8),
        measure("prealloc+F_FULLFSYNC+group32", true, "fullfsync", 32),
        measure("no-durability(control)", true, "none", 1),
    ];
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "records": N, "record_bytes": REC,
        "note": "Rust's sync_data() maps to F_FULLFSYNC on macOS (a true media \
                 flush); plain fsync(2) on macOS returns once the write reaches \
                 the drive cache and is NOT crash-safe against power loss.",
        "rows": rows
    })).unwrap());
}
