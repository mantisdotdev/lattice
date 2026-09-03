//! ADR-3 prototype: which store backend?
//!
//! Lattice's store has three access patterns and they pull in different
//! directions, which is exactly why this needs measuring rather than taste:
//!
//!   A. CHUNK WRITE — bulk insertion of many immutable content-addressed blobs
//!      during `ltx save`. Throughput matters; per-item durability does not,
//!      because a chunk is not visible until the checkpoint referencing it is.
//!   B. CHUNK READ  — random point lookups by 32-byte key during checkout and
//!      diff. Latency matters, and the working set does not fit in RAM.
//!   C. OP-LOG APPEND — one small durable record per command, fsynced before
//!      the command reports success. This is on the critical path of EVERY
//!      `ltx` invocation, so its latency is a floor under G1.5/G1.6.
//!
//! Candidates: redb (embedded, proven, transactional) versus a custom
//! append-only pack file with a separate index. The interesting question is
//! whether redb's transactional machinery costs too much on pattern C, where
//! Lattice needs a single durable append and nothing else.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

use clap::Parser;
use redb::{Database, TableDefinition};
use serde::Serialize;

const CHUNKS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("chunks");

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 40_000)]
    chunk_count: usize,
    #[arg(long, default_value_t = 8192)]
    chunk_size: usize,
    #[arg(long, default_value_t = 4000)]
    reads: usize,
    #[arg(long, default_value_t = 300)]
    oplog_appends: usize,
    #[arg(long)]
    json_out: Option<String>,
}

#[derive(Serialize, Default)]
struct Row {
    backend: String,
    op: String,
    n: usize,
    total_ms: f64,
    per_op_us: f64,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    bytes_on_disk: u64,
}

fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[(((v.len() as f64) * p).ceil() as usize).saturating_sub(1).min(v.len() - 1)]
}

fn dir_bytes(p: &Path) -> u64 {
    fn walk(p: &Path) -> u64 {
        let mut n = 0;
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    n += walk(&path);
                } else if let Ok(m) = e.metadata() {
                    n += m.len();
                }
            }
        }
        n
    }
    if p.is_dir() { walk(p) } else { p.metadata().map(|m| m.len()).unwrap_or(0) }
}

/// Deterministic pseudo-random content: no RNG dependency, reproducible runs.
fn make_chunk(i: usize, size: usize) -> Vec<u8> {
    let mut v = vec![0u8; size];
    let mut x = (i as u64).wrapping_mul(0x9E3779B97F4A7C15) | 1;
    for b in v.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x >> 24) as u8;
    }
    v
}

fn bench_redb(args: &Args, dir: &Path) -> Vec<Row> {
    let path = dir.join("store.redb");
    let db = Database::create(&path).unwrap();
    let mut rows = Vec::new();

    // A. bulk chunk write, one transaction (the `ltx save` shape).
    let keys: Vec<[u8; 32]> = (0..args.chunk_count)
        .map(|i| *blake3::hash(&make_chunk(i, 64)).as_bytes())
        .collect();
    let t = Instant::now();
    {
        let wtx = db.begin_write().unwrap();
        {
            let mut table = wtx.open_table(CHUNKS).unwrap();
            for (i, k) in keys.iter().enumerate() {
                table.insert(k.as_slice(), make_chunk(i, args.chunk_size).as_slice()).unwrap();
            }
        }
        wtx.commit().unwrap();
    }
    let el = t.elapsed().as_secs_f64() * 1000.0;
    rows.push(Row {
        backend: "redb".into(), op: "bulk_chunk_write".into(), n: args.chunk_count,
        total_ms: el, per_op_us: el * 1000.0 / args.chunk_count as f64,
        bytes_on_disk: dir_bytes(&path), ..Default::default()
    });

    // B. random point reads.
    let mut lat = Vec::with_capacity(args.reads);
    let rtx = db.begin_read().unwrap();
    let table = rtx.open_table(CHUNKS).unwrap();
    let t = Instant::now();
    for i in 0..args.reads {
        let k = keys[(i * 7919) % keys.len()];
        let s = Instant::now();
        let got = table.get(k.as_slice()).unwrap();
        std::hint::black_box(got.map(|v| v.value().len()));
        lat.push(s.elapsed().as_secs_f64() * 1e6);
    }
    let el = t.elapsed().as_secs_f64() * 1000.0;
    rows.push(Row {
        backend: "redb".into(), op: "random_chunk_read".into(), n: args.reads,
        total_ms: el, per_op_us: el * 1000.0 / args.reads as f64,
        p50_us: pct(&mut lat, 0.50), p95_us: pct(&mut lat, 0.95), p99_us: pct(&mut lat, 0.99),
        ..Default::default()
    });

    // C. durable op-log append: one committed transaction per command.
    let oplog = dir.join("oplog.redb");
    let odb = Database::create(&oplog).unwrap();
    let mut lat = Vec::with_capacity(args.oplog_appends);
    let t = Instant::now();
    for i in 0..args.oplog_appends {
        let s = Instant::now();
        let wtx = odb.begin_write().unwrap();
        {
            let mut table = wtx.open_table(CHUNKS).unwrap();
            let key = (i as u64).to_be_bytes();
            table.insert(key.as_slice(), make_chunk(i, 256).as_slice()).unwrap();
        }
        wtx.commit().unwrap();
        lat.push(s.elapsed().as_secs_f64() * 1e6);
    }
    let el = t.elapsed().as_secs_f64() * 1000.0;
    rows.push(Row {
        backend: "redb".into(), op: "durable_oplog_append".into(), n: args.oplog_appends,
        total_ms: el, per_op_us: el * 1000.0 / args.oplog_appends as f64,
        p50_us: pct(&mut lat, 0.50), p95_us: pct(&mut lat, 0.95), p99_us: pct(&mut lat, 0.99),
        bytes_on_disk: dir_bytes(&oplog), ..Default::default()
    });
    rows
}

fn bench_packs(args: &Args, dir: &Path) -> Vec<Row> {
    let pack = dir.join("pack.0");
    let idx = dir.join("pack.0.idx");
    let mut rows = Vec::new();
    let keys: Vec<[u8; 32]> = (0..args.chunk_count)
        .map(|i| *blake3::hash(&make_chunk(i, 64)).as_bytes())
        .collect();

    // A. append-only pack write + index build.
    let mut offsets: Vec<(usize, u64, u32)> = Vec::with_capacity(args.chunk_count);
    let t = Instant::now();
    {
        let mut f = std::io::BufWriter::with_capacity(
            1 << 22, File::create(&pack).unwrap());
        let mut off = 0u64;
        for i in 0..args.chunk_count {
            let c = make_chunk(i, args.chunk_size);
            f.write_all(&c).unwrap();
            offsets.push((i, off, c.len() as u32));
            off += c.len() as u64;
        }
        f.flush().unwrap();
        let inner = f.into_inner().unwrap();
        inner.sync_all().unwrap();

        // Index: sorted key -> (offset, len). One fsync for the whole index.
        let mut entries: Vec<([u8; 32], u64, u32)> =
            offsets.iter().map(|(i, o, l)| (keys[*i], *o, *l)).collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut ix = std::io::BufWriter::new(File::create(&idx).unwrap());
        for (k, o, l) in &entries {
            ix.write_all(k).unwrap();
            ix.write_all(&o.to_le_bytes()).unwrap();
            ix.write_all(&l.to_le_bytes()).unwrap();
        }
        ix.flush().unwrap();
        ix.into_inner().unwrap().sync_all().unwrap();
    }
    let el = t.elapsed().as_secs_f64() * 1000.0;
    rows.push(Row {
        backend: "append_only_packs".into(), op: "bulk_chunk_write".into(),
        n: args.chunk_count, total_ms: el,
        per_op_us: el * 1000.0 / args.chunk_count as f64,
        bytes_on_disk: dir_bytes(&pack) + dir_bytes(&idx), ..Default::default()
    });

    // B. random point reads: binary search the mmap-less index, then pread.
    let mut index_buf = Vec::new();
    File::open(&idx).unwrap().read_to_end(&mut index_buf).unwrap();
    let entry = 44usize; // 32 key + 8 offset + 4 len
    let count = index_buf.len() / entry;
    let mut pf = File::open(&pack).unwrap();
    let mut lat = Vec::with_capacity(args.reads);
    let t = Instant::now();
    for i in 0..args.reads {
        let k = keys[(i * 7919) % keys.len()];
        let s = Instant::now();
        let (mut lo, mut hi) = (0usize, count);
        let mut found = None;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let base = mid * entry;
            match index_buf[base..base + 32].cmp(&k[..]) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let off = u64::from_le_bytes(
                        index_buf[base + 32..base + 40].try_into().unwrap());
                    let len = u32::from_le_bytes(
                        index_buf[base + 40..base + 44].try_into().unwrap());
                    found = Some((off, len));
                    break;
                }
            }
        }
        if let Some((off, len)) = found {
            let mut buf = vec![0u8; len as usize];
            pf.seek(SeekFrom::Start(off)).unwrap();
            pf.read_exact(&mut buf).unwrap();
            std::hint::black_box(buf.len());
        }
        lat.push(s.elapsed().as_secs_f64() * 1e6);
    }
    let el = t.elapsed().as_secs_f64() * 1000.0;
    rows.push(Row {
        backend: "append_only_packs".into(), op: "random_chunk_read".into(),
        n: args.reads, total_ms: el, per_op_us: el * 1000.0 / args.reads as f64,
        p50_us: pct(&mut lat, 0.50), p95_us: pct(&mut lat, 0.95), p99_us: pct(&mut lat, 0.99),
        ..Default::default()
    });

    // C. durable op-log append: write record, fsync. Nothing else.
    let log = dir.join("oplog.bin");
    let mut f = OpenOptions::new().create(true).append(true).open(&log).unwrap();
    let mut lat = Vec::with_capacity(args.oplog_appends);
    let t = Instant::now();
    for i in 0..args.oplog_appends {
        let rec = make_chunk(i, 256);
        let s = Instant::now();
        f.write_all(&(rec.len() as u32).to_le_bytes()).unwrap();
        f.write_all(&rec).unwrap();
        f.sync_data().unwrap();
        lat.push(s.elapsed().as_secs_f64() * 1e6);
    }
    let el = t.elapsed().as_secs_f64() * 1000.0;
    rows.push(Row {
        backend: "append_only_packs".into(), op: "durable_oplog_append".into(),
        n: args.oplog_appends, total_ms: el,
        per_op_us: el * 1000.0 / args.oplog_appends as f64,
        p50_us: pct(&mut lat, 0.50), p95_us: pct(&mut lat, 0.95), p99_us: pct(&mut lat, 0.99),
        bytes_on_disk: dir_bytes(&log), ..Default::default()
    });
    rows
}

fn main() {
    let args = Args::parse();
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let mut rows = bench_redb(&args, d1.path());
    rows.extend(bench_packs(&args, d2.path()));
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "chunk_count": args.chunk_count,
        "chunk_size": args.chunk_size,
        "reads": args.reads,
        "oplog_appends": args.oplog_appends,
        "rows": rows,
    })).unwrap();
    if let Some(p) = &args.json_out {
        std::fs::write(p, &json).unwrap();
    }
    println!("{json}");
}
