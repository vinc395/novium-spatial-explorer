//! Per-sample information (e.g. patient, sex, age, timepoint, treatment) shown in the
//! viewer's Index panel. Loaded once at start-up from a CSV kept outside the index.
//! Only the columns listed on the command line are read and served (everything else
//! in the file is ignored), it is served only behind the access check like the rest
//! of the API, and values are never logged.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default)]
pub struct SampleInfo {
    /// Columns served, in the order given.
    pub columns: Vec<String>,
    by_section: HashMap<String, Vec<(String, String)>>,
    /// Lower-cased id -> id, so `S0001A` in the file matches section `S0001a`.
    by_lower: HashMap<String, String>,
    /// Rows whose id was already seen (the first row is kept).
    pub duplicate_ids: usize,
}

impl SampleInfo {
    /// `id_column` holds the section ids (sample ids); `columns` are the fields to show.
    pub fn load(path: &Path, id_column: &str, columns: &[String]) -> Result<Self> {
        if columns.is_empty() {
            bail!("list the sample columns to show (--sample-metadata-columns)");
        }
        let mut r = csv::ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::All)
            .from_path(path)
            .with_context(|| format!("open {}", path.display()))?;
        let header: Vec<String> = r
            .headers()?
            .iter()
            .enumerate()
            .map(|(i, h)| {
                if i == 0 {
                    h.trim_start_matches('\u{feff}').to_string()
                } else {
                    h.to_string()
                }
            })
            .collect();
        let find = |name: &str| -> Result<usize> {
            header.iter().position(|h| h == name).ok_or_else(|| {
                let near: Vec<&String> = header
                    .iter()
                    .filter(|h| h.eq_ignore_ascii_case(name))
                    .collect();
                anyhow!(
                    "column {name:?} is not in {}{}",
                    path.display(),
                    if near.is_empty() {
                        String::new()
                    } else {
                        format!(" (did you mean {:?}?)", near[0])
                    }
                )
            })
        };
        let id = find(id_column)?;
        let cols: Vec<(String, usize)> = columns
            .iter()
            .map(|c| Ok((c.clone(), find(c)?)))
            .collect::<Result<_>>()?;
        let mut out = SampleInfo {
            columns: columns.to_vec(),
            ..Default::default()
        };
        for rec in r.records() {
            let rec = rec.with_context(|| format!("read {}", path.display()))?;
            let Some(sid) = rec.get(id).filter(|s| !s.is_empty()) else {
                continue;
            };
            if out.by_section.contains_key(sid) {
                out.duplicate_ids += 1;
                continue;
            }
            let values: Vec<(String, String)> = cols
                .iter()
                .filter_map(|(name, i)| {
                    rec.get(*i)
                        .filter(|v| !v.is_empty())
                        .map(|v| (name.clone(), v.to_string()))
                })
                .collect();
            out.by_lower
                .entry(sid.to_lowercase())
                .or_insert_with(|| sid.to_string());
            out.by_section.insert(sid.to_string(), values);
        }
        Ok(out)
    }

    /// Values for a section id (exact match first, then ignoring case).
    pub fn get(&self, section: &str) -> Option<&Vec<(String, String)>> {
        self.by_section.get(section).or_else(|| {
            self.by_lower
                .get(&section.to_lowercase())
                .and_then(|k| self.by_section.get(k))
        })
    }

    pub fn has(&self, section: &str) -> bool {
        self.get(section).is_some()
    }

    pub fn len(&self) -> usize {
        self.by_section.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_section.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_listed_columns_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("meta.csv");
        std::fs::write(
            &p,
            "\u{feff}sample,patient,sample_id,Sex,Age,HLA,timepoint\nx,P1,A,F,54,secret,BL\ny,P2,B,M,,secret,6M\nz,P3,A,M,60,secret,BL\n",
        )
        .unwrap();
        let cols = ["patient", "Age", "Sex"].map(String::from);
        let s = SampleInfo::load(&p, "sample_id", &cols).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.duplicate_ids, 1);
        assert_eq!(s.get("a"), s.get("A"), "ids match ignoring case");
        let a = s.get("A").unwrap();
        assert_eq!(
            a,
            &vec![
                ("patient".into(), "P1".into()),
                ("Age".into(), "54".into()),
                ("Sex".into(), "F".into())
            ]
        );
        // Empty values are left out; unlisted columns (HLA) are never kept.
        assert_eq!(s.get("B").unwrap().len(), 2);
        assert!(s.get("A").unwrap().iter().all(|(k, _)| k != "HLA"));
        let err = SampleInfo::load(&p, "sample_id", &["age".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("did you mean \"Age\""), "{err}");
        assert!(SampleInfo::load(&p, "Sample_ID", &cols).is_err());
    }
}
