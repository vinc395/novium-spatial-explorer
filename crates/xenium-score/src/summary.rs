//! Cell-level scores summarised per category of a cell field (e.g. cell type,
//! niche label) or of a segmentation-free grid field (e.g. a niche map). For a grid,
//! each cell takes the category of the grid square containing its centroid
//! (nearest-neighbour join, the same rule the grid is drawn with).

use anyhow::{bail, Result};
use serde::Serialize;
use xenium_core::codec::{decode_field, decode_grid_raster, FieldColumn};
use xenium_core::manifest::FieldKind;
use xenium_index::SectionStore;

#[derive(Debug, Clone)]
pub enum GroupBy {
    Field(String),
    Grid { grid: String, field: String },
}

impl GroupBy {
    /// `field:<id>` or `grid:<grid id>/<field id>`.
    pub fn parse(s: &str) -> Result<Self> {
        if let Some(f) = s.strip_prefix("field:") {
            return Ok(GroupBy::Field(f.to_string()));
        }
        if let Some((g, f)) = s.strip_prefix("grid:").and_then(|r| r.split_once('/')) {
            return Ok(GroupBy::Grid {
                grid: g.into(),
                field: f.into(),
            });
        }
        bail!("group by must be field:<id> or grid:<grid>/<field>")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupStats {
    pub name: String,
    pub n: usize,
    pub mean: f64,
    pub sd: f64,
    pub min: f64,
    pub p05: f64,
    pub q1: f64,
    pub median: f64,
    pub q3: f64,
    pub p95: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub group_by: String,
    pub join: String,
    pub groups: Vec<GroupStats>,
    /// Cells with a score but no category (missing label or outside the grid).
    pub n_unassigned: usize,
    /// Cells without a score.
    pub n_no_score: usize,
    pub overall: Option<GroupStats>,
}

fn group_stats(name: &str, v: &mut [f64]) -> Option<GroupStats> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let mean = v.iter().sum::<f64>() / n as f64;
    let sd = if n > 1 {
        (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
    } else {
        0.0
    };
    // Linear interpolation between order statistics (R's default quantile type 7).
    let q = |p: f64| {
        let h = (n - 1) as f64 * p;
        let (lo, hi) = (h.floor() as usize, h.ceil() as usize);
        v[lo] + (h - lo as f64) * (v[hi] - v[lo])
    };
    Some(GroupStats {
        name: name.to_string(),
        n,
        mean,
        sd,
        min: v[0],
        p05: q(0.05),
        q1: q(0.25),
        median: q(0.5),
        q3: q(0.75),
        p95: q(0.95),
        max: v[n - 1],
    })
}

/// Per-category statistics of cell-level `scores` (index = cell_key - 1).
pub fn summarize(store: &SectionStore, scores: &[f64], by: &GroupBy) -> Result<Summary> {
    let m = &store.manifest;
    let n = store.n_cells();
    if scores.len() != n {
        bail!("scores do not match the section's cells");
    }
    let (codes, names, label, join): (Vec<u32>, Vec<String>, String, String) = match by {
        GroupBy::Field(id) => {
            let f = m
                .field(id)
                .ok_or_else(|| anyhow::anyhow!("no cell field {id:?}"))?;
            if f.kind != FieldKind::Categorical {
                bail!("{id} is not categorical");
            }
            let FieldColumn::Categorical { codes, .. } = decode_field(&store.field_bytes(id)?)?
            else {
                bail!("{id} is not categorical");
            };
            let names = f
                .categories
                .iter()
                .flatten()
                .map(|c| c.name.clone())
                .collect();
            (codes, names, f.label.clone(), "cell field".into())
        }
        GroupBy::Grid { grid, field } => {
            let g = m
                .grid(grid)
                .ok_or_else(|| anyhow::anyhow!("no grid {grid:?}"))?;
            let f = g
                .fields
                .iter()
                .find(|f| &f.id == field)
                .ok_or_else(|| anyhow::anyhow!("no grid field {field:?}"))?;
            if f.kind != FieldKind::Categorical {
                bail!("{grid}/{field} is not categorical");
            }
            let (w, h, col) = decode_grid_raster(&store.grid_raster(grid, field)?)?;
            let FieldColumn::Categorical { codes: raster, .. } = col else {
                bail!("{grid}/{field} is not categorical");
            };
            let mut codes = vec![0u32; n];
            for (i, c) in codes.iter_mut().enumerate() {
                let p = store.centroid(i as u32 + 1).unwrap();
                let gx = ((p[0] as f64 - g.origin_um[0]) / g.spacing_um).floor();
                let gy = ((p[1] as f64 - g.origin_um[1]) / g.spacing_um).floor();
                if gx >= 0.0 && gy >= 0.0 && (gx as u32) < w && (gy as u32) < h {
                    *c = raster[gy as usize * w as usize + gx as usize];
                }
            }
            let names = f
                .categories
                .iter()
                .flatten()
                .map(|c| c.name.clone())
                .collect();
            (
                codes,
                names,
                format!("{}: {}", g.label, f.label),
                format!(
                    "grid square containing the cell centroid ({} µm)",
                    g.spacing_um
                ),
            )
        }
    };
    let mut per: Vec<Vec<f64>> = vec![vec![]; names.len()];
    let (mut unassigned, mut no_score, mut all) = (0, 0, vec![]);
    for (i, &s) in scores.iter().enumerate() {
        if !s.is_finite() {
            no_score += 1;
            continue;
        }
        all.push(s);
        match codes[i] as usize {
            c if c >= 1 && c <= names.len() => per[c - 1].push(s),
            _ => unassigned += 1,
        }
    }
    let groups = names
        .iter()
        .zip(per.iter_mut())
        .filter_map(|(nm, v)| group_stats(nm, v))
        .collect();
    Ok(Summary {
        group_by: label,
        join,
        groups,
        n_unassigned: unassigned,
        n_no_score: no_score,
        overall: group_stats("all cells", &mut all),
    })
}
