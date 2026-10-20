//! UCell over one section index: the measured panel, gene coverage, and scoring of
//! cells (per-cell expression) or square bins (pooled transcripts).
//!
//! * **Cell level** ranks each cell's QV>=20 counts of the transcripts assigned to it
//!   (`expr_csr`), the same counts as Xenium's `cell_feature_matrix`.
//! * **Bin level** ranks the QV>=20 counts of *all* transcripts whose position falls
//!   in a `bin_um` square (assigned to a cell or not), so it is independent of cell
//!   segmentation. Bins with fewer than `min_transcripts` gene transcripts get NaN.
//!
//! Both rank over every gene-expression feature of the panel (control probes and
//! codewords are excluded), never over just the selected or visible genes.

use crate::progress::Progress;
use crate::ucell::{split_signature, Panel, Scorer, UcellParams};
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use xenium_core::codec::{POINT_RECORD_BYTES, QV_SCALE};
use xenium_core::manifest::SectionManifest;
use xenium_index::transcripts_ingest::EXPR_QV_MIN;
use xenium_index::SectionStore;

/// Cells per scoring chunk (one CSR read, one progress/cancel step).
pub const CELL_CHUNK: usize = 4096;
/// Largest bin grid accepted (width x height).
pub const MAX_BINS: usize = 4_000_000;

/// The gene-expression features of a section, as UCell's matrix rows.
pub struct SectionPanel {
    by_name: HashMap<String, u32>,
    /// Non-gene features by name (to explain why a "gene" is not usable).
    other: HashMap<String, String>,
    by_lower: HashMap<String, String>,
    is_gene: Vec<bool>,
    n: usize,
}

impl SectionPanel {
    pub fn from_manifest(m: &SectionManifest) -> Result<Self> {
        let Some(t) = &m.transcripts else {
            bail!(
                "section {} has no transcript index (no expression to rank)",
                m.section_id
            );
        };
        let mut is_gene = vec![false; t.genes.iter().map(|g| g.id as usize + 1).max().unwrap_or(0)];
        let (mut by_name, mut other, mut by_lower) =
            (HashMap::new(), HashMap::new(), HashMap::new());
        for g in &t.genes {
            if g.category.is_gene() {
                is_gene[g.id as usize] = true;
                by_name.insert(g.name.clone(), g.id);
                by_lower.insert(g.name.to_lowercase(), g.name.clone());
            } else {
                other.insert(g.name.clone(), format!("{:?}", g.category));
            }
        }
        let n = by_name.len();
        Ok(SectionPanel {
            by_name,
            other,
            by_lower,
            is_gene,
            n,
        })
    }

    pub fn max_feature_id(&self) -> u32 {
        self.is_gene.len().saturating_sub(1) as u32
    }

    pub fn gene_ids(&self) -> Vec<u32> {
        (0..self.is_gene.len() as u32)
            .filter(|&i| self.is_gene[i as usize])
            .collect()
    }

    pub fn scorer(&self, signatures: &[Vec<String>], p: &UcellParams) -> Result<Scorer> {
        Scorer::new(self, self.max_feature_id(), signatures, p)
    }
}

impl Panel for SectionPanel {
    fn n_genes(&self) -> usize {
        self.n
    }
    fn lookup(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }
    fn is_gene(&self, id: u32) -> bool {
        self.is_gene.get(id as usize) == Some(&true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneCoverage {
    /// Gene name as used for matching (suffix removed).
    pub gene: String,
    /// `+` positive, `-` negative.
    pub sign: char,
    /// `found`, `missing` (not in the panel) or `not_a_gene` (a control/codeword feature).
    pub status: String,
    pub cells_detected: Option<u32>,
    pub fraction_detected: Option<f64>,
    pub transcripts_qv20: Option<u64>,
    /// A panel gene that matches case-insensitively, when the name itself does not.
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coverage {
    pub panel_genes: usize,
    pub max_rank_effective: u32,
    pub n_entries: usize,
    pub positive_found: usize,
    pub positive_total: usize,
    pub negative_found: usize,
    pub negative_total: usize,
    /// Genes absent from the panel that are scored at maxRank (`impute`).
    pub imputed: usize,
    pub genes: Vec<GeneCoverage>,
    /// Why this signature cannot be scored with these parameters, if so.
    pub error: Option<String>,
}

/// How a signature's genes map onto this section's panel (and how they are detected).
pub fn coverage(
    store: &SectionStore,
    panel: &SectionPanel,
    genes: &[String],
    p: &UcellParams,
) -> Coverage {
    let sp = split_signature(genes);
    let n_cells = store.n_cells().max(1) as f64;
    let info = store.manifest.transcripts.as_ref();
    let mut out = vec![];
    let mut count = |names: &[String], sign: char| {
        let mut found = 0;
        for name in names {
            let id = panel.lookup(name);
            found += id.is_some() as usize;
            let status = if id.is_some() {
                "found"
            } else if panel.other.contains_key(name) {
                "not_a_gene"
            } else {
                "missing"
            };
            let detected = id.and_then(|g| store.gene_cells_detected(g));
            out.push(GeneCoverage {
                gene: name.clone(),
                sign,
                status: status.into(),
                cells_detected: detected,
                fraction_detected: detected.map(|d| d as f64 / n_cells),
                transcripts_qv20: id.and_then(|g| {
                    info.and_then(|t| t.genes.get(g as usize))
                        .map(|g| g.count_qv20)
                }),
                suggestion: if id.is_none() {
                    panel.by_lower.get(&name.to_lowercase()).cloned()
                } else {
                    None
                },
            });
        }
        found
    };
    let positive_found = count(&sp.positive, '+');
    let negative_found = count(&sp.negative, '-');
    let max_rank_effective = p.max_rank.min(panel.n_genes() as u32);
    let imputed = if p.missing_genes == crate::ucell::MissingGenes::Impute {
        sp.positive.len() + sp.negative.len() - positive_found - negative_found
    } else {
        0
    };
    let error = panel
        .scorer(&[genes.to_vec()], p)
        .err()
        .map(|e| e.to_string());
    Coverage {
        panel_genes: panel.n_genes(),
        max_rank_effective,
        n_entries: genes.len(),
        positive_found,
        positive_total: sp.positive.len(),
        negative_found,
        negative_total: sp.negative.len(),
        imputed,
        genes: out,
        error,
    }
}

/// Cell-level UCell: one column per signature, index = cell_key - 1.
pub fn score_cells(
    store: &SectionStore,
    scorer: &Scorer,
    progress: &Progress,
) -> Result<Vec<Vec<f64>>> {
    let n = store.n_cells();
    let k = scorer.n_signatures();
    progress.phase("scoring cells", n as u64);
    let chunks: Vec<(u32, usize)> = (0..n)
        .step_by(CELL_CHUNK)
        .map(|a| (a as u32 + 1, CELL_CHUNK.min(n - a)))
        .collect();
    let parts: Vec<Vec<f64>> = chunks
        .par_iter()
        .map(|&(first, len)| -> Result<Vec<f64>> {
            progress.check()?;
            let (offs, raw) = store.csr_block(first, len)?;
            let (mut entries, mut scratch, mut row) = (vec![], vec![], vec![0.0; k]);
            let mut out = Vec::with_capacity(len * k);
            for i in 0..len {
                entries.clear();
                let (a, b) = (offs[i] as usize * 8, offs[i + 1] as usize * 8);
                for r in raw[a..b].chunks_exact(8) {
                    let g = u32::from_le_bytes(r[0..4].try_into().unwrap());
                    let c = u32::from_le_bytes(r[4..8].try_into().unwrap());
                    entries.push((g, c as f64));
                }
                scorer.score_unit(&entries, &mut scratch, &mut row);
                out.extend_from_slice(&row);
            }
            progress.add(len as u64);
            Ok(out)
        })
        .collect::<Result<_>>()?;
    let mut cols = vec![Vec::with_capacity(n); k];
    for part in parts {
        for row in part.chunks_exact(k.max(1)) {
            for (c, v) in cols.iter_mut().zip(row) {
                c.push(*v);
            }
        }
    }
    Ok(cols)
}

/// A square bin grid aligned to multiples of `bin_um` (row 0 at the top, y down).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BinGrid {
    pub origin_um: [f64; 2],
    pub bin_um: f64,
    pub width: u32,
    pub height: u32,
}

impl BinGrid {
    pub fn for_bounds(b: [f64; 4], bin_um: f64) -> Result<Self> {
        if !(bin_um.is_finite() && (2.0..=2000.0).contains(&bin_um)) {
            bail!("bin size must be between 2 and 2000 µm");
        }
        let ox = (b[0] / bin_um).floor() * bin_um;
        let oy = (b[1] / bin_um).floor() * bin_um;
        let w = (((b[2] - ox) / bin_um).ceil() as u64).max(1);
        let h = (((b[3] - oy) / bin_um).ceil() as u64).max(1);
        if (w * h) as usize > MAX_BINS {
            bail!("{w} x {h} bins is too many; use bins larger than {bin_um} µm");
        }
        Ok(BinGrid {
            origin_um: [ox, oy],
            bin_um,
            width: w as u32,
            height: h as u32,
        })
    }
    pub fn len(&self) -> usize {
        self.width as usize * self.height as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct BinScores {
    pub grid: BinGrid,
    /// One column per signature, row-major bins; NaN = too few transcripts.
    pub scores: Vec<Vec<f64>>,
    /// QV>=20 gene transcripts per bin.
    pub n_transcripts: Vec<u32>,
}

/// Bin-level UCell over pooled transcripts. Bands of bin rows are processed in
/// parallel; for each band every gene's records of the overlapping tile rows are
/// read once (one contiguous range per gene).
pub fn score_bins(
    store: &SectionStore,
    panel: &SectionPanel,
    scorer: &Scorer,
    bin_um: f64,
    min_transcripts: u32,
    progress: &Progress,
) -> Result<BinScores> {
    let m = &store.manifest;
    let grid = BinGrid::for_bounds(m.bounds_um, bin_um)?;
    let ts = m.grid.tile_size_um;
    let k = scorer.n_signatures();
    let rows_per_band = ((ts / bin_um).floor() as usize).max(1);
    let n_bands = (grid.height as usize).div_ceil(rows_per_band);
    let genes = panel.gene_ids();
    let qv_q = (EXPR_QV_MIN * QV_SCALE) as u8;
    let [ox, oy] = grid.origin_um;
    let w = grid.width as usize;
    // One sequential read first: the per-band, per-gene reads below are then served
    // from the page cache (on a USB disk this turns minutes of seeking into seconds).
    progress.phase("reading transcripts", store.points_bytes() >> 20);
    store.warm_points(|n| {
        progress.add(n >> 20);
        !progress.is_cancelled()
    })?;
    progress.check()?;
    progress.phase("scoring bins", n_bands as u64);
    type Band = (usize, Vec<u32>, Vec<f64>);
    let bands: Vec<Band> = (0..n_bands)
        .into_par_iter()
        .map(|b| -> Result<Band> {
            progress.check()?;
            let r0 = b * rows_per_band;
            let r1 = (r0 + rows_per_band).min(grid.height as usize);
            let y0 = oy + r0 as f64 * bin_um;
            let y1 = oy + r1 as f64 * bin_um;
            let ty0 = (y0 / ts).floor() as i32;
            let ty1 = ((y1 / ts).ceil() as i32 - 1).max(ty0);
            let nb = (r1 - r0) * w;
            let mut bins: Vec<Vec<(u32, f64)>> = vec![vec![]; nb];
            let mut counts = vec![0u32; nb];
            let mut touched = vec![];
            for (gi, &g) in genes.iter().enumerate() {
                if gi % 64 == 0 {
                    progress.check()?;
                }
                let raw = store.gene_records_in_rows(g, ty0, ty1)?;
                for r in raw.chunks_exact(POINT_RECORD_BYTES) {
                    if r[14] < qv_q {
                        continue;
                    }
                    let x = f32::from_le_bytes(r[0..4].try_into().unwrap()) as f64;
                    let y = f32::from_le_bytes(r[4..8].try_into().unwrap()) as f64;
                    let row = ((y - oy) / bin_um).floor();
                    let col = ((x - ox) / bin_um).floor();
                    if row < r0 as f64 || row >= r1 as f64 || col < 0.0 || col >= w as f64 {
                        continue;
                    }
                    let i = (row as usize - r0) * w + col as usize;
                    if counts[i] == 0 {
                        touched.push(i);
                    }
                    counts[i] += 1;
                }
                for &i in &touched {
                    bins[i].push((g, counts[i] as f64));
                    counts[i] = 0;
                }
                touched.clear();
            }
            let (mut scratch, mut row) = (vec![], vec![0.0; k]);
            let mut ntx = vec![0u32; nb];
            let mut vals = vec![f64::NAN; nb * k];
            for (i, e) in bins.iter().enumerate() {
                ntx[i] = e.iter().map(|x| x.1 as u32).sum();
                if ntx[i] >= min_transcripts.max(1) {
                    scorer.score_unit(e, &mut scratch, &mut row);
                    vals[i * k..(i + 1) * k].copy_from_slice(&row);
                }
            }
            progress.add(1);
            Ok((r0 * w, ntx, vals))
        })
        .collect::<Result<_>>()?;
    let mut scores = vec![vec![f64::NAN; grid.len()]; k];
    let mut n_transcripts = vec![0u32; grid.len()];
    for (start, ntx, vals) in bands {
        for (i, n) in ntx.iter().enumerate() {
            n_transcripts[start + i] = *n;
            for (s, col) in scores.iter_mut().enumerate() {
                col[start + i] = vals[i * k + s];
            }
        }
    }
    Ok(BinScores {
        grid,
        scores,
        n_transcripts,
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoreStats {
    pub n_valid: usize,
    pub n_missing: usize,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub mean: Option<f64>,
    pub p01: Option<f64>,
    pub p50: Option<f64>,
    pub p99: Option<f64>,
}

pub fn stats(v: &[f64]) -> ScoreStats {
    let mut f: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if f.is_empty() {
        return ScoreStats {
            n_missing: v.len(),
            ..Default::default()
        };
    }
    f.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| f[((f.len() - 1) as f64 * p).round() as usize];
    ScoreStats {
        n_valid: f.len(),
        n_missing: v.len() - f.len(),
        min: Some(f[0]),
        max: Some(f[f.len() - 1]),
        mean: Some(f.iter().sum::<f64>() / f.len() as f64),
        p01: Some(q(0.01)),
        p50: Some(q(0.5)),
        p99: Some(q(0.99)),
    }
}
