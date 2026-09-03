//! ADR-2 prototype: what chunk parameters should Lattice use?
//!
//! Measures, over a real corpus, for each candidate (min, avg, max) triple:
//!   - deduplication ratio (unique chunk bytes / total bytes)
//!   - chunk count and therefore index overhead
//!   - stored size after per-chunk zstd, which is what actually lands on disk
//!   - the small-file crossover: where per-chunk overhead exceeds the dedup win
//!     and whole-file compression (git's regime) would be better
//!
//! This is a prototype. It is deliberately not in the product workspace and is
//! never imported by it (§ Stage G0: "throwaway prototypes kept under
//! prototypes/, never imported by product crates").

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Parser;
use rayon::prelude::*;
use serde::Serialize;

/// Index cost per chunk, in bytes, for a store that must be able to find a
/// chunk and reassemble a file from chunks:
///   32 (BLAKE3 key) + 8 (pack offset) + 4 (length) + 4 (pack id)
/// Deliberately optimistic; a real store also pays B-tree/page overhead.
const INDEX_BYTES_PER_CHUNK: u64 = 48;

#[derive(Parser)]
struct Args {
    /// Directories to walk for corpus material.
    #[arg(required = true)]
    roots: Vec<PathBuf>,
    /// Cap total bytes read, so a sweep is bounded and repeatable.
    #[arg(long, default_value_t = 3_000_000_000)]
    max_bytes: u64,
    /// Skip files larger than this.
    #[arg(long, default_value_t = 268_435_456)]
    max_file: u64,
    #[arg(long, default_value_t = 3)]
    zstd_level: i32,
    #[arg(long)]
    json_out: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
struct Params {
    min: u32,
    avg: u32,
    max: u32,
}

#[derive(Serialize)]
struct SweepRow {
    min: u32,
    avg: u32,
    max: u32,
    total_bytes: u64,
    unique_bytes: u64,
    dedup_ratio: f64,
    chunks_total: u64,
    chunks_unique: u64,
    mean_chunk_bytes: f64,
    stored_compressed_bytes: u64,
    index_bytes: u64,
    total_store_bytes: u64,
    /// Store size relative to whole-file zstd of the same corpus. <1.0 means
    /// chunking wins; >1.0 means chunking is paying more than it saves.
    vs_wholefile_ratio: f64,
    edit_locality_p95_bytes: u64,
    /// Unique chunks concatenated into one pack and compressed with a long
    /// window, so zstd can find redundancy ACROSS chunks. This approximates
    /// the cross-chunk delta compression that gives git's aggressive repack
    /// its advantage, and is the strategy Challenge 9's bet named.
    pack_long_bytes: u64,
    pack_long_total_store_bytes: u64,
}

#[derive(Serialize)]
struct SmallFileRow {
    bucket: String,
    files: u64,
    total_bytes: u64,
    chunked_store_bytes: u64,
    wholefile_store_bytes: u64,
    ratio: f64,
}

#[derive(Serialize)]
struct Report {
    corpus_files: u64,
    corpus_bytes: u64,
    wholefile_compressed_bytes: u64,
    zstd_level: i32,
    index_bytes_per_chunk: u64,
    sweep: Vec<SweepRow>,
    small_file_crossover: Vec<SmallFileRow>,
    crossover_note: String,
}

fn collect(roots: &[PathBuf], max_bytes: u64, max_file: u64) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut total: u64 = 0;
    for root in roots {
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if total >= max_bytes {
                return out;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(md) = entry.metadata() else { continue };
            if md.len() == 0 || md.len() > max_file {
                continue;
            }
            if let Ok(data) = fs::read(entry.path()) {
                total += data.len() as u64;
                out.push(data);
            }
        }
    }
    out
}

fn chunk_offsets(data: &[u8], p: Params) -> Vec<(usize, usize)> {
    fastcdc::v2020::FastCDC::new(data, p.min, p.avg, p.max)
        .map(|c| (c.offset, c.length))
        .collect()
}

/// G1.8's question, asked of a parameter set: flip one byte at a random offset
/// in a large file and count how many bytes of *new* chunk content result.
fn edit_locality(data: &[u8], p: Params, trials: usize) -> Vec<u64> {
    let before: HashMap<[u8; 32], usize> = chunk_offsets(data, p)
        .into_iter()
        .map(|(o, l)| (*blake3::hash(&data[o..o + l]).as_bytes(), l))
        .collect();

    // Deterministic offsets: no RNG, so the number is reproducible.
    let mut results = Vec::with_capacity(trials);
    for t in 0..trials {
        let off = (data.len() / (trials + 1)) * (t + 1);
        if off >= data.len() {
            continue;
        }
        let mut edited = data.to_vec();
        edited[off] ^= 0xFF;
        let new_bytes: u64 = chunk_offsets(&edited, p)
            .into_iter()
            .filter_map(|(o, l)| {
                let key = *blake3::hash(&edited[o..o + l]).as_bytes();
                (!before.contains_key(&key)).then_some(l as u64)
            })
            .sum();
        results.push(new_bytes);
    }
    results
}

fn p95(mut v: Vec<u64>) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[((v.len() as f64 * 0.95).ceil() as usize).saturating_sub(1).min(v.len() - 1)]
}

fn sweep_one(files: &[Vec<u8>], p: Params, level: i32, wholefile: u64) -> SweepRow {
    // Per-file chunking in parallel, then a global unique-chunk fold.
    let per_file: Vec<Vec<([u8; 32], usize)>> = files
        .par_iter()
        .map(|data| {
            chunk_offsets(data, p)
                .into_iter()
                .map(|(o, l)| (*blake3::hash(&data[o..o + l]).as_bytes(), l))
                .collect()
        })
        .collect();

    let mut unique: HashMap<[u8; 32], usize> = HashMap::new();
    let mut chunks_total: u64 = 0;
    for f in &per_file {
        chunks_total += f.len() as u64;
        for (k, l) in f {
            unique.insert(*k, *l);
        }
    }

    let total_bytes: u64 = files.iter().map(|f| f.len() as u64).sum();
    let unique_bytes: u64 = unique.values().map(|l| *l as u64).sum();

    // Compress unique chunk content, which is what a real store persists.
    // Sample when the unique set is very large: compressing every chunk of a
    // 3 GB corpus for every parameter set is not the measurement's point.
    let keys: Vec<&[u8; 32]> = unique.keys().collect();
    let sample_stride = (keys.len() / 20_000).max(1);
    let sampled: Vec<usize> = (0..keys.len()).step_by(sample_stride).collect();
    let mut sampled_raw: u64 = 0;
    let mut sampled_comp: u64 = 0;
    {
        // Re-chunk lazily to recover chunk bytes for the sampled keys.
        let want: std::collections::HashSet<[u8; 32]> =
            sampled.iter().map(|i| *keys[*i]).collect();
        let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
        for data in files {
            for (o, l) in chunk_offsets(data, p) {
                let key = *blake3::hash(&data[o..o + l]).as_bytes();
                if want.contains(&key) && seen.insert(key) {
                    sampled_raw += l as u64;
                    sampled_comp +=
                        zstd::encode_all(&data[o..o + l], level).map(|v| v.len() as u64).unwrap_or(l as u64);
                }
            }
        }
    }
    let compression = if sampled_raw > 0 {
        sampled_comp as f64 / sampled_raw as f64
    } else {
        1.0
    };
    let stored_compressed = (unique_bytes as f64 * compression) as u64;
    let index_bytes = unique.len() as u64 * INDEX_BYTES_PER_CHUNK;
    let total_store = stored_compressed + index_bytes;

    // Pack-level long-window compression: concatenate unique chunks in
    // first-seen order (which keeps versions of the same file adjacent) and
    // compress the whole pack with a 128 MiB window, so cross-chunk redundancy
    // is available to the compressor.
    let mut pack: Vec<u8> = Vec::with_capacity(unique_bytes as usize);
    {
        let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
        for data in files {
            for (o, l) in chunk_offsets(data, p) {
                let key = *blake3::hash(&data[o..o + l]).as_bytes();
                if seen.insert(key) {
                    pack.extend_from_slice(&data[o..o + l]);
                }
            }
        }
    }
    let pack_long_bytes = {
        let mut enc = zstd::Encoder::new(Vec::new(), level).unwrap();
        enc.long_distance_matching(true).ok();
        enc.window_log(27).ok();
        enc.write_all(&pack).ok();
        enc.finish().map(|v| v.len() as u64).unwrap_or(pack.len() as u64)
    };
    drop(pack);

    // Edit locality on the largest file available.
    let biggest = files.iter().max_by_key(|f| f.len());
    let locality = biggest.map(|d| edit_locality(d, p, 20)).unwrap_or_default();

    SweepRow {
        min: p.min,
        avg: p.avg,
        max: p.max,
        total_bytes,
        unique_bytes,
        dedup_ratio: unique_bytes as f64 / total_bytes.max(1) as f64,
        chunks_total,
        chunks_unique: unique.len() as u64,
        mean_chunk_bytes: unique_bytes as f64 / unique.len().max(1) as f64,
        stored_compressed_bytes: stored_compressed,
        index_bytes,
        total_store_bytes: total_store,
        vs_wholefile_ratio: total_store as f64 / wholefile.max(1) as f64,
        edit_locality_p95_bytes: p95(locality),
        pack_long_bytes,
        pack_long_total_store_bytes: pack_long_bytes + index_bytes,
    }
}

fn small_file_analysis(files: &[Vec<u8>], p: Params, level: i32) -> Vec<SmallFileRow> {
    let buckets: [(&str, u64, u64); 7] = [
        ("<1KiB", 0, 1024),
        ("1-4KiB", 1024, 4096),
        ("4-16KiB", 4096, 16384),
        ("16-64KiB", 16384, 65536),
        ("64-256KiB", 65536, 262_144),
        ("256KiB-1MiB", 262_144, 1_048_576),
        (">=1MiB", 1_048_576, u64::MAX),
    ];
    buckets
        .iter()
        .map(|(name, lo, hi)| {
            let set: Vec<&Vec<u8>> = files
                .iter()
                .filter(|f| (f.len() as u64) >= *lo && (f.len() as u64) < *hi)
                .collect();
            let total: u64 = set.iter().map(|f| f.len() as u64).sum();
            let mut unique: HashMap<[u8; 32], u64> = HashMap::new();
            let mut chunked_comp: u64 = 0;
            for f in &set {
                for (o, l) in chunk_offsets(f, p) {
                    let key = *blake3::hash(&f[o..o + l]).as_bytes();
                    if unique.insert(key, l as u64).is_none() {
                        chunked_comp += zstd::encode_all(&f[o..o + l], level)
                            .map(|v| v.len() as u64)
                            .unwrap_or(l as u64);
                    }
                }
            }
            let chunked = chunked_comp + unique.len() as u64 * INDEX_BYTES_PER_CHUNK;
            let whole: u64 = set
                .iter()
                .map(|f| {
                    zstd::encode_all(f.as_slice(), level)
                        .map(|v| v.len() as u64)
                        .unwrap_or(f.len() as u64)
                })
                .sum();
            SmallFileRow {
                bucket: name.to_string(),
                files: set.len() as u64,
                total_bytes: total,
                chunked_store_bytes: chunked,
                wholefile_store_bytes: whole,
                ratio: chunked as f64 / whole.max(1) as f64,
            }
        })
        .collect()
}

fn main() {
    let args = Args::parse();
    eprintln!("collecting corpus …");
    let files = collect(&args.roots, args.max_bytes, args.max_file);
    let corpus_bytes: u64 = files.iter().map(|f| f.len() as u64).sum();
    eprintln!("{} files, {:.2} GiB", files.len(), corpus_bytes as f64 / (1 << 30) as f64);

    eprintln!("whole-file baseline (git's regime) …");
    let wholefile: u64 = files
        .par_iter()
        .map(|f| {
            zstd::encode_all(f.as_slice(), args.zstd_level)
                .map(|v| v.len() as u64)
                .unwrap_or(f.len() as u64)
        })
        .sum();

    let candidates = [
        Params { min: 1024, avg: 2048, max: 8192 },
        Params { min: 2048, avg: 4096, max: 16384 },
        Params { min: 2048, avg: 8192, max: 32768 },
        Params { min: 4096, avg: 8192, max: 32768 },
        Params { min: 4096, avg: 16384, max: 65536 },
        Params { min: 8192, avg: 16384, max: 65536 },
        Params { min: 8192, avg: 32768, max: 131_072 },
        Params { min: 16384, avg: 65536, max: 262_144 },
    ];

    let mut sweep = Vec::new();
    for p in candidates {
        eprintln!("sweeping min={} avg={} max={} …", p.min, p.avg, p.max);
        sweep.push(sweep_one(&files, p, args.zstd_level, wholefile));
    }

    eprintln!("small-file crossover analysis …");
    let crossover = small_file_analysis(&files, Params { min: 2048, avg: 8192, max: 32768 }, args.zstd_level);

    let report = Report {
        corpus_files: files.len() as u64,
        corpus_bytes,
        wholefile_compressed_bytes: wholefile,
        zstd_level: args.zstd_level,
        index_bytes_per_chunk: INDEX_BYTES_PER_CHUNK,
        sweep,
        small_file_crossover: crossover,
        crossover_note: "ratio > 1.0 means content-defined chunking costs more \
                         than whole-file compression for that size bucket, which \
                         is the regime where git's pack format wins."
            .to_string(),
    };
    let json = serde_json::to_string_pretty(&report).unwrap();
    if let Some(path) = &args.json_out {
        fs::write(path, &json).unwrap();
        eprintln!("wrote {}", path.display());
    }
    println!("{json}");
}

#[allow(dead_code)]
fn unused_path_guard(_: &Path) {}
