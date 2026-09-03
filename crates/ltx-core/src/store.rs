//! Content-addressed chunk store: append-only packs with a sorted index.
//!
//! ADR-3 chose this over an embedded key-value store for chunk content after
//! measuring both: packs were 2.3× faster to write, 2× faster to read, and used
//! **3.3× less disk** than redb for identical content. That last figure alone
//! settled it — redb wrote 1,028 MiB for 312 MiB of chunks, and G1.9 caps the
//! whole store at 1.25× restic.
//!
//! Immutability is what makes this safe to hand-roll. A pack is written once,
//! fsynced, and never mutated, so there is no update-in-place to get wrong. The
//! durability ordering is the one invariant that matters:
//!
//!   **content is durable before the metadata that references it**
//!
//! A crash can therefore leave unreferenced chunks (garbage, collected later)
//! but never a dangling reference. G1.1's fault injector attacks precisely this
//! ordering, so it is stated here rather than left implicit in the call order.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::chunk::ChunkId;
use crate::error::{Error, Result};

/// Bytes of chunk payload per independently-decompressible zstd frame.
///
/// ADR-2: a solid pack compressed to 0.51× a `git gc --aggressive` pack, but a
/// solid pack must be decompressed from its start to reach any chunk. At 4 MiB
/// segments the ratio is 0.63–0.75× and a random read touches at most one
/// segment. That trade — roughly a fifth of the compression win for bounded
/// random access — is the whole reason this constant exists.
const SEGMENT_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

const ZSTD_LEVEL: i32 = 3;

/// index entry: 32-byte key, then u64 segment index, u32 offset-in-segment,
/// u32 length. Fixed width so the index can be binary-searched in place.
const INDEX_ENTRY_BYTES: usize = 32 + 8 + 4 + 4;

const PACK_MAGIC: &[u8; 8] = b"LTXPACK1";
const INDEX_MAGIC: &[u8; 8] = b"LTXIDX01";

/// Where a chunk lives inside a pack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Located {
    pub segment: u64,
    pub offset: u32,
    pub len: u32,
}

/// Accumulates chunks and writes one pack plus its index.
///
/// Chunks are held in insertion order, which is how ADR-2's pack-ordering
/// finding is honoured: the caller feeds chunks grouped by source path, and
/// that grouping is what let long-range compression reach 0.51× instead of
/// 0.87×. Ordering is part of the format, not an implementation detail.
pub struct PackWriter {
    /// Insertion-ordered payloads, deduplicated by address.
    pending: Vec<(ChunkId, Vec<u8>)>,
    seen: BTreeMap<ChunkId, ()>,
    bytes: usize,
}

impl Default for PackWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl PackWriter {
    pub fn new() -> Self {
        PackWriter {
            pending: Vec::new(),
            seen: BTreeMap::new(),
            bytes: 0,
        }
    }

    /// Returns true if this chunk was new to the pack.
    pub fn add(&mut self, id: ChunkId, payload: &[u8]) -> bool {
        if self.seen.contains_key(&id) {
            return false;
        }
        self.seen.insert(id, ());
        self.bytes += payload.len();
        self.pending.push((id, payload.to_vec()));
        true
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drop chunks the store already holds durably.
    ///
    /// Cross-pack deduplication: content is addressed by hash, so a chunk that
    /// already lives in an older pack need not be written again — the tree that
    /// references it still resolves through `Store::read`. Without this, every
    /// save re-stored the entire working tree and a one-byte edit re-persisted
    /// every unchanged file, which is exactly what G1.8 and G1.9 forbid.
    ///
    /// The presence test is an index lookup (`contains`), not a re-read of the
    /// payload. This is the content-addressed dedup contract that git, restic
    /// and borg all use: a save trusts that an already-stored address holds
    /// correct bytes. Re-verifying every candidate would re-read the whole
    /// working set on each save — O(content), blowing the G1.5 latency budget
    /// and defeating the point of dedup. A pack that bit-rots after it was
    /// written is a separate failure surfaced by `verify` and recovered by
    /// refetch, not something a write-time check can prevent without paying
    /// that cost on every save.
    pub fn retain_unknown(&mut self, store: &Store) {
        self.pending.retain(|(id, _)| !store.contains(*id));
        self.seen = self.pending.iter().map(|(id, _)| (*id, ())).collect();
        self.bytes = self.pending.iter().map(|(_, p)| p.len()).sum();
    }

    pub fn chunk_count(&self) -> usize {
        self.pending.len()
    }

    pub fn payload_bytes(&self) -> usize {
        self.bytes
    }

    /// Write the pack and its index, making both durable.
    ///
    /// Order is deliberate and is the invariant G1.1 attacks: the pack's data
    /// is fsynced BEFORE the index that points into it, and the index is
    /// fsynced before this returns. A crash between the two leaves a pack with
    /// no index — unreferenced bytes that recovery discards — never an index
    /// entry pointing at content that was never written.
    pub fn finish(self, dir: &Path, pack_id: u64) -> Result<Vec<(ChunkId, Located)>> {
        fs::create_dir_all(dir)?;
        let pack_path = dir.join(format!("{pack_id:012}.pack"));
        let index_path = dir.join(format!("{pack_id:012}.idx"));

        let mut located: Vec<(ChunkId, Located)> = Vec::with_capacity(self.pending.len());
        let mut segment_payload: Vec<u8> = Vec::with_capacity(SEGMENT_PAYLOAD_BYTES);
        let mut segment_members: Vec<(ChunkId, u32, u32)> = Vec::new();
        let mut segment_index: u64 = 0;

        let file = File::create(&pack_path)?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        out.write_all(PACK_MAGIC)?;

        // Segment table is written after the payload; record offsets as we go.
        let mut segment_offsets: Vec<(u64, u32)> = Vec::new();
        let mut pack_cursor: u64 = PACK_MAGIC.len() as u64;

        let flush_segment = |out: &mut BufWriter<File>,
                             payload: &mut Vec<u8>,
                             members: &mut Vec<(ChunkId, u32, u32)>,
                             seg: &mut u64,
                             cursor: &mut u64,
                             offsets: &mut Vec<(u64, u32)>,
                             located: &mut Vec<(ChunkId, Located)>|
         -> Result<()> {
            if payload.is_empty() {
                return Ok(());
            }
            let compressed = zstd::stream::encode_all(&payload[..], ZSTD_LEVEL)?;
            out.write_all(&(compressed.len() as u32).to_le_bytes())?;
            out.write_all(&compressed)?;
            offsets.push((*cursor, compressed.len() as u32));
            *cursor += 4 + compressed.len() as u64;
            for (id, off, len) in members.drain(..) {
                located.push((
                    id,
                    Located {
                        segment: *seg,
                        offset: off,
                        len,
                    },
                ));
            }
            payload.clear();
            *seg += 1;
            Ok(())
        };

        for (id, payload) in self.pending {
            if segment_payload.len() + payload.len() > SEGMENT_PAYLOAD_BYTES
                && !segment_payload.is_empty()
            {
                flush_segment(
                    &mut out,
                    &mut segment_payload,
                    &mut segment_members,
                    &mut segment_index,
                    &mut pack_cursor,
                    &mut segment_offsets,
                    &mut located,
                )?;
            }
            let offset = segment_payload.len() as u32;
            let len = payload.len() as u32;
            segment_payload.extend_from_slice(&payload);
            segment_members.push((id, offset, len));
        }
        flush_segment(
            &mut out,
            &mut segment_payload,
            &mut segment_members,
            &mut segment_index,
            &mut pack_cursor,
            &mut segment_offsets,
            &mut located,
        )?;

        // Segment table, then its own offset, so a reader can find it from the tail.
        let table_offset = pack_cursor;
        for (off, len) in &segment_offsets {
            out.write_all(&off.to_le_bytes())?;
            out.write_all(&len.to_le_bytes())?;
        }
        out.write_all(&(segment_offsets.len() as u64).to_le_bytes())?;
        out.write_all(&table_offset.to_le_bytes())?;
        out.flush()?;
        // Content durable BEFORE the index that references it.
        out.into_inner()
            .map_err(|e| Error::Io(e.into_error()))?
            .sync_all()?;

        located.sort_unstable_by_key(|a| a.0);
        let mut idx = BufWriter::new(File::create(&index_path)?);
        idx.write_all(INDEX_MAGIC)?;
        idx.write_all(&(located.len() as u64).to_le_bytes())?;
        for (id, loc) in &located {
            idx.write_all(id.as_bytes())?;
            idx.write_all(&loc.segment.to_le_bytes())?;
            idx.write_all(&loc.offset.to_le_bytes())?;
            idx.write_all(&loc.len.to_le_bytes())?;
        }
        idx.flush()?;
        idx.into_inner()
            .map_err(|e| Error::Io(e.into_error()))?
            .sync_all()?;

        // The directory entry itself must be durable, or a crash can lose the
        // files entirely while their contents sit safely on disk. This is the
        // barrier G1.1's replayer models as OP_DIRSYNC.
        sync_dir(dir)?;
        Ok(located)
    }
}

fn sync_dir(dir: &Path) -> Result<()> {
    crate::platform::sync_dir(dir)
}

/// Read-side view of one pack.
pub struct Pack {
    pack_path: PathBuf,
    index: Vec<u8>,
    entries: usize,
    segments: Vec<(u64, u32)>,
}

impl Pack {
    pub fn open(dir: &Path, pack_id: u64) -> Result<Self> {
        let pack_path = dir.join(format!("{pack_id:012}.pack"));
        let index_path = dir.join(format!("{pack_id:012}.idx"));

        let mut index = Vec::new();
        File::open(&index_path)?.read_to_end(&mut index)?;
        if index.len() < 16 || &index[..8] != INDEX_MAGIC {
            return Err(Error::Corrupt(format!(
                "{} is not a Lattice index",
                index_path.display()
            )));
        }
        let declared = u64::from_le_bytes(index[8..16].try_into().unwrap());
        // A valid index is EXACTLY 16 + entries*48 bytes. A declared count that
        // overflows that arithmetic, or that does not match the actual length,
        // is corruption or a crash truncation — never a valid index. Checked
        // arithmetic and an exact-length test so a garbage count reports Corrupt
        // rather than wrapping (in release builds) into an in-bounds value that
        // then indexes out of bounds and panics during a lookup.
        let expected = (declared as usize)
            .checked_mul(INDEX_ENTRY_BYTES)
            .and_then(|n| n.checked_add(16));
        let entries = match expected {
            Some(expected) if index.len() == expected => declared as usize,
            _ => {
                return Err(Error::Corrupt(format!(
                    "{} declares {declared} entries but holds {} bytes",
                    index_path.display(),
                    index.len()
                )));
            }
        };

        let mut file = File::open(&pack_path)?;
        let size = file.metadata()?.len();
        if size < 16 {
            return Err(Error::Corrupt(format!(
                "{} is truncated",
                pack_path.display()
            )));
        }
        file.seek(SeekFrom::End(-16))?;
        let mut tail = [0u8; 16];
        file.read_exact(&mut tail)?;
        let count = u64::from_le_bytes(tail[0..8].try_into().unwrap());
        let table_offset = u64::from_le_bytes(tail[8..16].try_into().unwrap());

        // The segment table is `count` 12-byte entries at table_offset, ahead
        // of the 16-byte tail. Validate that footprint against the real file
        // size BEFORE trusting `count` to size an allocation or drive a loop —
        // a corrupt tail must report Corrupt, not allocate gigabytes or panic.
        let table_end = (count)
            .checked_mul(12)
            .and_then(|b| b.checked_add(table_offset))
            .and_then(|e| e.checked_add(16));
        match table_end {
            Some(end) if table_offset <= size && end <= size => {}
            _ => {
                return Err(Error::Corrupt(format!(
                    "{} has an invalid segment table ({count} entries at offset \
                     {table_offset}, file is {size} bytes)",
                    pack_path.display()
                )));
            }
        }
        let count = count as usize;

        let mut segments = Vec::with_capacity(count);
        file.seek(SeekFrom::Start(table_offset))?;
        for _ in 0..count {
            let mut e = [0u8; 12];
            file.read_exact(&mut e)?;
            let seg_off = u64::from_le_bytes(e[0..8].try_into().unwrap());
            let seg_len = u32::from_le_bytes(e[8..12].try_into().unwrap());
            // Each segment occupies [seg_off, seg_off + 4 + seg_len). Validate
            // it lies within the file so Pack::read can allocate and seek from
            // these values without a stat and without risking an OOM.
            let within = seg_off
                .checked_add(4)
                .and_then(|o| o.checked_add(u64::from(seg_len)))
                .map(|end| end <= size)
                .unwrap_or(false);
            if !within {
                return Err(Error::Corrupt(format!(
                    "{} has a segment that runs past end of file",
                    pack_path.display()
                )));
            }
            segments.push((seg_off, seg_len));
        }

        Ok(Pack {
            pack_path,
            index,
            entries,
            segments,
        })
    }

    pub fn locate(&self, id: ChunkId) -> Option<Located> {
        let (mut lo, mut hi) = (0usize, self.entries);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let base = 16 + mid * INDEX_ENTRY_BYTES;
            let key = &self.index[base..base + 32];
            match key.cmp(id.as_bytes().as_slice()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    return Some(Located {
                        segment: u64::from_le_bytes(
                            self.index[base + 32..base + 40].try_into().unwrap(),
                        ),
                        offset: u32::from_le_bytes(
                            self.index[base + 40..base + 44].try_into().unwrap(),
                        ),
                        len: u32::from_le_bytes(
                            self.index[base + 44..base + 48].try_into().unwrap(),
                        ),
                    });
                }
            }
        }
        None
    }

    /// Read one chunk, verifying its content against its address.
    ///
    /// The verification is not optional and not a debug aid: a content-addressed
    /// store that returns bytes not matching the key it was asked for has
    /// silently corrupted the caller's data, and every "no data loss" claim in
    /// this project rests on that never happening unnoticed.
    pub fn read(&self, id: ChunkId) -> Result<Option<Vec<u8>>> {
        let Some(loc) = self.locate(id) else {
            return Ok(None);
        };
        let Some(&(seg_off, seg_len)) = self.segments.get(loc.segment as usize) else {
            return Err(Error::Corrupt(format!(
                "{} references segment {} of {}",
                self.pack_path.display(),
                loc.segment,
                self.segments.len()
            )));
        };

        let mut file = File::open(&self.pack_path)?;
        file.seek(SeekFrom::Start(seg_off + 4))?;
        let mut compressed = vec![0u8; seg_len as usize];
        file.read_exact(&mut compressed)?;
        let payload = zstd::stream::decode_all(&compressed[..])?;

        let start = loc.offset as usize;
        let end = start + loc.len as usize;
        if end > payload.len() {
            return Err(Error::Corrupt(format!(
                "chunk {:?} runs past its segment in {}",
                id,
                self.pack_path.display()
            )));
        }
        let bytes = payload[start..end].to_vec();
        let actual = ChunkId::of(&bytes);
        if actual != id {
            return Err(Error::Corrupt(format!(
                "content address mismatch in {}: asked for {:?}, stored bytes hash to {:?}",
                self.pack_path.display(),
                id,
                actual
            )));
        }
        Ok(Some(bytes))
    }

    pub fn chunk_ids(&self) -> Vec<ChunkId> {
        (0..self.entries)
            .map(|i| {
                let base = 16 + i * INDEX_ENTRY_BYTES;
                let mut key = [0u8; 32];
                key.copy_from_slice(&self.index[base..base + 32]);
                ChunkId(key)
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries == 0
    }
}

/// The full chunk store: every pack in a directory.
pub struct Store {
    dir: PathBuf,
    packs: Vec<(u64, Pack)>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let mut ids: Vec<u64> = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".pack") {
                if let Ok(id) = stem.parse::<u64>() {
                    // A pack with no index is the residue of a crash between
                    // the two fsyncs. Skipping it is the recovery: the chunks
                    // are unreferenced, and whatever needed them was never
                    // committed either.
                    if dir.join(format!("{id:012}.idx")).exists() {
                        ids.push(id);
                    }
                }
            }
        }
        ids.sort_unstable();
        let mut packs = Vec::with_capacity(ids.len());
        for id in ids {
            match Pack::open(dir, id) {
                Ok(pack) => packs.push((id, pack)),
                // An index that will not parse is the residue of a crash during
                // its creation: the pack+index+dir-sync sequence is the atomic
                // unit, and an unparseable index means that unit never
                // completed. Treat it like a missing index and skip the pack,
                // rather than letting one torn file make the whole repository
                // unopenable. Bit-rot on a previously-good index is a different
                // failure that `verify` surfaces; `open` must stay available.
                Err(Error::Corrupt(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(Store {
            dir: dir.to_path_buf(),
            packs,
        })
    }

    pub fn next_pack_id(&self) -> u64 {
        self.packs.last().map(|(id, _)| id + 1).unwrap_or(0)
    }

    pub fn read(&self, id: ChunkId) -> Result<Option<Vec<u8>>> {
        // Newest pack first: a chunk written recently is the one most likely
        // to be read next, and duplicates across packs are byte-identical by
        // construction so either answer is correct. A corrupt copy in one pack
        // must NOT abort the read while an intact duplicate survives in an
        // older one, so a pack-level error is remembered and the search
        // continues; the error surfaces only if no pack yields an intact copy.
        let mut last_err: Option<Error> = None;
        for (_, pack) in self.packs.iter().rev() {
            match pack.read(id) {
                Ok(Some(bytes)) => return Ok(Some(bytes)),
                Ok(None) => {}
                Err(e) => last_err = Some(e),
            }
        }
        match last_err {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }

    pub fn contains(&self, id: ChunkId) -> bool {
        self.packs.iter().any(|(_, p)| p.locate(id).is_some())
    }

    pub fn write_pack(&mut self, writer: PackWriter) -> Result<usize> {
        if writer.is_empty() {
            return Ok(0);
        }
        let id = self.next_pack_id();
        let count = writer.chunk_count();
        writer.finish(&self.dir, id)?;
        self.packs.push((id, Pack::open(&self.dir, id)?));
        Ok(count)
    }

    pub fn chunk_count(&self) -> usize {
        self.packs.iter().map(|(_, p)| p.len()).sum()
    }

    pub fn pack_count(&self) -> usize {
        self.packs.len()
    }

    pub fn all_chunk_ids(&self) -> Vec<ChunkId> {
        let mut out: Vec<ChunkId> = self.packs.iter().flat_map(|(_, p)| p.chunk_ids()).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(seed: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(seed).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn round_trips_a_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = payload(7, 5000);
        let id = ChunkId::of(&bytes);

        let mut w = PackWriter::new();
        assert!(w.add(id, &bytes));
        store.write_pack(w).unwrap();

        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.read(id).unwrap().as_deref(), Some(&bytes[..]));
    }

    #[test]
    fn deduplicates_within_a_pack() {
        let mut w = PackWriter::new();
        let bytes = payload(3, 100);
        let id = ChunkId::of(&bytes);
        assert!(w.add(id, &bytes));
        assert!(
            !w.add(id, &bytes),
            "second add of the same address is a no-op"
        );
        assert_eq!(w.chunk_count(), 1);
    }

    #[test]
    fn spans_multiple_segments() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut w = PackWriter::new();
        let mut ids = Vec::new();
        // Enough payload to force several 4 MiB segments.
        for seed in 1..=40u8 {
            let bytes = payload(seed, 300_000);
            let id = ChunkId::of(&bytes);
            ids.push((id, bytes.clone()));
            w.add(id, &bytes);
        }
        store.write_pack(w).unwrap();

        let store = Store::open(dir.path()).unwrap();
        for (id, expected) in ids {
            assert_eq!(store.read(id).unwrap().as_deref(), Some(&expected[..]));
        }
    }

    #[test]
    fn missing_chunk_reads_as_none_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.read(ChunkId::of(b"never stored")).unwrap(), None);
    }

    #[test]
    fn a_pack_without_its_index_is_ignored_on_open() {
        // The exact residue a crash between the two fsyncs leaves behind.
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = payload(9, 4096);
        let mut w = PackWriter::new();
        w.add(ChunkId::of(&bytes), &bytes);
        store.write_pack(w).unwrap();

        fs::remove_file(dir.path().join("000000000000.idx")).unwrap();
        let recovered = Store::open(dir.path()).unwrap();
        assert_eq!(
            recovered.pack_count(),
            0,
            "an unindexed pack must not be loaded"
        );
        assert_eq!(recovered.read(ChunkId::of(&bytes)).unwrap(), None);
    }

    #[test]
    fn a_truncated_index_is_treated_as_crash_residue_not_a_brick() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = payload(9, 4096);
        let id = ChunkId::of(&bytes);
        let mut w = PackWriter::new();
        w.add(id, &bytes);
        store.write_pack(w).unwrap();

        // A torn index prefix — the residue of a crash mid-index-write.
        let idx = dir.path().join("000000000000.idx");
        let raw = fs::read(&idx).unwrap();
        fs::write(&idx, &raw[..12]).unwrap();

        // The store still opens; the torn pack is skipped, not bricked.
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.pack_count(), 0);
        assert_eq!(store.read(id).unwrap(), None);
    }

    #[test]
    fn a_garbage_index_count_reports_corrupt_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = payload(3, 4096);
        let mut w = PackWriter::new();
        w.add(ChunkId::of(&bytes), &bytes);
        store.write_pack(w).unwrap();

        // A count that would overflow entries*48 must not wrap and then panic.
        let idx = dir.path().join("000000000000.idx");
        let mut raw = fs::read(&idx).unwrap();
        raw[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&idx, &raw).unwrap();

        assert!(Pack::open(dir.path(), 0).is_err());
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.pack_count(), 0);
    }

    #[test]
    fn a_corrupt_pack_tail_reports_corrupt_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = payload(7, 9000);
        let mut w = PackWriter::new();
        w.add(ChunkId::of(&bytes), &bytes);
        store.write_pack(w).unwrap();

        // An absurd segment count in the tail must be rejected, not allocated.
        let pack = dir.path().join("000000000000.pack");
        let mut raw = fs::read(&pack).unwrap();
        let n = raw.len();
        raw[n - 16..n - 8].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&pack, &raw).unwrap();

        assert!(Pack::open(dir.path(), 0).is_err());
        // Store still opens (the pack is skipped as residue).
        let _ = Store::open(dir.path()).unwrap();
    }

    #[test]
    fn read_falls_back_to_an_intact_duplicate_in_an_older_pack() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = payload(11, 9000);
        let id = ChunkId::of(&bytes);

        // The same chunk in two packs — duplicates are byte-identical.
        let mut w0 = PackWriter::new();
        w0.add(id, &bytes);
        store.write_pack(w0).unwrap();
        let mut w1 = PackWriter::new();
        w1.add(id, &bytes);
        store.write_pack(w1).unwrap();

        // Corrupt the NEWER pack's payload; the older intact copy must recover.
        let newer = dir.path().join("000000000001.pack");
        let mut raw = fs::read(&newer).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 0xFF;
        fs::write(&newer, &raw).unwrap();

        let store = Store::open(dir.path()).unwrap();
        assert_eq!(
            store.read(id).unwrap().as_deref(),
            Some(&bytes[..]),
            "a corrupt newest copy must not hide an intact older one"
        );
    }

    #[test]
    fn corrupted_content_is_detected_not_returned() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = payload(5, 9000);
        let id = ChunkId::of(&bytes);
        let mut w = PackWriter::new();
        w.add(id, &bytes);
        store.write_pack(w).unwrap();

        // Flip a byte inside the compressed payload region.
        let pack = dir.path().join("000000000000.pack");
        let mut raw = fs::read(&pack).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 0xFF;
        fs::write(&pack, &raw).unwrap();

        let store = Store::open(dir.path()).unwrap();
        // Either the frame fails to decompress or the address check fires.
        // Both are errors; silently returning wrong bytes is not acceptable.
        assert!(
            store.read(id).is_err(),
            "corrupted content must not read back as success"
        );
    }
}
