//! Content-defined chunking.
//!
//! Every byte of content passes through here — §5.1 makes that a constraint,
//! and ADR-2 measured what it costs and what it buys.
//!
//! The parameters are not adjustable at runtime. ADR-2's finding was that the
//! chunk sizes barely matter for storage (the compression boundary dominates)
//! but that `MAX` alone determines whether G1.8 can pass: a one-byte edit
//! invalidates at most a few chunks, so the worst case is bounded by the
//! largest chunk. A 256 KiB maximum would put that worst case around 786 KiB
//! and fail the gate outright, whatever the ratio looked like.

use std::fmt;

/// ADR-2. Changing these without changing the ADR makes the ADR's evidence
/// section describe a system that no longer exists.
pub const CHUNK_MIN: u32 = 2 * 1024;
pub const CHUNK_AVG: u32 = 8 * 1024;
pub const CHUNK_MAX: u32 = 32 * 1024;

/// A BLAKE3 content address.
///
/// Content addresses are compared and stored as raw bytes, never as strings.
/// Rendering only happens at the edge, for humans.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkId(pub [u8; 32]);

impl ChunkId {
    pub fn of(bytes: &[u8]) -> Self {
        ChunkId(*blake3::hash(bytes).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            use fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(ChunkId(out))
    }
}

impl fmt::Debug for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Short form: enough to identify in a log, never enough to be mistaken
        // for the full address.
        write!(f, "chunk:{}…", &self.to_hex()[..12])
    }
}

/// One chunk's position in the content it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub id: ChunkId,
    pub offset: u64,
    pub len: u32,
}

/// Split content into content-defined chunks.
///
/// Empty input yields no chunks. That is deliberate and load-bearing: a
/// zero-byte file is a real case (G1.2 names it), and it is represented as a
/// file with an empty chunk list rather than as a chunk of length zero — one
/// empty chunk would collide with every other empty chunk and make the
/// distinction between "empty file" and "no file" depend on the store.
pub fn split(content: &[u8]) -> Vec<Chunk> {
    if content.is_empty() {
        return Vec::new();
    }
    fastcdc::v2020::FastCDC::new(content, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX)
        .map(|c| Chunk {
            id: ChunkId::of(&content[c.offset..c.offset + c.length]),
            offset: c.offset as u64,
            len: c.length as u32,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_content_produces_no_chunks() {
        assert!(split(b"").is_empty());
    }

    #[test]
    fn chunks_cover_the_content_exactly_once() {
        let content: Vec<u8> = (0..500_000u32).map(|i| (i * 37) as u8).collect();
        let chunks = split(&content);
        assert!(!chunks.is_empty());

        let mut cursor = 0u64;
        for c in &chunks {
            assert_eq!(c.offset, cursor, "chunks must be contiguous with no gap");
            cursor += u64::from(c.len);
        }
        assert_eq!(
            cursor,
            content.len() as u64,
            "chunks must cover all content"
        );
    }

    #[test]
    fn chunk_sizes_respect_the_adr2_bounds() {
        let content: Vec<u8> = (0..2_000_000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        let chunks = split(&content);
        for c in chunks.iter().take(chunks.len() - 1) {
            assert!(
                c.len <= CHUNK_MAX,
                "chunk of {} exceeds MAX {CHUNK_MAX}; G1.8's worst case is bounded by MAX",
                c.len
            );
        }
    }

    #[test]
    fn identical_content_yields_identical_addresses() {
        let a: Vec<u8> = (0..300_000u32).map(|i| (i * 91) as u8).collect();
        let b = a.clone();
        let ids_a: Vec<_> = split(&a).into_iter().map(|c| c.id).collect();
        let ids_b: Vec<_> = split(&b).into_iter().map(|c| c.id).collect();
        assert_eq!(ids_a, ids_b);
    }

    #[test]
    fn a_one_byte_edit_changes_few_chunks() {
        // The property G1.8 measures, asserted here at unit scale so a
        // regression is caught before the gate runs.
        let mut content: Vec<u8> = (0..1_000_000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 11) as u8)
            .collect();
        let before: std::collections::HashSet<_> =
            split(&content).into_iter().map(|c| c.id).collect();

        content[500_000] ^= 0xFF;
        let new_bytes: u64 = split(&content)
            .into_iter()
            .filter(|c| !before.contains(&c.id))
            .map(|c| u64::from(c.len))
            .sum();

        assert!(
            new_bytes < 256 * 1024,
            "a one-byte edit produced {new_bytes} new bytes; G1.8 caps this at 256 KiB"
        );
    }

    #[test]
    fn hex_round_trips() {
        let id = ChunkId::of(b"lattice");
        assert_eq!(ChunkId::from_hex(&id.to_hex()), Some(id));
        assert_eq!(ChunkId::from_hex("nonsense"), None);
    }
}
