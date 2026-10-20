//! Run a scoring job for one section: score the signatures that are not cached yet
//! (all in one pass over the expression data) and store each result.

use crate::cache::{result_key, Level, ResultCache, ResultMeta, SignatureSnapshot, Values};
use crate::progress::Progress;
use crate::section::{coverage, score_bins, score_cells, stats, SectionPanel};
use crate::ucell::{UcellParams, ALGORITHM};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use xenium_index::SectionStore;

pub const MAX_SIGNATURES_PER_JOB: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub signatures: Vec<SignatureSnapshot>,
    pub params: UcellParams,
    #[serde(flatten)]
    pub level: Level,
}

impl JobSpec {
    pub fn keys(&self) -> Vec<String> {
        self.signatures
            .iter()
            .map(|s| result_key(&s.genes, &self.params, &self.level))
            .collect()
    }

    /// Reject what cannot be scored before any work starts.
    pub fn check(&self, panel: &SectionPanel) -> Result<()> {
        if self.signatures.is_empty() || self.signatures.len() > MAX_SIGNATURES_PER_JOB {
            bail!("a job scores 1-{MAX_SIGNATURES_PER_JOB} signatures");
        }
        if let Level::Bin {
            min_transcripts, ..
        } = self.level
        {
            if min_transcripts == 0 {
                bail!("min_transcripts must be at least 1");
            }
        }
        for s in &self.signatures {
            if let Err(e) = panel.scorer(std::slice::from_ref(&s.genes), &self.params) {
                bail!("{}: {e}", s.name);
            }
        }
        Ok(())
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Score and store every signature of `spec` that has no cached result for this
/// index version; returns the metadata of all of them (cached and new), in order.
pub fn run(
    store: &SectionStore,
    version: &str,
    cache: &ResultCache,
    spec: &JobSpec,
    progress: &Progress,
) -> Result<Vec<ResultMeta>> {
    let m = &store.manifest;
    let panel = SectionPanel::from_manifest(m)?;
    spec.check(&panel)?;
    let keys = spec.keys();
    let mut todo = vec![];
    for (i, k) in keys.iter().enumerate() {
        match cache
            .meta(&m.section_id, version, k)?
            .filter(|meta| cache.is_complete(meta))
        {
            None if !todo.iter().any(|&j: &usize| keys[j] == *k) => todo.push(i),
            // Scores depend only on the genes and settings, so a signature with the same
            // gene list reuses them; the result is relabelled with the requesting name.
            Some(mut meta) if meta.signature != spec.signatures[i] => {
                meta.signature = spec.signatures[i].clone();
                cache.store_meta(&meta)?;
            }
            _ => {}
        }
    }
    if !todo.is_empty() {
        let genes: Vec<Vec<String>> = todo
            .iter()
            .map(|&i| spec.signatures[i].genes.clone())
            .collect();
        let scorer = panel.scorer(&genes, &spec.params)?;
        let t0 = Instant::now();
        let (cols, n_tx, grid) = match &spec.level {
            Level::Cell => (score_cells(store, &scorer, progress)?, None, None),
            Level::Bin {
                bin_um,
                min_transcripts,
            } => {
                let b = score_bins(store, &panel, &scorer, *bin_um, *min_transcripts, progress)?;
                (b.scores, Some(b.n_transcripts), Some(b.grid))
            }
        };
        let ms = t0.elapsed().as_millis() as u64;
        progress.check()?;
        progress.phase("saving", todo.len() as u64);
        for (col, &i) in cols.into_iter().zip(&todo) {
            let s = &spec.signatures[i];
            let meta = ResultMeta {
                key: keys[i].clone(),
                section_id: m.section_id.clone(),
                index_version: version.to_string(),
                signature: s.clone(),
                params: spec.params.clone(),
                level: spec.level.clone(),
                algorithm: ALGORITHM.into(),
                n_units: col.len(),
                grid,
                coverage: coverage(store, &panel, &s.genes, &spec.params),
                stats: stats(&col),
                computed_unix: now(),
                duration_ms: ms,
            };
            cache.store(
                &meta,
                &Values {
                    scores: col,
                    n_transcripts: n_tx.clone(),
                },
            )?;
            progress.add(1);
        }
    }
    keys.iter()
        .map(|k| {
            cache
                .meta(&m.section_id, version, k)?
                .ok_or_else(|| anyhow::anyhow!("result {k} missing after scoring"))
        })
        .collect()
}
