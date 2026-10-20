//! Disk-backed UCell results, one file pair per (section version, signature, settings):
//!
//! ```text
//! <state>/ucell/<section_id>/<index_version>/<key>.bin   XUS1 scores
//! <state>/ucell/<section_id>/<index_version>/<key>.json  ResultMeta (written last)
//! ```
//!
//! `key` hashes the algorithm version, the signature's gene list, the parameters and
//! the level, so it is the same for every section and survives renaming a signature.
//! Results of index versions that are no longer live are removed when a new result
//! is stored for the section.

use crate::section::{BinGrid, Coverage, ScoreStats};
use crate::ucell::{UcellParams, ALGORITHM};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use xenium_core::manifest::is_safe_id;

/// What a result describes. Cell- and bin-level results are different quantities
/// and never share a key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "lowercase")]
pub enum Level {
    /// UCell of each cell's own expression.
    Cell,
    /// UCell of the transcripts pooled in each `bin_um` square.
    Bin { bin_um: f64, min_transcripts: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignatureSnapshot {
    /// Id of the stored signature this came from (None for ad-hoc lists).
    pub id: Option<String>,
    pub name: String,
    pub genes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultMeta {
    pub key: String,
    pub section_id: String,
    pub index_version: String,
    pub signature: SignatureSnapshot,
    pub params: UcellParams,
    #[serde(flatten)]
    pub level: Level,
    pub algorithm: String,
    /// Cells (cell level) or bins (bin level).
    pub n_units: usize,
    pub grid: Option<BinGrid>,
    pub coverage: Coverage,
    pub stats: ScoreStats,
    pub computed_unix: u64,
    pub duration_ms: u64,
}

pub fn result_key(genes: &[String], params: &UcellParams, level: &Level) -> String {
    let canon = json!({"algorithm": ALGORITHM, "genes": genes, "params": params, "level": level})
        .to_string();
    let h = Sha256::digest(canon.as_bytes());
    h.iter().take(10).map(|b| format!("{b:02x}")).collect()
}

pub fn is_key(s: &str) -> bool {
    s.len() == 20
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Stored values: f64 per unit, plus transcripts per bin for bin-level results.
pub struct Values {
    pub scores: Vec<f64>,
    pub n_transcripts: Option<Vec<u32>>,
}

const MAGIC: &[u8; 4] = b"XUS1";
const HEADER: usize = 48;

fn encode(level: &Level, grid: Option<&BinGrid>, v: &Values) -> Vec<u8> {
    let n = v.scores.len();
    let mut b = Vec::with_capacity(HEADER + n * 12);
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&(matches!(level, Level::Bin { .. }) as u16).to_le_bytes());
    b.extend_from_slice(&(n as u32).to_le_bytes());
    let g = grid.copied().unwrap_or(BinGrid {
        origin_um: [0.0; 2],
        bin_um: 0.0,
        width: 0,
        height: 0,
    });
    b.extend_from_slice(&g.width.to_le_bytes());
    b.extend_from_slice(&g.height.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    for x in [g.origin_um[0], g.origin_um[1], g.bin_um] {
        b.extend_from_slice(&x.to_le_bytes());
    }
    for x in &v.scores {
        b.extend_from_slice(&x.to_le_bytes());
    }
    if let Some(t) = &v.n_transcripts {
        for x in t {
            b.extend_from_slice(&x.to_le_bytes());
        }
    }
    b
}

fn decode(b: &[u8], n_expected: usize) -> Result<Values> {
    if b.len() < HEADER || &b[..4] != MAGIC || u16::from_le_bytes([b[4], b[5]]) != 1 {
        bail!("not an XUS1 v1 file");
    }
    let bin = u16::from_le_bytes([b[6], b[7]]) == 1;
    let n = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
    let want = HEADER + n * 8 + if bin { n * 4 } else { 0 };
    if n != n_expected || b.len() != want {
        bail!("XUS1 length mismatch");
    }
    let scores = b[HEADER..HEADER + n * 8]
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let n_transcripts = bin.then(|| {
        b[HEADER + n * 8..]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    });
    Ok(Values {
        scores,
        n_transcripts,
    })
}

/// Write `bytes` to `path` atomically (temp file, optional fsync, rename).
fn write_file(path: &Path, bytes: &[u8], fsync: bool) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        path.file_name().unwrap().to_string_lossy()
    ));
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)?;
        if fsync {
            f.sync_all()?;
        }
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Durable atomic write (user data such as signatures).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_file(path, bytes, true)
}

pub struct ResultCache {
    root: PathBuf,
}

impl ResultCache {
    /// `state_dir/ucell` (created on first write).
    pub fn new(state_dir: &Path) -> Self {
        ResultCache {
            root: state_dir.join("ucell"),
        }
    }

    fn dir(&self, section: &str, version: &str) -> Result<PathBuf> {
        if !is_safe_id(section)
            || section.contains(':')
            || !is_safe_id(version)
            || version.starts_with('.')
        {
            bail!("invalid section id or version");
        }
        Ok(self.root.join(section).join(version))
    }

    pub fn store(&self, meta: &ResultMeta, values: &Values) -> Result<()> {
        if !is_key(&meta.key) {
            bail!("invalid result key");
        }
        let dir = self.dir(&meta.section_id, &meta.index_version)?;
        // Results can be recomputed, so they are not fsynced (fsync is slow on USB
        // disks); `is_complete` catches a values file lost or cut short by a crash.
        write_file(
            &dir.join(format!("{}.bin", meta.key)),
            &encode(&meta.level, meta.grid.as_ref(), values),
            false,
        )?;
        write_file(
            &dir.join(format!("{}.json", meta.key)),
            &serde_json::to_vec_pretty(meta)?,
            false,
        )?;
        // Drop results of index versions that are no longer live.
        if let Ok(rd) = std::fs::read_dir(self.root.join(&meta.section_id)) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy() != meta.index_version && e.path().is_dir() {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        Ok(())
    }

    /// Remove one stored result; returns whether it existed.
    pub fn delete(&self, section: &str, version: &str, key: &str) -> Result<bool> {
        if !is_key(key) {
            bail!("invalid result key");
        }
        let dir = self.dir(section, version)?;
        let mut found = false;
        // Metadata first: without it the values file is never listed or served.
        for ext in ["json", "bin"] {
            match std::fs::remove_file(dir.join(format!("{key}.{ext}"))) {
                Ok(()) => found = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(found)
    }

    /// Rewrite only the metadata of an existing result (e.g. to relabel it).
    pub fn store_meta(&self, meta: &ResultMeta) -> Result<()> {
        if !is_key(&meta.key) {
            bail!("invalid result key");
        }
        let dir = self.dir(&meta.section_id, &meta.index_version)?;
        write_file(
            &dir.join(format!("{}.json", meta.key)),
            &serde_json::to_vec_pretty(meta)?,
            false,
        )
    }

    /// Whether the values file exists with the length the metadata implies.
    pub fn is_complete(&self, meta: &ResultMeta) -> bool {
        let Ok(dir) = self.dir(&meta.section_id, &meta.index_version) else {
            return false;
        };
        let want = HEADER + meta.n_units * if meta.grid.is_some() { 12 } else { 8 };
        std::fs::metadata(dir.join(format!("{}.bin", meta.key)))
            .is_ok_and(|m| m.len() == want as u64)
    }

    pub fn meta(&self, section: &str, version: &str, key: &str) -> Result<Option<ResultMeta>> {
        if !is_key(key) {
            bail!("invalid result key");
        }
        let p = self.dir(section, version)?.join(format!("{key}.json"));
        match std::fs::read(&p) {
            Ok(b) => Ok(Some(
                serde_json::from_slice(&b).with_context(|| format!("parse {}", p.display()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn values(&self, meta: &ResultMeta) -> Result<Values> {
        let p = self
            .dir(&meta.section_id, &meta.index_version)?
            .join(format!("{}.bin", meta.key));
        decode(
            &std::fs::read(&p).with_context(|| format!("read {}", p.display()))?,
            meta.n_units,
        )
    }

    /// All complete results for one section version, newest first.
    pub fn list(&self, section: &str, version: &str) -> Result<Vec<ResultMeta>> {
        let dir = self.dir(section, version)?;
        let mut out = vec![];
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(key) = name.strip_suffix(".json").filter(|k| is_key(k)) {
                match self.meta(section, version, key) {
                    Ok(Some(m)) if self.is_complete(&m) => out.push(m),
                    Ok(Some(_)) => tracing::warn!(
                        "UCell result {key} is incomplete; it will be recomputed when requested"
                    ),
                    Ok(None) => {}
                    Err(err) => tracing::warn!("skipping unreadable UCell result {key}: {err:#}"),
                }
            }
        }
        out.sort_by(|a, b| {
            b.computed_unix
                .cmp(&a.computed_unix)
                .then(a.key.cmp(&b.key))
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_depend_on_genes_params_and_level_only() {
        let g = vec!["A".to_string(), "B-".to_string()];
        let p = UcellParams::default();
        let k = result_key(&g, &p, &Level::Cell);
        assert!(is_key(&k));
        assert_eq!(k, result_key(&g, &p, &Level::Cell));
        assert_ne!(
            k,
            result_key(
                &g,
                &UcellParams {
                    max_rank: 100,
                    ..p.clone()
                },
                &Level::Cell
            )
        );
        assert_ne!(
            k,
            result_key(
                &g,
                &p,
                &Level::Bin {
                    bin_um: 50.0,
                    min_transcripts: 10
                }
            )
        );
        assert_ne!(k, result_key(&["B-".into(), "A".into()], &p, &Level::Cell));
    }

    #[test]
    fn values_round_trip() {
        let v = Values {
            scores: vec![0.5, f64::NAN, 1.0],
            n_transcripts: Some(vec![3, 0, 9]),
        };
        let lvl = Level::Bin {
            bin_um: 20.0,
            min_transcripts: 1,
        };
        let grid = BinGrid {
            origin_um: [0.0, 20.0],
            bin_um: 20.0,
            width: 3,
            height: 1,
        };
        let back = decode(&encode(&lvl, Some(&grid), &v), 3).unwrap();
        assert_eq!(back.scores[0], 0.5);
        assert!(back.scores[1].is_nan());
        assert_eq!(back.n_transcripts, Some(vec![3, 0, 9]));
        assert!(decode(&encode(&lvl, Some(&grid), &v), 4).is_err());
    }
}
