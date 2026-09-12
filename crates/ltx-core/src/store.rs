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
    pub fn retain_unknown(&mut self, store: &Store) -> Result<()> {
        let mut kept = Vec::with_capacity(self.pending.len());
        for (id, payload) in std::mem::take(&mut self.pending) {
            if !store.contains(id)? {
                kept.push((id, payload));
            }
        }
        self.pending = kept;
        self.seen = self.pending.iter().map(|(id, _)| (*id, ())).collect();
        self.bytes = self.pending.iter().map(|(_, p)| p.len()).sum();
        Ok(())
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
/// The index cache: every pack's index bytes in ONE file beside the packs,
/// so a store with thousands of packs opens them with one read instead of
/// one `open` per pack — which, at ~36 µs each, was most of every command
/// that touched content once history held a few thousand packs.
///
/// Derived and disposable: the directory listing says which packs exist, the
/// cache only saves re-reading their indexes, and a pack the cache does not
/// know is read from its own index and the cache rewritten at the next write.
/// Append-only: a pack's record is appended when it is written, a tombstone
/// when it is removed, and a torn tail is where the reader stops. Its highest
/// id, removed or not, is also what keeps a pack id from ever being reused
/// while the cache exists — a reused id is the one way a cached index could
/// describe a pack it was not written for.
const INDEX_CACHE_FILE: &str = "index.cache";
const INDEX_CACHE_MAGIC: &[u8; 8] = b"LTXIDXC1";
const CACHE_RECORD_INDEX: u8 = 1;
const CACHE_RECORD_REMOVED: u8 = 2;
/// Names the highest pack id ever used without describing a pack: what a
/// rewrite writes so the mark survives when the highest pack is a removed one.
const CACHE_RECORD_HIGH_WATER: u8 = 3;
const CACHE_RECORD_HEADER_BYTES: usize = 8 + 1 + 4;

/// What the cache file says: the newest record per pack, the highest id it
/// has ever named, and whether its tail was torn (in which case the next
/// write rewrites it whole).
#[derive(Default)]
struct IndexCache {
    /// Pack id -> (the pack file's byte length when it was written, its
    /// index bytes). The length is checked against the file when the packs
    /// open: a pack that shrank is torn and is skipped, so the cache never
    /// vouches for a copy a save would deduplicate against and then fail to
    /// read.
    indexes: std::collections::HashMap<u64, (u64, Vec<u8>)>,
    highest_id: Option<u64>,
    torn: bool,
}

fn read_index_cache(dir: &Path) -> Result<IndexCache> {
    let bytes = match fs::read(dir.join(INDEX_CACHE_FILE)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(IndexCache::default()),
        Err(e) => return Err(e.into()),
    };
    let mut cache = IndexCache::default();
    if bytes.len() < INDEX_CACHE_MAGIC.len()
        || &bytes[..INDEX_CACHE_MAGIC.len()] != INDEX_CACHE_MAGIC
    {
        cache.torn = !bytes.is_empty();
        return Ok(cache);
    }
    let mut at = INDEX_CACHE_MAGIC.len();
    while at + CACHE_RECORD_HEADER_BYTES <= bytes.len() {
        let id = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let kind = bytes[at + 8];
        let len = u32::from_le_bytes(bytes[at + 9..at + 13].try_into().unwrap()) as usize;
        at += CACHE_RECORD_HEADER_BYTES;
        if at + len > bytes.len() {
            cache.torn = true;
            break;
        }
        match kind {
            CACHE_RECORD_INDEX => {
                if len < 8 {
                    cache.torn = true;
                    break;
                }
                let pack_len = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
                cache
                    .indexes
                    .insert(id, (pack_len, bytes[at + 8..at + len].to_vec()));
            }
            CACHE_RECORD_REMOVED => {
                cache.indexes.remove(&id);
            }
            CACHE_RECORD_HIGH_WATER => {}
            _ => {
                cache.torn = true;
                break;
            }
        }
        cache.highest_id = Some(cache.highest_id.map_or(id, |h| h.max(id)));
        at += len;
    }
    if at != bytes.len() {
        cache.torn = true;
    }
    Ok(cache)
}

fn index_record(id: u64, pack_len: u64, index: &[u8]) -> Vec<u8> {
    let mut payload = pack_len.to_le_bytes().to_vec();
    payload.extend_from_slice(index);
    cache_record(id, CACHE_RECORD_INDEX, &payload)
}

fn cache_record(id: u64, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut record = Vec::with_capacity(CACHE_RECORD_HEADER_BYTES + payload.len());
    record.extend_from_slice(&id.to_le_bytes());
    record.push(kind);
    record.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    record.extend_from_slice(payload);
    record
}

fn append_index_cache(dir: &Path, record: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(INDEX_CACHE_FILE))?;
    let created = file.metadata()?.len() == 0;
    if created {
        file.write_all(INDEX_CACHE_MAGIC)?;
    }
    file.write_all(record)?;
    // Durable like every other store write: the cache carries the retired
    // ids that keep a pack id from being reused, so a record it acknowledged
    // must survive a power loss.
    file.sync_data()?;
    if created {
        sync_dir(dir)?;
    }
    Ok(())
}

/// Replace the cache with one record per pack given, via a temporary file
/// and a rename: a crash leaves the old cache or the new one, never a torn
/// middle.
fn rewrite_index_cache(dir: &Path, packs: &[(u64, Pack)], high_water: Option<u64>) -> Result<()> {
    let mut bytes = INDEX_CACHE_MAGIC.to_vec();
    for (id, pack) in packs {
        bytes.extend_from_slice(&index_record(*id, pack.bytes, &pack.index));
    }
    // The highest id ever used may belong to a removed pack, which the
    // records above no longer name; without this it would be reused.
    if let Some(high_water) = high_water {
        bytes.extend_from_slice(&cache_record(high_water, CACHE_RECORD_HIGH_WATER, &[]));
    }
    let temporary = dir.join(format!("{INDEX_CACHE_FILE}.rewrite"));
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, dir.join(INDEX_CACHE_FILE))?;
    sync_dir(dir)?;
    Ok(())
}

pub struct Pack {
    pack_path: PathBuf,
    /// The pack file's byte length when it was written.
    bytes: u64,
    index: Vec<u8>,
    entries: usize,
    /// The segment table from the pack's tail, read the first time a chunk
    /// is read from this pack. Opening reads only the index: `locate` and
    /// `contains` never need the table, and a store with thousands of packs
    /// opens every one of them before answering whether a chunk exists.
    segments: std::sync::OnceLock<Vec<(u64, u32)>>,
}

impl Pack {
    /// Open a pack from its own files. The segment table at the pack's tail
    /// is read here, so a torn pack file is found now and skipped, rather
    /// than trusted by `contains` and failed by `read`.
    pub fn open(dir: &Path, pack_id: u64) -> Result<Self> {
        let index_path = dir.join(format!("{pack_id:012}.idx"));
        let mut index = Vec::new();
        File::open(&index_path)?.read_to_end(&mut index)?;
        let bytes = fs::metadata(dir.join(format!("{pack_id:012}.pack")))?.len();
        let pack = Self::from_index(dir, pack_id, index, bytes)?;
        pack.segments()?;
        Ok(pack)
    }

    /// A pack from the cache's copy of its index. The file is not opened;
    /// its length is checked against what the cache recorded, which is what
    /// tells a torn pack from a whole one without reading it.
    fn from_cache(dir: &Path, pack_id: u64, index: Vec<u8>, bytes: u64) -> Result<Self> {
        let actual = fs::metadata(dir.join(format!("{pack_id:012}.pack")))?.len();
        if actual != bytes {
            return Err(Error::Corrupt(format!(
                "pack {pack_id} is {actual} bytes but {bytes} were written"
            )));
        }
        Self::from_index(dir, pack_id, index, bytes)
    }

    /// A pack from index bytes already in hand, validated the same way
    /// whichever way they arrived.
    pub fn from_index(dir: &Path, pack_id: u64, index: Vec<u8>, bytes: u64) -> Result<Self> {
        let pack_path = dir.join(format!("{pack_id:012}.pack"));
        let index_path = dir.join(format!("{pack_id:012}.idx"));
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

        Ok(Pack {
            pack_path,
            bytes,
            index,
            entries,
            segments: std::sync::OnceLock::new(),
        })
    }

    /// The segment table, read and validated on first use.
    fn segments(&self) -> Result<&[(u64, u32)]> {
        if let Some(segments) = self.segments.get() {
            return Ok(segments);
        }
        let pack_path = &self.pack_path;
        let mut file = File::open(pack_path)?;
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

        Ok(self.segments.get_or_init(|| segments))
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
        let segments = self.segments()?;
        let Some(&(seg_off, seg_len)) = segments.get(loc.segment as usize) else {
            return Err(Error::Corrupt(format!(
                "{} references segment {} of {}",
                self.pack_path.display(),
                loc.segment,
                segments.len()
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
///
/// Opening lists the directory and nothing more. The packs themselves — every
/// index, read whole — are opened the first time a chunk is looked up, because
/// most commands never look one up: status, log, assign, split, a lens change,
/// a dry-run sync, compaction and the undo of a save all finish without
/// touching content. Reading every index on every command cost the size of
/// history each time — 24 ms of a 59 ms `workspace list` at 781 packs, and at
/// the thousands of packs a long history holds, most of every command.
pub struct Store {
    dir: PathBuf,
    /// Every pack that has an index, ascending: what `open` reads.
    ids: Vec<u64>,
    /// The packs, once something needed them. A `OnceLock` so that a lookup
    /// through `&self` can be the thing that opens them.
    packs: std::sync::OnceLock<Vec<(u64, Pack)>>,
    /// Every chunk in the store and the newest pack that holds it, built from
    /// the indexes once they are open. `contains` and `read` answer from it
    /// instead of searching every pack: a save asks about every file in the
    /// working tree, and searching thousands of packs for each of thousands
    /// of files was the cost that grew fastest with history. Dropped whenever
    /// a pack is removed, and rebuilt on the next lookup, so it is never
    /// stale — a stale "present" would make a save skip storing a chunk that
    /// is gone.
    located: std::sync::OnceLock<std::collections::HashMap<ChunkId, u64>>,
    /// The index cache, read on first use.
    cache: std::sync::OnceLock<IndexCache>,
    /// Set when the packs were opened and the cache did not cover every one
    /// of them, or was torn: the next write rewrites it whole.
    cache_incomplete: std::sync::atomic::AtomicBool,
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
        Ok(Store {
            dir: dir.to_path_buf(),
            ids,
            packs: std::sync::OnceLock::new(),
            located: std::sync::OnceLock::new(),
            cache: std::sync::OnceLock::new(),
            cache_incomplete: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn cache(&self) -> Result<&IndexCache> {
        if let Some(cache) = self.cache.get() {
            return Ok(cache);
        }
        let cache = read_index_cache(&self.dir)?;
        Ok(self.cache.get_or_init(|| cache))
    }

    /// Chunk -> the newest pack holding it, built on first use.
    fn located(&self) -> Result<&std::collections::HashMap<ChunkId, u64>> {
        if let Some(located) = self.located.get() {
            return Ok(located);
        }
        let mut located = std::collections::HashMap::new();
        // Ascending pack order, so a chunk held twice ends up mapped to the
        // newest copy — the one `read` preferred when it searched.
        for (id, pack) in self.packs()? {
            for chunk in pack.chunk_ids() {
                located.insert(chunk, *id);
            }
        }
        Ok(self.located.get_or_init(|| located))
    }

    fn pack(&self, id: u64) -> Result<Option<&Pack>> {
        let packs = self.packs()?;
        Ok(packs
            .binary_search_by_key(&id, |(pid, _)| *pid)
            .ok()
            .map(|position| &packs[position].1))
    }

    /// The packs, opened on first use.
    fn packs(&self) -> Result<&[(u64, Pack)]> {
        if let Some(packs) = self.packs.get() {
            return Ok(packs);
        }
        let cache = self.cache()?;
        let mut incomplete = cache.torn;
        let mut packs = Vec::with_capacity(self.ids.len());
        for &id in &self.ids {
            let opened = match cache.indexes.get(&id) {
                Some((bytes, index)) => Pack::from_cache(&self.dir, id, index.clone(), *bytes)
                    .or_else(|_| Pack::open(&self.dir, id)),
                None => {
                    incomplete = true;
                    Pack::open(&self.dir, id)
                }
            };
            match opened {
                Ok(pack) => packs.push((id, pack)),
                // An index that will not parse is the residue of a crash during
                // its creation: the pack+index+dir-sync sequence is the atomic
                // unit, and an unparseable index means that unit never
                // completed. Treat it like a missing index and skip the pack,
                // rather than letting one torn file make the whole repository
                // unopenable. Bit-rot on a previously-good index is a different
                // failure that `verify` surfaces; the store must stay usable.
                Err(Error::Corrupt(_)) => {
                    incomplete = true;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        if incomplete {
            self.cache_incomplete
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // Another thread may have opened them meanwhile; both read the same
        // files, so either answer serves.
        Ok(self.packs.get_or_init(|| packs))
    }

    /// One past the highest id in the directory OR in the cache — removed
    /// packs included, so an id is never reused while the cache remembers
    /// it. `thin` also leans on ids never going backwards.
    pub fn next_pack_id(&self) -> Result<u64> {
        let on_disk = self.ids.last().copied();
        let remembered = self.cache()?.highest_id;
        match on_disk.max(remembered) {
            Some(id) => id
                .checked_add(1)
                .ok_or_else(|| Error::Corrupt("the pack id space is exhausted".to_string())),
            None => Ok(0),
        }
    }

    pub fn read(&self, id: ChunkId) -> Result<Option<Vec<u8>>> {
        // Newest pack first: a chunk written recently is the one most likely
        // to be read next, and duplicates across packs are byte-identical by
        // construction so either answer is correct. A corrupt copy in one pack
        // must NOT abort the read while an intact duplicate survives in an
        // older one, so a pack-level error is remembered and the search
        // continues; the error surfaces only if no pack yields an intact copy.
        let Some(&newest) = self.located()?.get(&id) else {
            return Ok(None);
        };
        let mut last_err: Option<Error> = None;
        if let Some(pack) = self.pack(newest)? {
            match pack.read(id) {
                Ok(Some(bytes)) => return Ok(Some(bytes)),
                Ok(None) => {}
                Err(e) => last_err = Some(e),
            }
        }
        for (pid, pack) in self.packs()?.iter().rev() {
            if *pid == newest {
                continue;
            }
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

    pub fn contains(&self, id: ChunkId) -> Result<bool> {
        Ok(self.located()?.contains_key(&id))
    }

    pub fn write_pack(&mut self, writer: PackWriter) -> Result<usize> {
        if writer.is_empty() {
            return Ok(0);
        }
        let id = self.next_pack_id()?;
        let count = writer.chunk_count();
        writer.finish(&self.dir, id)?;
        self.ids.push(id);
        let index = fs::read(self.dir.join(format!("{id:012}.idx")))?;
        let bytes = fs::metadata(self.dir.join(format!("{id:012}.pack")))?.len();
        // Joins the opened packs only if they are open: writing one is not a
        // reason to read every other. The map, likewise, learns the new
        // chunks only if it exists — it is the newest pack, so it wins.
        if let Some(packs) = self.packs.get_mut() {
            let pack = Pack::from_index(&self.dir, id, index.clone(), bytes)?;
            if let Some(located) = self.located.get_mut() {
                for chunk in pack.chunk_ids() {
                    located.insert(chunk, id);
                }
            }
            packs.push((id, pack));
        }
        self.remember(id, Some((bytes, &index)))?;
        Ok(count)
    }

    /// Put one record in the cache — appended, or, when the packs were
    /// opened with the cache incomplete, by rewriting the cache whole from
    /// the packs now open. Either way the in-memory copy is kept in step.
    fn remember(&mut self, id: u64, entry: Option<(u64, &[u8])>) -> Result<()> {
        let incomplete = self
            .cache_incomplete
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        // Never below anything the cache or the directory has named, nor the
        // id being recorded — which, for a removal, is no longer on disk.
        let high_water = self
            .cache()?
            .highest_id
            .max(self.ids.last().copied())
            .max(Some(id));
        if incomplete && self.packs.get().is_some() {
            let packs = self.packs.get().expect("checked");
            rewrite_index_cache(&self.dir, packs, high_water)?;
            let mut rebuilt = IndexCache::default();
            for (pid, pack) in packs {
                rebuilt
                    .indexes
                    .insert(*pid, (pack.bytes, pack.index.clone()));
            }
            rebuilt.highest_id = high_water;
            self.cache.take();
            let _ = self.cache.set(rebuilt);
            return Ok(());
        }
        let record = match entry {
            Some((bytes, index)) => index_record(id, bytes, index),
            None => cache_record(id, CACHE_RECORD_REMOVED, &[]),
        };
        append_index_cache(&self.dir, &record)?;
        let mut cache = self.cache.take().unwrap_or_default();
        match entry {
            Some((bytes, index)) => {
                cache.indexes.insert(id, (bytes, index.to_vec()));
            }
            None => {
                cache.indexes.remove(&id);
            }
        }
        cache.highest_id = high_water;
        let _ = self.cache.set(cache);
        Ok(())
    }

    pub fn chunk_count(&self) -> Result<usize> {
        Ok(self.packs()?.iter().map(|(_, p)| p.len()).sum())
    }

    pub fn pack_count(&self) -> Result<usize> {
        Ok(self.packs()?.len())
    }

    /// Every pack, with the chunks it holds. The unit `thin` reasons about:
    /// a pack is removed whole or not at all.
    pub fn packs_with_chunks(&self) -> Result<Vec<(u64, Vec<ChunkId>)>> {
        Ok(self
            .packs()?
            .iter()
            .map(|(id, pack)| (*id, pack.chunk_ids()))
            .collect())
    }

    /// Remove one pack and its index, durably.
    ///
    /// The index goes first. A crash between the two leaves a pack with no
    /// index, which `open` already treats as the residue of an interrupted
    /// write and skips — so the store never opens onto a half-removed pack
    /// that reads as intact. The reverse order would leave an index pointing
    /// at bytes that are gone.
    pub fn remove_pack(&mut self, id: u64) -> Result<()> {
        let Some(position) = self.ids.iter().position(|pid| *pid == id) else {
            return Err(Error::NotFound(format!("pack {id} is not in this store")));
        };
        self.ids.remove(position);
        if let Some(packs) = self.packs.get_mut() {
            packs.retain(|(pid, _)| *pid != id);
        }
        // A chunk this pack held may survive in an older one or in none; the
        // map cannot know which without a rebuild, so it is dropped here and
        // rebuilt on the next lookup.
        self.located.take();
        self.remember(id, None)?;
        fs::remove_file(self.dir.join(format!("{id:012}.idx")))?;
        fs::remove_file(self.dir.join(format!("{id:012}.pack")))?;
        crate::platform::sync_dir(&self.dir)?;
        Ok(())
    }

    /// Rewrite one pack without the chunks in `doomed`, durably, and remove
    /// the original. Returns how many chunks were dropped.
    ///
    /// The replacement is written and made durable as a NEW pack before the
    /// original goes, so a crash at any point leaves every kept chunk readable
    /// from one pack or the other — duplicates across packs are byte-identical
    /// by construction and either copy serves. Only redaction calls this; it
    /// is the one operation that removes content something still names.
    ///
    /// The new pack takes a fresh id, which breaks the ordering `thin` leans
    /// on — that a chunk in pack P was not in the store before P. A redacted
    /// store therefore makes thin walk all of history rather than a bounded
    /// slice; `Repo::thin` checks the redaction ledger for exactly that.
    pub fn rewrite_pack_without(
        &mut self,
        id: u64,
        doomed: &std::collections::HashSet<ChunkId>,
    ) -> Result<usize> {
        let chunks = {
            let Some((_, pack)) = self.packs()?.iter().find(|(pid, _)| *pid == id) else {
                return Err(Error::NotFound(format!("pack {id} is not in this store")));
            };
            pack.chunk_ids()
        };
        let mut kept = PackWriter::new();
        let mut dropped = 0usize;
        for chunk in chunks {
            if doomed.contains(&chunk) {
                dropped += 1;
                continue;
            }
            // Read through the pack itself, not the store: the store would
            // happily answer from a duplicate elsewhere, and this pack's own
            // copy is what must be carried over.
            let bytes = {
                let (_, pack) = self
                    .packs()?
                    .iter()
                    .find(|(pid, _)| *pid == id)
                    .expect("found above and nothing removed it since");
                pack.read(chunk)?
            };
            let Some(bytes) = bytes else {
                return Err(Error::Corrupt(format!(
                    "pack {id} indexes {chunk:?} but cannot read it"
                )));
            };
            kept.add(chunk, &bytes);
        }
        if !kept.is_empty() {
            self.write_pack(kept)?;
        }
        self.remove_pack(id)?;
        Ok(dropped)
    }

    pub fn all_chunk_ids(&self) -> Result<Vec<ChunkId>> {
        let mut out: Vec<ChunkId> = self
            .packs()?
            .iter()
            .flat_map(|(_, p)| p.chunk_ids())
            .collect();
        out.sort_unstable();
        out.dedup();
        Ok(out)
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
    fn a_pack_written_before_the_store_has_opened_its_packs_is_still_readable() {
        // Opening reads no pack; a write must not change that, and the first
        // lookup afterwards has to see what was written as well as what was
        // already there.
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut first = PackWriter::new();
        first.add(ChunkId::of(b"old"), b"old");
        store.write_pack(first).unwrap();

        let mut store = Store::open(dir.path()).unwrap();
        let mut second = PackWriter::new();
        second.add(ChunkId::of(b"new"), b"new");
        store.write_pack(second).unwrap();

        assert_eq!(
            store.read(ChunkId::of(b"old")).unwrap(),
            Some(b"old".to_vec())
        );
        assert_eq!(
            store.read(ChunkId::of(b"new")).unwrap(),
            Some(b"new".to_vec())
        );
        assert!(store.contains(ChunkId::of(b"new")).unwrap());
        assert_eq!(store.pack_count().unwrap(), 2);
        assert_eq!(store.chunk_count().unwrap(), 2);
    }

    #[test]
    fn removing_a_pack_forgets_its_chunks_and_still_finds_older_copies() {
        // The chunk map must be exact after a removal: a chunk the removed
        // pack held alone is gone, one an older pack also holds is still
        // found there. A stale "present" would make a save skip storing it.
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut older = PackWriter::new();
        older.add(ChunkId::of(b"shared"), b"shared");
        store.write_pack(older).unwrap();
        let mut newer = PackWriter::new();
        newer.add(ChunkId::of(b"shared"), b"shared");
        newer.add(ChunkId::of(b"only newer"), b"only newer");
        store.write_pack(newer).unwrap();
        assert!(
            store.contains(ChunkId::of(b"only newer")).unwrap(),
            "premise"
        );

        store.remove_pack(1).unwrap();

        assert!(!store.contains(ChunkId::of(b"only newer")).unwrap());
        assert_eq!(store.read(ChunkId::of(b"only newer")).unwrap(), None);
        assert!(store.contains(ChunkId::of(b"shared")).unwrap());
        assert_eq!(
            store.read(ChunkId::of(b"shared")).unwrap(),
            Some(b"shared".to_vec())
        );
    }

    #[test]
    fn a_reopened_store_reads_its_packs_from_the_cache() {
        // With the cache written, the index files are not what a lookup
        // reads: tear one and the chunk is still found and served.
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        for word in [b"one".as_slice(), b"two".as_slice()] {
            let mut w = PackWriter::new();
            w.add(ChunkId::of(word), word);
            store.write_pack(w).unwrap();
        }
        assert!(dir.path().join(INDEX_CACHE_FILE).exists(), "premise");
        let idx = dir.path().join("000000000001.idx");
        let raw = fs::read(&idx).unwrap();
        fs::write(&idx, &raw[..12]).unwrap();

        let store = Store::open(dir.path()).unwrap();

        assert!(store.contains(ChunkId::of(b"two")).unwrap());
        assert_eq!(
            store.read(ChunkId::of(b"two")).unwrap(),
            Some(b"two".to_vec())
        );
        assert_eq!(store.pack_count().unwrap(), 2);
    }

    #[test]
    fn a_removed_pack_is_forgotten_by_the_cache_and_its_id_is_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        for word in [b"keep".as_slice(), b"drop".as_slice()] {
            let mut w = PackWriter::new();
            w.add(ChunkId::of(word), word);
            store.write_pack(w).unwrap();
        }
        store.remove_pack(1).unwrap();

        let mut store = Store::open(dir.path()).unwrap();
        assert!(!store.contains(ChunkId::of(b"drop")).unwrap());
        let mut w = PackWriter::new();
        w.add(ChunkId::of(b"later"), b"later");
        store.write_pack(w).unwrap();

        assert!(
            dir.path().join("000000000002.pack").exists()
                && !dir.path().join("000000000001.pack").exists(),
            "the removed id 1 stays retired; the new pack is 2"
        );
        assert_eq!(
            store.read(ChunkId::of(b"later")).unwrap(),
            Some(b"later".to_vec())
        );
    }

    #[test]
    fn a_torn_cache_is_read_up_to_the_tear_and_rewritten_whole_by_the_next_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut w = PackWriter::new();
        w.add(ChunkId::of(b"first"), b"first");
        store.write_pack(w).unwrap();
        let cache = dir.path().join(INDEX_CACHE_FILE);
        let mut raw = fs::read(&cache).unwrap();
        raw.extend_from_slice(&[9u8; 7]);
        fs::write(&cache, &raw).unwrap();

        let mut store = Store::open(dir.path()).unwrap();
        assert_eq!(
            store.read(ChunkId::of(b"first")).unwrap(),
            Some(b"first".to_vec())
        );
        let mut w = PackWriter::new();
        w.add(ChunkId::of(b"second"), b"second");
        store.write_pack(w).unwrap();

        let rebuilt = read_index_cache(dir.path()).unwrap();
        assert!(!rebuilt.torn, "the write rewrote the cache whole");
        assert_eq!(rebuilt.indexes.len(), 2);
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(
            store.read(ChunkId::of(b"second")).unwrap(),
            Some(b"second".to_vec())
        );
    }

    #[test]
    fn a_pack_file_shorter_than_the_cache_recorded_is_skipped_and_its_chunk_is_stored_again() {
        // A torn pack must not be vouched for: `contains` would let a save
        // deduplicate against a copy that cannot be read.
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = payload(5, 9000);
        let id = ChunkId::of(&bytes);
        let mut w = PackWriter::new();
        w.add(id, &bytes);
        store.write_pack(w).unwrap();
        let pack = dir.path().join("000000000000.pack");
        let raw = fs::read(&pack).unwrap();
        fs::write(&pack, &raw[..raw.len() - 10]).unwrap();

        let store = Store::open(dir.path()).unwrap();
        assert!(!store.contains(id).unwrap());
        assert_eq!(store.read(id).unwrap(), None);
        assert_eq!(store.pack_count().unwrap(), 0);
        let mut again = PackWriter::new();
        again.add(id, &bytes);
        again.retain_unknown(&store).unwrap();
        assert_eq!(again.chunk_count(), 1, "a save stores the chunk again");

        // Without the cache the pack's own files say the same.
        fs::remove_file(dir.path().join(INDEX_CACHE_FILE)).unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert!(!store.contains(id).unwrap());
    }

    #[test]
    fn a_rewrite_keeps_the_high_water_mark_of_a_removed_pack() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        for word in [b"a".as_slice(), b"b".as_slice()] {
            let mut w = PackWriter::new();
            w.add(ChunkId::of(word), word);
            store.write_pack(w).unwrap();
        }
        let cache = dir.path().join(INDEX_CACHE_FILE);
        let mut raw = fs::read(&cache).unwrap();
        raw.extend_from_slice(&[9u8; 5]);
        fs::write(&cache, &raw).unwrap();

        // Opened with the cache torn, the removal rewrites it; the highest
        // id is the pack being removed and must survive the rewrite.
        let mut store = Store::open(dir.path()).unwrap();
        assert!(store.contains(ChunkId::of(b"b")).unwrap(), "premise");
        store.remove_pack(1).unwrap();
        assert!(!read_index_cache(dir.path()).unwrap().torn);

        let mut store = Store::open(dir.path()).unwrap();
        let mut w = PackWriter::new();
        w.add(ChunkId::of(b"c"), b"c");
        store.write_pack(w).unwrap();
        assert!(
            dir.path().join("000000000002.pack").exists()
                && !dir.path().join("000000000001.pack").exists(),
            "id 1 stays retired across the rewrite"
        );
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
            recovered.pack_count().unwrap(),
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

        // Torn after the write completed, the cache still holds the index
        // it was written with, so the pack stays readable.
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.pack_count().unwrap(), 1);
        assert_eq!(store.read(id).unwrap(), Some(bytes.clone()));

        // Torn during the write — before the cache could record it — it is
        // the residue of a crash: skipped, not a brick.
        fs::remove_file(dir.path().join(INDEX_CACHE_FILE)).unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.pack_count().unwrap(), 0);
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
        fs::remove_file(dir.path().join(INDEX_CACHE_FILE)).unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.pack_count().unwrap(), 0);
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

        assert!(
            Pack::open(dir.path(), 0).is_err(),
            "opening from the files reads the tail"
        );
        // The cache vouches for the index and the file is its recorded
        // length, so the store still lists the pack; the corruption is
        // reported by the read that first needs the tail.
        let store = Store::open(dir.path()).unwrap();
        assert!(store.read(ChunkId::of(&bytes)).is_err());
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
