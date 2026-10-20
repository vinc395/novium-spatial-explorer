//! Stable, reversible mapping between original Xenium string `cell_id`s and
//! compact `CellKey`s used on the GPU and in binary streams.
//!
//! * Keys are assigned by sorting the original IDs byte-wise (Rust `str` ordering)
//!   and numbering them `1..=N`. Key `0` is reserved for "no cell / unassigned".
//! * The mapping is scoped to one section; keys are never compared across sections.
//! * Duplicate or empty IDs are rejected (no hashing / truncation).

use std::collections::HashMap;
use thiserror::Error;

pub type CellKey = u32;
pub const NO_CELL: CellKey = 0;

#[derive(Debug, Error, PartialEq)]
pub enum CellIdError {
    #[error("duplicate cell_id {0:?}")]
    Duplicate(String),
    #[error("empty cell_id at input row {0}")]
    Empty(usize),
    #[error("too many cells ({0}) for 32-bit keys")]
    TooMany(usize),
    #[error("corrupt cell dictionary: {0}")]
    Corrupt(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CellDictionary {
    /// Sorted original IDs; `ids[key - 1]`.
    ids: Vec<String>,
}

const MAGIC: &[u8; 4] = b"XCI1";

impl CellDictionary {
    pub fn from_ids(ids: Vec<String>) -> Result<Self, CellIdError> {
        if let Some(i) = ids.iter().position(|s| s.is_empty()) {
            return Err(CellIdError::Empty(i));
        }
        if ids.len() >= u32::MAX as usize {
            return Err(CellIdError::TooMany(ids.len()));
        }
        let mut ids = ids;
        ids.sort_unstable();
        if let Some(w) = ids.windows(2).find(|w| w[0] == w[1]) {
            return Err(CellIdError::Duplicate(w[0].clone()));
        }
        Ok(Self { ids })
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// O(log N) lookup. For bulk joins use [`CellDictionary::lookup_map`].
    pub fn key_of(&self, id: &str) -> Option<CellKey> {
        self.ids
            .binary_search_by(|probe| probe.as_str().cmp(id))
            .ok()
            .map(|i| i as CellKey + 1)
    }

    pub fn id_of(&self, key: CellKey) -> Option<&str> {
        if key == NO_CELL {
            return None;
        }
        self.ids.get(key as usize - 1).map(String::as_str)
    }

    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// Hash map for O(1) bulk joins (e.g. hundreds of millions of transcripts).
    pub fn lookup_map(&self) -> HashMap<&str, CellKey> {
        self.ids
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i as CellKey + 1))
            .collect()
    }

    /// `XCI1 | u32 n | u32 offsets[n+1] | utf8 blob`, little-endian.
    pub fn to_bytes(&self) -> Vec<u8> {
        let blob_len: usize = self.ids.iter().map(String::len).sum();
        let mut out = Vec::with_capacity(8 + 4 * (self.ids.len() + 1) + blob_len);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.ids.len() as u32).to_le_bytes());
        let mut off = 0u32;
        out.extend_from_slice(&off.to_le_bytes());
        for s in &self.ids {
            off += s.len() as u32;
            out.extend_from_slice(&off.to_le_bytes());
        }
        for s in &self.ids {
            out.extend_from_slice(s.as_bytes());
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self, CellIdError> {
        if b.len() < 8 || &b[0..4] != MAGIC {
            return Err(CellIdError::Corrupt("bad magic"));
        }
        let n = u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize;
        let off_end = 8 + 4 * (n + 1);
        if b.len() < off_end {
            return Err(CellIdError::Corrupt("truncated offsets"));
        }
        let offs: Vec<usize> = b[8..off_end]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()) as usize)
            .collect();
        let blob = &b[off_end..];
        if offs[0] != 0
            || *offs.last().unwrap() != blob.len()
            || offs.windows(2).any(|w| w[0] > w[1])
        {
            return Err(CellIdError::Corrupt("bad offsets"));
        }
        let mut ids = Vec::with_capacity(n);
        for w in offs.windows(2) {
            let s =
                std::str::from_utf8(&blob[w[0]..w[1]]).map_err(|_| CellIdError::Corrupt("utf8"))?;
            ids.push(s.to_string());
        }
        if ids.windows(2).any(|w| w[0] >= w[1]) {
            return Err(CellIdError::Corrupt("ids not strictly sorted"));
        }
        Ok(Self { ids })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(ids: &[&str]) -> CellDictionary {
        CellDictionary::from_ids(ids.iter().map(|s| s.to_string()).collect()).unwrap()
    }

    #[test]
    fn keys_are_sorted_one_based_and_bijective() {
        let d = dict(&["ccc-1", "aaa-1", "bbb-1"]);
        assert_eq!(d.key_of("aaa-1"), Some(1));
        assert_eq!(d.key_of("ccc-1"), Some(3));
        assert_eq!(d.key_of("zzz"), None);
        assert_eq!(d.id_of(0), None);
        assert_eq!(d.id_of(2), Some("bbb-1"));
        assert_eq!(d.id_of(4), None);
        for k in 1..=3 {
            assert_eq!(d.key_of(d.id_of(k).unwrap()), Some(k));
        }
        let m = d.lookup_map();
        assert_eq!(m["bbb-1"], 2);
    }

    #[test]
    fn input_order_does_not_change_keys() {
        assert_eq!(dict(&["b", "a", "c"]), dict(&["c", "b", "a"]));
    }

    #[test]
    fn rejects_duplicates_and_empty() {
        assert_eq!(
            CellDictionary::from_ids(vec!["a".into(), "b".into(), "a".into()]),
            Err(CellIdError::Duplicate("a".into()))
        );
        assert_eq!(
            CellDictionary::from_ids(vec!["a".into(), "".into()]),
            Err(CellIdError::Empty(1))
        );
    }

    #[test]
    fn bytes_round_trip_and_corruption() {
        let d = dict(&["aaaaaaaa-1", "bbbbbbbb-1", "émoji-ok"]);
        let b = d.to_bytes();
        assert_eq!(CellDictionary::from_bytes(&b).unwrap(), d);
        assert!(CellDictionary::from_bytes(&b[..b.len() - 1]).is_err());
        let mut bad = b.clone();
        bad[0] = b'Y';
        assert!(CellDictionary::from_bytes(&bad).is_err());
        let empty = CellDictionary::from_ids(vec![]).unwrap();
        assert_eq!(
            CellDictionary::from_bytes(&empty.to_bytes()).unwrap(),
            empty
        );
    }
}
