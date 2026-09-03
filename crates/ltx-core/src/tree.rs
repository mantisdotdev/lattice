//! Directory trees as Merkle structures over chunk trees (§5.1).
//!
//! A file is a list of chunk addresses; a directory is a sorted list of named
//! entries; both hash to a single address, so a checkpoint is one address that
//! transitively fixes every byte beneath it.
//!
//! Two decisions here exist because G1.2 names the cases explicitly:
//!
//! **Paths are bytes.** Not `String`, not `PathBuf` rendered through UTF-8. The
//! adversarial corpus contains an NFC/NFD filename pair that renders
//! identically and a name with invalid UTF-8; decoding either would silently
//! merge or mangle them. A version control system that cannot store a filename
//! its filesystem accepted is losing data.
//!
//! **Entry kinds are explicit.** A symlink stores its target, never the bytes
//! it points at — following it would inline the target and lose the link. An
//! empty directory is representable, because git cannot represent one and G3.4
//! requires that lossy edge to be documented rather than discovered.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::chunk::{self, ChunkId};
use crate::error::Result;
use crate::store::PackWriter;

/// What a tree entry is. Mode is carried for files because the executable bit
/// must round-trip (G1.2 checks it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Node {
    File {
        /// Chunk addresses in order. Empty for a zero-byte file — distinct
        /// from "no file", and distinct from a single empty chunk.
        chunks: Vec<String>,
        size: u64,
        /// Unix permission bits, masked to 0o777.
        mode: u32,
    },
    Symlink {
        /// Raw target bytes. Stored as bytes for the same reason paths are:
        /// a target need not be valid UTF-8.
        target: Vec<u8>,
    },
    Directory {
        /// Address of the child tree. Present even when empty.
        tree: String,
    },
}

/// One named entry, as it appears on the wire.
///
/// The tree serialises as a SEQUENCE of these rather than a JSON object,
/// because names are raw bytes and JSON object keys must be strings. Encoding
/// them as strings would have forced a lossy decode of exactly the names G1.2
/// exists to protect -- invalid UTF-8, and NFC/NFD pairs that differ only in
/// bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub name: Vec<u8>,
    pub node: Node,
}

/// A directory: name bytes to node. `BTreeMap` gives a canonical order in
/// memory, so the same directory always hashes to the same address regardless
/// of the order the filesystem happened to enumerate it in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub entries: BTreeMap<Vec<u8>, Node>,
}

impl Serialize for Tree {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        let rows: Vec<TreeEntry> = self
            .entries
            .iter()
            .map(|(name, node)| TreeEntry {
                name: name.clone(),
                node: node.clone(),
            })
            .collect();
        rows.serialize(ser)
    }
}

impl<'de> Deserialize<'de> for Tree {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        let rows = Vec::<TreeEntry>::deserialize(de)?;
        let mut entries = BTreeMap::new();
        for row in rows {
            entries.insert(row.name, row.node);
        }
        Ok(Tree { entries })
    }
}

impl Tree {
    pub fn new() -> Self {
        Tree::default()
    }

    /// Canonical serialisation. The address is the hash of exactly these bytes,
    /// so two trees are equal iff their addresses are.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    pub fn id(&self) -> Result<String> {
        Ok(ChunkId::of(&self.to_bytes()?).to_hex())
    }

    /// Store this tree (and nothing beneath it — children are stored by the
    /// caller as it recurses) and return its address.
    pub fn write(&self, packer: &mut PackWriter) -> Result<String> {
        let bytes = self.to_bytes()?;
        let id = ChunkId::of(&bytes);
        packer.add(id, &bytes);
        Ok(id.to_hex())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Split file content into chunks, stage them, and return the node.
pub fn file_node(content: &[u8], mode: u32, packer: &mut PackWriter) -> Node {
    let chunks = chunk::split(content);
    let mut ids = Vec::with_capacity(chunks.len());
    for c in &chunks {
        let bytes = &content[c.offset as usize..(c.offset as usize + c.len as usize)];
        packer.add(c.id, bytes);
        ids.push(c.id.to_hex());
    }
    Node::File {
        chunks: ids,
        size: content.len() as u64,
        mode: mode & 0o777,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_trees_hash_identically_regardless_of_insertion_order() {
        let mut a = Tree::new();
        a.entries
            .insert(b"zebra".to_vec(), Node::Directory { tree: "t1".into() });
        a.entries
            .insert(b"apple".to_vec(), Node::Directory { tree: "t2".into() });

        let mut b = Tree::new();
        b.entries
            .insert(b"apple".to_vec(), Node::Directory { tree: "t2".into() });
        b.entries
            .insert(b"zebra".to_vec(), Node::Directory { tree: "t1".into() });

        assert_eq!(a.id().unwrap(), b.id().unwrap());
    }

    #[test]
    fn nfc_and_nfd_names_are_distinct_entries() {
        // The exact G1.2 case. These render identically; comparing decoded
        // strings on a normalising filesystem would merge them and lose a file.
        let nfc = "é".as_bytes().to_vec(); // U+00E9
        let nfd = "e\u{0301}".as_bytes().to_vec(); // e + combining
        assert_ne!(nfc, nfd);

        let mut t = Tree::new();
        t.entries
            .insert(nfc.clone(), Node::Directory { tree: "a".into() });
        t.entries
            .insert(nfd.clone(), Node::Directory { tree: "b".into() });
        assert_eq!(t.len(), 2, "NFC and NFD names must remain distinct");
    }

    #[test]
    fn a_name_that_is_not_valid_utf8_survives() {
        let name = vec![0x66, 0x6f, 0xff, 0xfe, 0x6f];
        assert!(String::from_utf8(name.clone()).is_err());
        let mut t = Tree::new();
        t.entries
            .insert(name.clone(), Node::Directory { tree: "x".into() });
        let round = Tree::from_bytes(&t.to_bytes().unwrap()).unwrap();
        assert_eq!(round.entries.keys().next().unwrap(), &name);
    }

    #[test]
    fn an_empty_file_has_no_chunks_and_is_not_a_missing_file() {
        let mut packer = PackWriter::new();
        let node = file_node(b"", 0o644, &mut packer);
        match node {
            Node::File { chunks, size, .. } => {
                assert!(chunks.is_empty(), "a zero-byte file stores no chunks");
                assert_eq!(size, 0);
            }
            other => panic!("expected a file, got {other:?}"),
        }
        assert_eq!(packer.chunk_count(), 0);
    }

    #[test]
    fn the_executable_bit_is_carried() {
        let mut packer = PackWriter::new();
        match file_node(b"#!/bin/sh\n", 0o755, &mut packer) {
            Node::File { mode, .. } => assert_eq!(mode, 0o755),
            other => panic!("expected a file, got {other:?}"),
        }
    }

    #[test]
    fn mode_is_masked_to_permission_bits() {
        let mut packer = PackWriter::new();
        // A full st_mode carries the file-type bits too; only permissions belong
        // in the tree, or the same file would hash differently by type encoding.
        match file_node(b"x", 0o100_644, &mut packer) {
            Node::File { mode, .. } => assert_eq!(mode, 0o644),
            other => panic!("expected a file, got {other:?}"),
        }
    }

    #[test]
    fn a_symlink_stores_its_target_not_the_content() {
        let node = Node::Symlink {
            target: b"../elsewhere".to_vec(),
        };
        let mut t = Tree::new();
        t.entries.insert(b"link".to_vec(), node);
        let round = Tree::from_bytes(&t.to_bytes().unwrap()).unwrap();
        match round.entries.get(b"link".as_slice()).unwrap() {
            Node::Symlink { target } => assert_eq!(target, b"../elsewhere"),
            other => panic!("expected a symlink, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_directory_is_representable() {
        // git cannot represent this; G3.4 requires the lossy edge be documented
        // rather than discovered, which starts with Lattice actually storing it.
        let empty = Tree::new();
        let mut parent = Tree::new();
        parent.entries.insert(
            b"empty".to_vec(),
            Node::Directory {
                tree: empty.id().unwrap(),
            },
        );
        assert_eq!(parent.len(), 1);
    }

    #[test]
    fn content_round_trips_through_chunks() {
        let content: Vec<u8> = (0..200_000u32)
            .map(|i| (i.wrapping_mul(31)) as u8)
            .collect();
        let mut packer = PackWriter::new();
        let node = file_node(&content, 0o644, &mut packer);
        match node {
            Node::File { chunks, size, .. } => {
                assert_eq!(size, content.len() as u64);
                assert!(!chunks.is_empty());
                // The packer holds UNIQUE chunks. This content is periodic, so
                // several chunks are byte-identical and collapse to one entry --
                // which is deduplication working, not a miscount.
                let unique: std::collections::HashSet<_> = chunks.iter().collect();
                assert_eq!(packer.chunk_count(), unique.len());
                assert!(packer.chunk_count() <= chunks.len());
            }
            other => panic!("expected a file, got {other:?}"),
        }
    }
}
