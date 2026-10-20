//! Column bundles: a generic, efficient way to import many per-cell columns that
//! were computed elsewhere (e.g. CytoSignal LR scores exported from R).
//!
//! ```text
//! <name>.json          {"format":"xenium-web-columns","version":1,"n_rows":N,
//!                       "cells_file":"<name>.cells.txt.gz","data_file":"<name>.f32.gz",
//!                       "columns":[{"id","label","kind":"numeric"|"categorical",
//!                                   "categories":[..],"group","provenance":{..}}, ..]}
//! <name>.cells.txt.gz  one original cell_id per line (N lines)
//! <name>.f32.gz        column-major little-endian float32, N values per column;
//!                      numeric: NaN = missing; categorical: 0 = missing, 1..k = category
//! ```
//! File references must be plain names next to the JSON (no paths).

use crate::stats::field_info;
use crate::tables_ingest::{ImportReport, ImportedField};
use anyhow::{anyhow, bail, Context, Result};
use flate2::read::MultiGzDecoder;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use xenium_core::codec::FieldColumn;
use xenium_core::manifest::{is_safe_id, CategoryInfo};
use xenium_core::CellDictionary;

#[derive(Deserialize)]
struct Meta {
    format: String,
    version: u32,
    n_rows: usize,
    cells_file: String,
    data_file: String,
    columns: Vec<Column>,
}

#[derive(Deserialize)]
struct Column {
    id: String,
    label: String,
    kind: String,
    #[serde(default)]
    categories: Option<Vec<String>>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    provenance: Option<serde_json::Map<String, serde_json::Value>>,
}

fn sibling(json: &Path, name: &str) -> Result<std::path::PathBuf> {
    let p = Path::new(name);
    if p.components().count() != 1 || name.starts_with('.') {
        bail!("bundle file reference {name:?} must be a plain file name next to the JSON");
    }
    Ok(json.with_file_name(p))
}

fn gz(path: &Path) -> Result<Box<dyn Read>> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let r = std::io::BufReader::with_capacity(1 << 20, f);
    Ok(if path.extension().is_some_and(|e| e == "gz") {
        Box::new(MultiGzDecoder::new(r))
    } else {
        Box::new(r)
    })
}

pub fn import_column_bundle(
    json: &Path,
    dict: &CellDictionary,
    taken: &BTreeSet<String>,
) -> Result<ImportReport> {
    let source = json
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let meta: Meta = serde_json::from_slice(
        &std::fs::read(json).with_context(|| format!("read {}", json.display()))?,
    )
    .with_context(|| format!("{source}: not a column bundle"))?;
    if meta.format != "xenium-web-columns" || meta.version != 1 {
        bail!(
            "{source}: unsupported bundle format {:?} v{}",
            meta.format,
            meta.version
        );
    }
    let mut warnings = vec![];
    let ids: Vec<String> = BufReader::new(gz(&sibling(json, &meta.cells_file)?)?)
        .lines()
        .collect::<std::io::Result<_>>()?;
    if ids.len() != meta.n_rows {
        bail!(
            "{source}: {} cell ids, n_rows says {}",
            ids.len(),
            meta.n_rows
        );
    }
    let n = dict.len();
    let keys: Vec<Option<u32>> = ids.iter().map(|id| dict.key_of(id)).collect();
    let unknown = keys.iter().filter(|k| k.is_none()).count();
    if unknown > 0 {
        warnings.push(format!(
            "{source}: {unknown} rows have a cell_id not present in cells.parquet; ignored"
        ));
    }
    let mut seen = vec![false; n];
    for k in keys.iter().flatten() {
        if std::mem::replace(&mut seen[*k as usize - 1], true) {
            bail!(
                "{source}: cell {:?} appears more than once",
                dict.id_of(*k).unwrap_or("")
            );
        }
    }
    let mut data = gz(&sibling(json, &meta.data_file)?)?;
    let mut buf = vec![0u8; meta.n_rows * 4];
    let mut fields = vec![];
    let mut ids_seen = BTreeSet::new();
    for c in &meta.columns {
        if !is_safe_id(&c.id) || taken.contains(&c.id) || !ids_seen.insert(c.id.clone()) {
            bail!("{source}: column id {:?} is invalid or already used", c.id);
        }
        data.read_exact(&mut buf)
            .map_err(|_| anyhow!("{source}: data file ends before column {:?}", c.id))?;
        let vals = buf
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()));
        let (column, cats) = match c.kind.as_str() {
            "numeric" => {
                let mut out = vec![f32::NAN; n];
                for (v, k) in vals.zip(&keys) {
                    if let Some(k) = k {
                        out[*k as usize - 1] = v;
                    }
                }
                (FieldColumn::Numeric(out), None)
            }
            "categorical" => {
                let names = c
                    .categories
                    .clone()
                    .ok_or_else(|| anyhow!("{source}: {:?} has no categories", c.id))?;
                let ncat = names.len() as u32;
                let mut codes = vec![0u32; n];
                let mut counts = vec![0u64; names.len()];
                for (v, k) in vals.zip(&keys) {
                    let code = if v.is_nan() { 0.0 } else { v };
                    if code.fract() != 0.0 || code < 0.0 || code > ncat as f32 {
                        bail!("{source}: {:?} has invalid category code {v}", c.id);
                    }
                    if let Some(k) = k {
                        codes[*k as usize - 1] = code as u32;
                        if code >= 1.0 {
                            counts[code as usize - 1] += 1;
                        }
                    }
                }
                let cats = names
                    .into_iter()
                    .zip(counts)
                    .map(|(name, count)| CategoryInfo { name, count })
                    .collect();
                (
                    FieldColumn::Categorical {
                        codes,
                        n_categories: ncat,
                    },
                    Some(cats),
                )
            }
            other => bail!("{source}: column {:?} has unknown kind {other:?}", c.id),
        };
        let group = c.group.clone().unwrap_or_else(|| "bundle".into());
        let mut info = field_info(&c.id, &c.label, &group, &source, &column, cats);
        if let Some(p) = &c.provenance {
            info.provenance = p.clone();
        }
        fields.push(ImportedField { info, column });
    }
    let mut rest = [0u8; 1];
    if data.read(&mut rest)? != 0 {
        bail!("{source}: data file has more values than the declared columns");
    }
    Ok(ImportReport { fields, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use std::io::Write;

    fn write_gz(path: &Path, bytes: &[u8]) {
        let mut e = GzEncoder::new(
            std::fs::File::create(path).unwrap(),
            flate2::Compression::fast(),
        );
        e.write_all(bytes).unwrap();
        e.finish().unwrap();
    }

    #[test]
    fn bundle_round_trip_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let dict =
            CellDictionary::from_ids(vec!["a-1".into(), "b-1".into(), "c-1".into()]).unwrap();
        write_gz(&dir.path().join("x.cells.txt.gz"), b"c-1\na-1\nzz\n");
        let mut f32s = vec![];
        for v in [2.5f32, f32::NAN, 9.0, /* cat */ 2.0, 1.0, 0.0] {
            f32s.extend_from_slice(&v.to_le_bytes());
        }
        write_gz(&dir.path().join("x.f32.gz"), &f32s);
        let meta = serde_json::json!({"format": "xenium-web-columns", "version": 1, "n_rows": 3,
            "cells_file": "x.cells.txt.gz", "data_file": "x.f32.gz", "columns": [
            {"id": "cs:diff:P1:score", "label": "L → R score", "kind": "numeric", "group": "lr_score", "provenance": {"method": "m"}},
            {"id": "cs:diff:P1:hotspot", "label": "L → R hotspot", "kind": "categorical", "categories": ["background", "hotspot"], "group": "lr_hotspot"}]});
        let json = dir.path().join("x.json");
        std::fs::write(&json, meta.to_string()).unwrap();
        let r = import_column_bundle(&json, &dict, &BTreeSet::new()).unwrap();
        assert_eq!(r.warnings.len(), 1); // "zz" unknown
        match &r.fields[0].column {
            FieldColumn::Numeric(v) => assert!(v[0].is_nan() && v[1].is_nan() && v[2] == 2.5),
            _ => panic!(),
        }
        assert_eq!(r.fields[0].info.group, "lr_score");
        assert_eq!(r.fields[0].info.provenance["method"], "m");
        assert_eq!(
            r.fields[1].column,
            FieldColumn::Categorical {
                codes: vec![1, 0, 2],
                n_categories: 2
            }
        );
        assert_eq!(r.fields[1].info.categories.as_ref().unwrap()[1].count, 1);
        // Path escapes and truncated data are rejected.
        let bad = meta.to_string().replace("x.f32.gz", "../x.f32.gz");
        std::fs::write(&json, bad).unwrap();
        assert!(import_column_bundle(&json, &dict, &BTreeSet::new()).is_err());
        write_gz(&dir.path().join("x.f32.gz"), &f32s[..20]);
        std::fs::write(&json, meta.to_string()).unwrap();
        assert!(import_column_bundle(&json, &dict, &BTreeSet::new()).is_err());
    }
}
