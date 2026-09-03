//! G1.8 measurement kernel: how much new data does a 1-byte edit persist?
//!
//! Lives in Rust, using the same `fastcdc` crate the engine will use, so there
//! is exactly ONE chunker implementation in the project. A second one written
//! in the harness would eventually disagree with the product, and the harness
//! copy is the one nobody would notice was wrong.
//!
//! Generates the five PINNED file types deterministically from a fixed seed, so
//! the corpus cannot drift between runs and cannot be chosen to flatter the
//! result.

use std::collections::HashSet;

use serde::Serialize;

const CHUNK_MIN: u32 = 2048;
const CHUNK_AVG: u32 = 8192;
const CHUNK_MAX: u32 = 32768;

/// xorshift64*: deterministic, seedable, no rand dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            out.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        out.truncate(n);
        out
    }
}

fn make_file(kind: &str, size: usize, rng: &mut Rng) -> Vec<u8> {
    match kind {
        "incompressible" => rng.bytes(size),
        "source_text" => {
            let lines: Vec<String> = (0..512)
                .map(|i| format!("    let value_{i} = compute(&context, {i});\n"))
                .collect();
            let mut out = Vec::with_capacity(size + 128);
            while out.len() < size {
                out.extend_from_slice(lines[rng.below(lines.len())].as_bytes());
            }
            out.truncate(size);
            out
        }
        "structured_binary" => {
            let mut out = Vec::with_capacity(size + 4096);
            let record = 4096usize;
            while out.len() < size {
                out.extend_from_slice(&((out.len() / record) as u64).to_le_bytes());
                out.extend_from_slice(&rng.bytes(record - 8));
            }
            out.truncate(size);
            out
        }
        "sparse_zeros" => {
            let mut out = Vec::with_capacity(size + 65536);
            while out.len() < size {
                let zeros = 4096 + rng.below(61440);
                out.extend(std::iter::repeat(0u8).take(zeros));
                let n = 512 + rng.below(3584);
                out.extend_from_slice(&rng.bytes(n));
            }
            out.truncate(size);
            out
        }
        "mixed_media" => {
            let mut out = Vec::with_capacity(size + (1 << 18));
            while out.len() < size {
                out.extend_from_slice(
                    format!("--boundary-{}\nContent-Type: application/octet-stream\n\n", out.len())
                        .as_bytes(),
                );
                let n = (1 << 16) + rng.below(1 << 17);
                out.extend_from_slice(&rng.bytes(n));
            }
            out.truncate(size);
            out
        }
        other => panic!("unknown file type {other}"),
    }
}

fn chunk_keys(data: &[u8]) -> Vec<([u8; 32], usize)> {
    fastcdc::v2020::FastCDC::new(data, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX)
        .map(|c| {
            (
                *blake3::hash(&data[c.offset..c.offset + c.length]).as_bytes(),
                c.length,
            )
        })
        .collect()
}

#[derive(Serialize)]
struct TypeResult {
    kind: String,
    trials: usize,
    p50: u64,
    p95: u64,
    max: u64,
    samples: Vec<u64>,
}

#[derive(Serialize)]
struct Report {
    seed: u64,
    file_bytes: usize,
    trials_per_type: usize,
    chunk_min: u32,
    chunk_avg: u32,
    chunk_max: u32,
    per_type: Vec<TypeResult>,
    overall_p95: u64,
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (((sorted.len() as f64) * p + 0.5) as usize).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seed: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20260903);
    let file_bytes: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50 * 1024 * 1024);
    let trials: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);

    let kinds = [
        "incompressible",
        "source_text",
        "structured_binary",
        "sparse_zeros",
        "mixed_media",
    ];

    let mut per_type = Vec::new();
    let mut all: Vec<u64> = Vec::new();

    for kind in kinds {
        // Content seeded per-kind so each file is stable and independent.
        let mut content_rng = Rng::new(seed ^ kind.len() as u64);
        let data = make_file(kind, file_bytes, &mut content_rng);

        let before: HashSet<[u8; 32]> = chunk_keys(&data).into_iter().map(|(k, _)| k).collect();

        let mut offset_rng = Rng::new(seed.wrapping_add(0x9E3779B97F4A7C15));
        let mut samples = Vec::with_capacity(trials);
        let mut edited = data.clone();
        for _ in 0..trials {
            let off = offset_rng.below(data.len());
            edited.copy_from_slice(&data);
            edited[off] ^= 0xFF;
            let new_bytes: u64 = chunk_keys(&edited)
                .into_iter()
                .filter(|(k, _)| !before.contains(k))
                .map(|(_, l)| l as u64)
                .sum();
            samples.push(new_bytes);
        }
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        all.extend_from_slice(&samples);
        per_type.push(TypeResult {
            kind: kind.to_string(),
            trials,
            p50: pct(&sorted, 0.50),
            p95: pct(&sorted, 0.95),
            max: *sorted.last().unwrap_or(&0),
            samples,
        });
    }

    all.sort_unstable();
    let report = Report {
        seed,
        file_bytes,
        trials_per_type: trials,
        chunk_min: CHUNK_MIN,
        chunk_avg: CHUNK_AVG,
        chunk_max: CHUNK_MAX,
        overall_p95: pct(&all, 0.95),
        per_type,
    };
    println!("{}", serde_json::to_string(&report).unwrap());
}
