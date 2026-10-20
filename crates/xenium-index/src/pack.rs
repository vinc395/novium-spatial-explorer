//! A "pack" is a blob file plus an index of `(key, offset, len)` entries
//! (`XIX1 | u32 n | n x (u32 key, u64 offset, u32 len)`), used for polygon
//! chunks and similar keyed payloads. Readers use positional reads, so a pack
//! can be shared between threads without locking.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

pub struct PackWriter {
    blob: BufWriter<File>,
    idx_path: PathBuf,
    entries: Vec<(u32, u64, u32)>,
    offset: u64,
}

impl PackWriter {
    pub fn create(base: &Path) -> Result<Self> {
        let blob = File::create(base.with_extension("pack"))
            .with_context(|| format!("create {}", base.display()))?;
        Ok(PackWriter {
            blob: BufWriter::with_capacity(1 << 20, blob),
            idx_path: base.with_extension("idx"),
            entries: vec![],
            offset: 0,
        })
    }

    /// Keys must be added in strictly increasing order.
    pub fn add(&mut self, key: u32, data: &[u8]) -> Result<()> {
        if let Some(&(last, _, _)) = self.entries.last() {
            if key <= last {
                bail!("pack keys must increase ({key} after {last})");
            }
        }
        self.insert(key, data)
    }

    /// Like [`add`](Self::add), in any key order: [`finish`](Self::finish) sorts the
    /// index and rejects a key added twice.
    pub fn insert(&mut self, key: u32, data: &[u8]) -> Result<()> {
        let len = u32::try_from(data.len()).context("pack entry over 4 GiB")?;
        self.blob.write_all(data)?;
        self.entries.push((key, self.offset, len));
        self.offset += data.len() as u64;
        Ok(())
    }

    pub fn finish(mut self) -> Result<usize> {
        self.entries.sort_unstable_by_key(|e| e.0);
        if let Some(w) = self.entries.windows(2).find(|w| w[0].0 == w[1].0) {
            bail!("pack key {} added twice", w[0].0);
        }
        self.blob.flush()?;
        self.blob.get_ref().sync_all()?;
        let mut out = Vec::with_capacity(8 + 16 * self.entries.len());
        out.extend_from_slice(b"XIX1");
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (k, o, l) in &self.entries {
            out.extend_from_slice(&k.to_le_bytes());
            out.extend_from_slice(&o.to_le_bytes());
            out.extend_from_slice(&l.to_le_bytes());
        }
        std::fs::write(&self.idx_path, out)?;
        Ok(self.entries.len())
    }
}

pub struct PackReader {
    blob: File,
    blob_len: u64,
    index: BTreeMap<u32, (u64, u32)>,
}

impl PackReader {
    pub fn open(base: &Path) -> Result<Self> {
        let idx = std::fs::read(base.with_extension("idx"))
            .with_context(|| format!("read {}", base.with_extension("idx").display()))?;
        if idx.len() < 8 || &idx[..4] != b"XIX1" {
            bail!("{}: bad pack index", base.display());
        }
        let n = u32::from_le_bytes(idx[4..8].try_into().unwrap()) as usize;
        if idx.len() != 8 + 16 * n {
            bail!("{}: truncated pack index", base.display());
        }
        let blob = File::open(base.with_extension("pack"))?;
        let blob_len = blob.metadata()?.len();
        let mut index = BTreeMap::new();
        for e in idx[8..].chunks_exact(16) {
            let k = u32::from_le_bytes(e[0..4].try_into().unwrap());
            let o = u64::from_le_bytes(e[4..12].try_into().unwrap());
            let l = u32::from_le_bytes(e[12..16].try_into().unwrap());
            if o + l as u64 > blob_len {
                bail!("{}: pack entry {k} out of bounds", base.display());
            }
            index.insert(k, (o, l));
        }
        Ok(PackReader {
            blob,
            blob_len,
            index,
        })
    }

    pub fn get(&self, key: u32) -> Result<Option<Vec<u8>>> {
        let Some(&(o, l)) = self.index.get(&key) else {
            return Ok(None);
        };
        let mut buf = vec![0u8; l as usize];
        self.blob.read_exact_at(&mut buf, o)?;
        Ok(Some(buf))
    }

    /// `len` bytes at `off` within the entry of `key` (None if there is no such entry).
    pub fn get_range(&self, key: u32, off: u64, len: u64) -> Result<Option<Vec<u8>>> {
        let Some(&(o, l)) = self.index.get(&key) else {
            return Ok(None);
        };
        if off.checked_add(len).is_none_or(|end| end > l as u64) {
            bail!("pack entry {key}: read {off}+{len} beyond its {l} bytes");
        }
        let mut buf = vec![0u8; len as usize];
        self.blob.read_exact_at(&mut buf, o + off)?;
        Ok(Some(buf))
    }

    pub fn contains(&self, key: u32) -> bool {
        self.index.contains_key(&key)
    }

    pub fn keys(&self) -> impl Iterator<Item = u32> + '_ {
        self.index.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn blob_len(&self) -> u64 {
        self.blob_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_order() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("p");
        let mut w = PackWriter::create(&base).unwrap();
        w.add(1, b"abc").unwrap();
        w.add(7, b"").unwrap();
        w.add(9, b"xyz12").unwrap();
        assert!(w.add(9, b"dup").is_err());
        assert_eq!(w.finish().unwrap(), 3);
        let r = PackReader::open(&base).unwrap();
        assert_eq!(r.get(1).unwrap().unwrap(), b"abc");
        assert_eq!(r.get(7).unwrap().unwrap(), b"");
        assert_eq!(r.get(9).unwrap().unwrap(), b"xyz12");
        assert_eq!(r.get(2).unwrap(), None);
        assert_eq!(r.keys().collect::<Vec<_>>(), vec![1, 7, 9]);
    }

    #[test]
    fn insert_in_any_order() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("p");
        let mut w = PackWriter::create(&base).unwrap();
        for k in [5u32, 1, 9, 3] {
            w.insert(k, &k.to_le_bytes()).unwrap();
        }
        assert_eq!(w.finish().unwrap(), 4);
        let r = PackReader::open(&base).unwrap();
        assert_eq!(r.keys().collect::<Vec<_>>(), vec![1, 3, 5, 9]);
        for k in [5u32, 1, 9, 3] {
            assert_eq!(r.get(k).unwrap().unwrap(), k.to_le_bytes());
        }
        assert!(r.contains(3) && !r.contains(4));
        assert_eq!(r.get_range(5, 1, 2).unwrap().unwrap(), [0, 0]);
        assert_eq!(r.get_range(4, 0, 1).unwrap(), None);
        assert!(r.get_range(5, 3, 2).is_err());
        let mut w = PackWriter::create(&base).unwrap();
        w.insert(2, b"a").unwrap();
        w.insert(1, b"b").unwrap();
        w.insert(2, b"c").unwrap();
        assert!(w.finish().is_err());
    }
}
