//! UCell signature scores (Andreatta & Carmona, 2021), computed exactly as the
//! reference R package does (`UCell` 2.14.0; `calculate_Uscore`, `u_stat`,
//! `u_stat_signature_list`, `get_gene_idx` — unchanged in 2.17 devel).
//!
//! For one unit (a cell, or a spatial bin), every gene of the measured panel is
//! ranked by decreasing expression with average ranks for ties
//! (`data.table::frankv(x, ties.method = "average", order = -1L)`). Genes with zero
//! counts tie at the bottom: with `k` expressed genes out of `G`, they all get rank
//! `k + (G - k + 1) / 2`. For a gene set of length `n` (after missing-gene handling):
//!
//! ```text
//! rank_sum = n_missing * maxRank + Σ_present min(rank, maxRank)
//! score    = 1 - (rank_sum - n(n+1)/2) / (n * maxRank - n(n+1)/2)
//! ```
//!
//! and the signature score is `max(0, score(positive) - w_neg * score(negative))`
//! (a NaN stays NaN, as in R). Ranks are half-integers, so every sum is exact in
//! `f64` and the arithmetic below follows R's operation order: results agree with
//! R to the last bit.
//!
//! R parity details kept on purpose:
//! * genes ending in `-` are negative (duplicates kept); the rest are de-duplicated
//!   (`setdiff`) before one trailing `+` is stripped;
//! * `maxRank` is capped at the number of panel genes; a signature with more raw
//!   entries than `maxRank` is an error;
//! * `missing_genes = "impute"` (R's default) counts a gene that is not in the panel
//!   at `maxRank`; `"skip"` drops it; an empty set scores 0.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Version tag stored with cached results; bump when scores could change.
pub const ALGORITHM: &str = "ucell-r2.14-v1";
/// UCell's default rank cutoff.
pub const DEFAULT_MAX_RANK: u32 = 1500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissingGenes {
    /// Count genes absent from the panel at `maxRank` (R default).
    Impute,
    /// Drop genes absent from the panel from the signature.
    Skip,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UcellParams {
    pub max_rank: u32,
    pub w_neg: f64,
    pub missing_genes: MissingGenes,
}

impl Default for UcellParams {
    fn default() -> Self {
        UcellParams {
            max_rank: DEFAULT_MAX_RANK,
            w_neg: 1.0,
            missing_genes: MissingGenes::Impute,
        }
    }
}

/// Positive and negative gene names of one signature, parsed as R does.
#[derive(Clone, Debug, PartialEq)]
pub struct SplitSignature {
    pub positive: Vec<String>,
    pub negative: Vec<String>,
}

pub fn split_signature(genes: &[String]) -> SplitSignature {
    // sig_neg <- grep('-$', sig, value = TRUE)   (order and duplicates kept)
    let negative: Vec<String> = genes
        .iter()
        .filter(|g| g.ends_with('-'))
        .map(|g| g[..g.len() - 1].to_string())
        .collect();
    // sig_pos <- setdiff(sig, sig_neg)            (unique, first occurrence)
    let mut positive: Vec<String> = vec![];
    let mut seen = std::collections::HashSet::new();
    for g in genes.iter().filter(|g| !g.ends_with('-')) {
        if seen.insert(g.as_str()) {
            positive.push(g.strip_suffix('+').unwrap_or(g).to_string());
        }
    }
    SplitSignature { positive, negative }
}

/// One gene set resolved against the panel: feature ids of present genes (in
/// signature order, duplicates kept) and the number of imputed missing genes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GeneSet {
    pub present: Vec<u32>,
    pub n_missing: usize,
}

impl GeneSet {
    pub fn len(&self) -> usize {
        self.present.len() + self.n_missing
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The measured panel a unit is ranked over.
pub trait Panel {
    /// Number of genes ranked per unit (the rows of R's matrix).
    fn n_genes(&self) -> usize;
    /// Feature id of a gene name, if it is a panel gene.
    fn lookup(&self, name: &str) -> Option<u32>;
    /// Whether a feature id is a panel gene (other features are ignored when ranking).
    fn is_gene(&self, id: u32) -> bool;
}

/// Signatures resolved for one panel and parameter set.
#[derive(Clone, Debug)]
pub struct Scorer {
    pub n_genes: usize,
    /// `min(max_rank, n_genes)`, as R caps it.
    pub max_rank: f64,
    pub w_neg: f64,
    pub sets: Vec<(GeneSet, GeneSet)>,
    is_gene: Vec<bool>,
}

fn resolve(names: &[String], panel: &dyn Panel, missing: MissingGenes) -> GeneSet {
    let mut s = GeneSet::default();
    for n in names {
        match panel.lookup(n) {
            Some(id) => s.present.push(id),
            None if missing == MissingGenes::Impute => s.n_missing += 1,
            None => {}
        }
    }
    s
}

impl Scorer {
    /// `signatures` are raw gene lists (with `+`/`-` suffixes), one per signature.
    /// `max_feature_id` bounds the feature ids that `score_unit` will see.
    pub fn new(
        panel: &dyn Panel,
        max_feature_id: u32,
        signatures: &[Vec<String>],
        p: &UcellParams,
    ) -> Result<Self> {
        let g = panel.n_genes();
        if g == 0 {
            bail!("the panel has no genes to rank");
        }
        if p.max_rank < 1 {
            bail!("maxRank must be at least 1");
        }
        if !p.w_neg.is_finite() || p.w_neg < 0.0 {
            bail!("Weight on negative signatures (w_neg) must be >=0");
        }
        let max_rank = p.max_rank.min(g as u32) as f64;
        if signatures.iter().any(|s| s.len() as f64 > max_rank) {
            bail!(
                "One or more signatures contain more genes than maxRank ({max_rank}). \
                 Increase maxRank or make shorter signatures"
            );
        }
        let sets = signatures
            .iter()
            .map(|s| {
                let sp = split_signature(s);
                (
                    resolve(&sp.positive, panel, p.missing_genes),
                    resolve(&sp.negative, panel, p.missing_genes),
                )
            })
            .collect();
        let is_gene = (0..=max_feature_id).map(|i| panel.is_gene(i)).collect();
        Ok(Scorer {
            n_genes: g,
            max_rank,
            w_neg: p.w_neg,
            sets,
            is_gene,
        })
    }

    pub fn n_signatures(&self) -> usize {
        self.sets.len()
    }

    /// Score one unit. `entries` are `(feature id, value)` sorted by feature id with
    /// unique ids; values must be finite and >= 0 (counts). Non-gene features are
    /// ignored. `scratch` is reused between calls; `out` gets one score per signature.
    pub fn score_unit(&self, entries: &[(u32, f64)], scratch: &mut Vec<f64>, out: &mut [f64]) {
        scratch.clear();
        scratch.extend(
            entries
                .iter()
                .filter(|e| e.1 > 0.0 && self.is_gene.get(e.0 as usize) == Some(&true))
                .map(|e| e.1),
        );
        // Decreasing order (values are finite).
        scratch.sort_unstable_by(|a, b| b.partial_cmp(a).unwrap());
        let sorted: &[f64] = scratch;
        let k = sorted.len();
        let zero_rank = k as f64 + ((self.n_genes - k) as f64 + 1.0) / 2.0;
        let rank = |id: u32| -> f64 {
            let v = match entries.binary_search_by_key(&id, |e| e.0) {
                Ok(i) => entries[i].1,
                Err(_) => 0.0,
            };
            if v > 0.0 {
                let gt = sorted.partition_point(|&x| x > v);
                let ge = sorted.partition_point(|&x| x >= v);
                gt as f64 + ((ge - gt) as f64 + 1.0) / 2.0
            } else {
                zero_rank
            }
        };
        for (o, (pos, neg)) in out.iter_mut().zip(&self.sets) {
            let up = u_stat(pos, self.max_rank, rank);
            let un = u_stat(neg, self.max_rank, rank);
            let diff = up - self.w_neg * un;
            *o = if diff < 0.0 { 0.0 } else { diff };
        }
    }
}

/// R's `u_stat` for one unit.
fn u_stat(set: &GeneSet, max_rank: f64, rank: impl Fn(u32) -> f64) -> f64 {
    let len = set.len();
    if len == 0 {
        return 0.0;
    }
    let mut rank_sum = set.n_missing as f64 * max_rank;
    if !set.present.is_empty() {
        let mut s = 0.0;
        for &g in &set.present {
            let r = rank(g);
            s += if r >= max_rank { max_rank } else { r };
        }
        rank_sum += s;
    }
    let rank_sum_min = (len * (len + 1)) as f64 / 2.0;
    1.0 - (rank_sum - rank_sum_min) / (len as f64 * max_rank - rank_sum_min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct P(HashMap<String, u32>, usize);
    impl Panel for P {
        fn n_genes(&self) -> usize {
            self.1
        }
        fn lookup(&self, n: &str) -> Option<u32> {
            self.0.get(n).copied()
        }
        fn is_gene(&self, id: u32) -> bool {
            (id as usize) < self.1
        }
    }
    fn panel(n: usize) -> P {
        P((0..n).map(|i| (format!("G{i}"), i as u32)).collect(), n)
    }
    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }
    fn score(p: &P, entries: &[(u32, f64)], sig: &[&str], params: &UcellParams) -> f64 {
        let sc = Scorer::new(p, p.1 as u32, &[s(sig)], params).unwrap();
        let mut out = [0.0];
        sc.score_unit(entries, &mut vec![], &mut out);
        out[0]
    }

    #[test]
    fn split_follows_r() {
        let sp = split_signature(&s(&["A", "B-", "A", "C+", "B-", "C", "D+"]));
        assert_eq!(sp.negative, s(&["B", "B"]));
        // "C+" and "C" are different strings for setdiff, then both become "C".
        assert_eq!(sp.positive, s(&["A", "C", "C", "D"]));
    }

    #[test]
    fn hand_computed_ranks_and_ties() {
        // G = 10 genes; cell: G0=5, G1=3, G2=3, G3=1, others 0.
        // Ranks: G0 1, G1/G2 2.5, G3 4, zeros (6 genes) = 4 + 7/2 = 7.5.
        let p = panel(10);
        let e = [(0, 5.0), (1, 3.0), (2, 3.0), (3, 1.0)];
        let prm = UcellParams {
            max_rank: 10,
            ..Default::default()
        };
        // {G0, G1}: rank_sum 3.5, min 3, max 2*10-3 = 17 -> 1 - 0.5/17.
        assert_eq!(score(&p, &e, &["G0", "G1"], &prm), 1.0 - 0.5 / 17.0);
        // {G5}: zero rank 7.5 -> 1 - 6.5/9.
        assert_eq!(score(&p, &e, &["G5"], &prm), 1.0 - 6.5 / 9.0);
        // maxRank 5 caps the zero rank at 5 -> score 0 for an unexpressed gene.
        let prm5 = UcellParams {
            max_rank: 5,
            ..Default::default()
        };
        assert_eq!(score(&p, &e, &["G5"], &prm5), 0.0);
        // maxRank above the panel size is capped at G.
        let big = UcellParams {
            max_rank: 1500,
            ..Default::default()
        };
        assert_eq!(score(&p, &e, &["G5"], &big), 1.0 - 6.5 / 9.0);
    }

    #[test]
    fn missing_genes_and_negatives() {
        let p = panel(10);
        let e = [(0, 5.0), (1, 3.0)];
        let imp = UcellParams {
            max_rank: 10,
            ..Default::default()
        };
        let skip = UcellParams {
            missing_genes: MissingGenes::Skip,
            ..imp.clone()
        };
        // Impute: {G0 (1), NOPE (10)}: rank_sum 11, min 3, max 17 -> 1 - 8/17.
        assert_eq!(score(&p, &e, &["G0", "NOPE"], &imp), 1.0 - 8.0 / 17.0);
        // Skip: {G0}: 1 - 0/9 = 1.
        assert_eq!(score(&p, &e, &["G0", "NOPE"], &skip), 1.0);
        // Only missing genes with skip: empty set scores 0.
        assert_eq!(score(&p, &e, &["NOPE"], &skip), 0.0);
        // Negative set larger than positive: clipped at 0.
        assert_eq!(score(&p, &e, &["G5", "G0-"], &imp), 0.0);
        // w_neg = 0.5: 1 - 0.5 * u(G1) with G1 at rank 2 -> u = 1 - 1/9.
        let half = UcellParams {
            w_neg: 0.5,
            ..imp.clone()
        };
        assert_eq!(
            score(&p, &e, &["G0", "G1-"], &half),
            1.0 - 0.5 * (1.0 - 1.0 / 9.0)
        );
    }

    #[test]
    fn non_gene_features_are_not_ranked() {
        // Feature 12 (a control probe) has the highest count but is not a panel gene.
        let p = panel(10);
        let e = [(0, 5.0), (12, 50.0)];
        let prm = UcellParams {
            max_rank: 10,
            ..Default::default()
        };
        let sc = Scorer::new(&p, 12, &[s(&["G0"])], &prm).unwrap();
        let mut out = [0.0];
        sc.score_unit(&e, &mut vec![], &mut out);
        assert_eq!(out[0], 1.0);
    }

    #[test]
    fn signature_longer_than_max_rank_is_an_error() {
        let p = panel(3);
        let prm = UcellParams {
            max_rank: 1500,
            ..Default::default()
        };
        let err = Scorer::new(&p, 3, &[s(&["G0", "G1", "G2", "G0"])], &prm).unwrap_err();
        assert!(err.to_string().contains("more genes than maxRank"));
        assert!(Scorer::new(&p, 3, &[s(&["G0"])], &UcellParams { w_neg: -1.0, ..prm }).is_err());
    }

    #[test]
    fn single_gene_with_max_rank_one_is_nan_as_in_r() {
        // 1 - (1 - 1) / (1 * 1 - 1) = 1 - 0/0 = NaN; R keeps NaN (NaN < 0 is NA).
        let p = panel(5);
        let prm = UcellParams {
            max_rank: 1,
            ..Default::default()
        };
        assert!(score(&p, &[(0, 1.0)], &["G0"], &prm).is_nan());
    }
}
