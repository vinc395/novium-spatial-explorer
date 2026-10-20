//! Parity with the reference R implementation (UCell 2.14.0).
//!
//! * `golden_cases_match_r` always runs: it scores the cases stored in
//!   `tests/fixtures/ucell_r_golden.json` (inputs plus scores *computed by R*) and
//!   requires bit-identical results.
//! * `live_cases_match_r` runs when `XENIUM_UCELL_RSCRIPT` points to an `Rscript`
//!   with UCell installed (see `scripts/verify.sh`). It generates the cases below,
//!   runs `scripts/ucell_reference.R` on each, and compares. With
//!   `XENIUM_UCELL_WRITE_GOLDEN=1` it also rewrites the golden file from R's output.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use xenium_score::ucell::{MissingGenes, Panel, Scorer, UcellParams};

struct TestPanel {
    ids: HashMap<String, u32>,
    n: usize,
}
impl Panel for TestPanel {
    fn n_genes(&self) -> usize {
        self.n
    }
    fn lookup(&self, name: &str) -> Option<u32> {
        self.ids.get(name).copied()
    }
    fn is_gene(&self, id: u32) -> bool {
        (id as usize) < self.n
    }
}

#[derive(Clone)]
struct Case {
    name: String,
    genes: Vec<String>,
    /// Per unit: `(feature id, value)` sorted by id. Ids >= genes.len() are control
    /// features: given to Rust only (R's matrix holds panel genes), so they must not
    /// change any rank.
    units: Vec<Vec<(u32, f64)>>,
    sigs: Vec<(String, Vec<String>)>,
    params: UcellParams,
    /// Stored in the golden file (small cases only).
    golden: bool,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn gene_names(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("G{i:03}")).collect()
}

/// Small integer counts with many zeros and ties; optional all-zero / all-expressed units.
fn counts(
    rng: &mut Rng,
    g: usize,
    n: usize,
    p_zero: f64,
    max: u64,
    extremes: bool,
) -> Vec<Vec<(u32, f64)>> {
    (0..n)
        .map(|u| {
            if extremes && u % 10 == 3 {
                return vec![];
            }
            let all = extremes && u % 10 == 7;
            (0..g)
                .filter_map(|i| {
                    if !all && rng.f() < p_zero {
                        return None;
                    }
                    Some((i as u32, (1 + rng.next() % max) as f64))
                })
                .collect()
        })
        .collect()
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn cases() -> Vec<Case> {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let mut out = vec![];
    // 1. Tiny panel: maxRank is capped at 12 and the zero-tie rank (k + (G-k+1)/2)
    //    is below maxRank, so unexpressed genes do not all score alike.
    let g12 = gene_names(12);
    let units = counts(&mut rng, 12, 80, 0.6, 4, true);
    let sigs12 = vec![
        ("plain".into(), s(&["G001", "G002", "G003"])),
        (
            "dups_plus".into(),
            s(&["G003", "G003", "G005+", "G005", "G006"]),
        ),
        (
            "with_neg".into(),
            s(&["G001", "G004", "G007-", "G008-", "G008-"]),
        ),
        ("missing".into(), s(&["G002", "NOPE1", "NOPE2-", "G009"])),
        ("only_neg".into(), s(&["G010-", "G011-"])),
        ("single".into(), s(&["G012"])),
        ("whole_panel".into(), g12.clone()),
    ];
    let base = Case {
        name: "tiny_panel_impute".into(),
        genes: g12.clone(),
        units,
        sigs: sigs12,
        params: UcellParams::default(),
        golden: true,
    };
    out.push(base.clone());
    out.push(Case {
        name: "tiny_panel_skip_wneg_half".into(),
        params: UcellParams {
            max_rank: 1500,
            w_neg: 0.5,
            missing_genes: MissingGenes::Skip,
        },
        ..base.clone()
    });
    // 2. maxRank below the panel size, plus control features with high counts.
    let g40 = gene_names(40);
    let mut units = counts(&mut rng, 40, 120, 0.5, 12, true);
    for u in units.iter_mut() {
        u.push((40, 99.0));
        u.push((41, (rng.next() % 30) as f64));
    }
    out.push(Case {
        name: "max_rank_10_with_controls".into(),
        genes: g40.clone(),
        units,
        sigs: vec![
            ("a".into(), s(&["G001", "G002", "G003", "G004", "G005"])),
            (
                "b".into(),
                s(&["G010", "G020", "G030", "G040", "G011-", "G012-"]),
            ),
            ("ten".into(), g40[..10].to_vec()),
            ("missing_only".into(), s(&["NOPE"])),
        ],
        params: UcellParams {
            max_rank: 10,
            w_neg: 1.0,
            missing_genes: MissingGenes::Impute,
        },
        golden: true,
    });
    // 3. maxRank 1: a one-gene signature is 0/0 = NaN in R.
    out.push(Case {
        name: "max_rank_1_nan".into(),
        genes: gene_names(8),
        units: counts(&mut rng, 8, 20, 0.5, 3, false),
        sigs: vec![
            ("one".into(), s(&["G001"])),
            ("one_missing".into(), s(&["NOPE"])),
        ],
        params: UcellParams {
            max_rank: 1,
            w_neg: 1.0,
            missing_genes: MissingGenes::Impute,
        },
        golden: true,
    });
    // 4. Non-integer (log-normalised) values; ties only between equal counts.
    let g30 = gene_names(30);
    let units: Vec<Vec<(u32, f64)>> = counts(&mut rng, 30, 60, 0.55, 6, true)
        .into_iter()
        .map(|u| {
            let total: f64 = u.iter().map(|e| e.1).sum();
            u.into_iter()
                .map(|(g, c)| (g, (c / total * 1e4).ln_1p()))
                .collect()
        })
        .collect();
    out.push(Case {
        name: "lognorm_values".into(),
        genes: g30.clone(),
        units,
        sigs: vec![
            ("x".into(), s(&["G002", "G004", "G006", "G008"])),
            ("y".into(), s(&["G001", "G003-", "G005-"])),
        ],
        params: UcellParams {
            max_rank: 20,
            w_neg: 1.0,
            missing_genes: MissingGenes::Impute,
        },
        golden: true,
    });
    // 5. Larger, Xenium-like sparsity (live only).
    let g300 = gene_names(300);
    let mut sigs = vec![];
    for k in 0..12 {
        let len = 3 + rng.below(40);
        let mut genes: Vec<String> = (0..len).map(|_| g300[rng.below(300)].clone()).collect();
        if k % 3 == 0 {
            genes.push(format!("{}-", g300[rng.below(300)]));
            genes.push("ABSENT".into());
        }
        sigs.push((format!("sig{k}"), genes));
    }
    out.push(Case {
        name: "xenium_like_300x2000".into(),
        genes: g300,
        units: counts(&mut rng, 300, 2000, 0.85, 20, true),
        sigs,
        params: UcellParams {
            max_rank: 150,
            w_neg: 1.0,
            missing_genes: MissingGenes::Impute,
        },
        golden: false,
    });
    out
}

fn rust_scores(c: &Case) -> anyhow::Result<Vec<Vec<f64>>> {
    let panel = TestPanel {
        ids: c
            .genes
            .iter()
            .enumerate()
            .map(|(i, g)| (g.clone(), i as u32))
            .collect(),
        n: c.genes.len(),
    };
    let max_id = c
        .units
        .iter()
        .flatten()
        .map(|e| e.0)
        .max()
        .unwrap_or(0)
        .max(c.genes.len() as u32);
    let sigs: Vec<Vec<String>> = c.sigs.iter().map(|s| s.1.clone()).collect();
    let sc = Scorer::new(&panel, max_id, &sigs, &c.params)?;
    let mut out = vec![vec![0.0; c.units.len()]; sigs.len()];
    let mut scratch = vec![];
    let mut row = vec![0.0; sigs.len()];
    for (u, e) in c.units.iter().enumerate() {
        sc.score_unit(e, &mut scratch, &mut row);
        for (k, v) in row.iter().enumerate() {
            out[k][u] = *v;
        }
    }
    Ok(out)
}

fn same(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

fn compare(name: &str, rust: &[Vec<f64>], r: &[Vec<f64>]) {
    assert_eq!(rust.len(), r.len(), "{name}: signature count");
    let mut worst = 0.0f64;
    let mut n_diff = 0;
    for (k, (a, b)) in rust.iter().zip(r).enumerate() {
        assert_eq!(a.len(), b.len(), "{name}: unit count");
        for (u, (x, y)) in a.iter().zip(b).enumerate() {
            if !same(*x, *y) {
                n_diff += 1;
                worst = worst.max((x - y).abs());
                if n_diff <= 3 {
                    eprintln!("{name}: sig {k} unit {u}: rust {x:e} vs R {y:e}");
                }
            }
        }
    }
    assert_eq!(
        n_diff, 0,
        "{name}: {n_diff} scores differ from R (max |diff| {worst:e})"
    );
}

// ---------------------------------------------------------------- golden

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ucell_r_golden.json")
}

fn case_json(c: &Case, r: &[Vec<f64>]) -> Value {
    let entries: Vec<Value> = c
        .units
        .iter()
        .enumerate()
        .flat_map(|(u, e)| e.iter().map(move |(g, v)| json!([u, g, v])))
        .collect();
    json!({
        "name": c.name,
        "genes": c.genes,
        "n_units": c.units.len(),
        "entries": entries,
        "signatures": c.sigs.iter().map(|(n, g)| json!([n, g])).collect::<Vec<_>>(),
        "params": c.params,
        "r_scores": r.iter().map(|col| col.iter().map(|v| format!("{v:e}")).collect::<Vec<_>>()).collect::<Vec<_>>(),
    })
}

#[test]
fn golden_cases_match_r() {
    let doc: Value = serde_json::from_slice(&std::fs::read(golden_path()).unwrap()).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 5, "golden file has too few cases");
    for c in cases {
        let n_units = c["n_units"].as_u64().unwrap() as usize;
        let mut units = vec![vec![]; n_units];
        for e in c["entries"].as_array().unwrap() {
            units[e[0].as_u64().unwrap() as usize]
                .push((e[1].as_u64().unwrap() as u32, e[2].as_f64().unwrap()));
        }
        let case = Case {
            name: c["name"].as_str().unwrap().into(),
            genes: serde_json::from_value(c["genes"].clone()).unwrap(),
            units,
            sigs: serde_json::from_value(c["signatures"].clone()).unwrap(),
            params: serde_json::from_value(c["params"].clone()).unwrap(),
            golden: true,
        };
        let r: Vec<Vec<f64>> = c["r_scores"]
            .as_array()
            .unwrap()
            .iter()
            .map(|col| {
                col.as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().parse().unwrap())
                    .collect()
            })
            .collect();
        compare(&case.name, &rust_scores(&case).unwrap(), &r);
    }
    // Errors are part of the contract too: R stops on a signature longer than maxRank.
    let err = cases_error();
    assert!(rust_scores(&err).is_err());
}

fn cases_error() -> Case {
    Case {
        name: "signature_longer_than_max_rank".into(),
        genes: gene_names(5),
        units: vec![vec![(0, 1.0)]],
        sigs: vec![(
            "too_long".into(),
            s(&["G001", "G002", "G003", "G004", "G005", "G001"]),
        )],
        params: UcellParams::default(),
        golden: false,
    }
}

// ---------------------------------------------------------------- live R

fn write_case(dir: &Path, c: &Case) {
    use std::fmt::Write;
    let g = c.genes.len() as u32;
    let nnz: usize = c.units.iter().flatten().filter(|e| e.0 < g).count();
    let mut mtx = format!(
        "%%MatrixMarket matrix coordinate real general\n{} {} {}\n",
        g,
        c.units.len(),
        nnz
    );
    for (u, e) in c.units.iter().enumerate() {
        for (id, v) in e.iter().filter(|e| e.0 < g) {
            writeln!(mtx, "{} {} {:e}", id + 1, u + 1, v).unwrap();
        }
    }
    std::fs::write(dir.join("matrix.mtx"), mtx).unwrap();
    std::fs::write(dir.join("genes.txt"), c.genes.join("\n") + "\n").unwrap();
    let units: Vec<String> = (0..c.units.len()).map(|u| format!("u{u:05}")).collect();
    std::fs::write(dir.join("units.txt"), units.join("\n") + "\n").unwrap();
    let sigs: serde_json::Map<String, Value> =
        c.sigs.iter().map(|(n, g)| (n.clone(), json!(g))).collect();
    std::fs::write(dir.join("signatures.json"), Value::Object(sigs).to_string()).unwrap();
    std::fs::write(
        dir.join("params.json"),
        serde_json::to_string(&c.params).unwrap(),
    )
    .unwrap();
}

/// R's scores in the case's signature order (the JSON object R reads has sorted keys,
/// so R's columns are matched by name).
fn run_r(rscript: &str, dir: &Path, c: &Case) -> anyhow::Result<Vec<Vec<f64>>> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ucell_reference.R");
    let o = std::process::Command::new(rscript)
        .arg(&script)
        .arg(dir)
        .output()?;
    if !o.status.success() {
        anyhow::bail!("R failed: {}", String::from_utf8_lossy(&o.stderr));
    }
    let text = std::fs::read_to_string(dir.join("r_scores.tsv"))?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap().split('\t').skip(1).collect();
    let mut cols = vec![vec![]; header.len()];
    for l in lines {
        for (k, v) in l.split('\t').skip(1).enumerate() {
            cols[k].push(v.parse::<f64>()?);
        }
    }
    c.sigs
        .iter()
        .map(|(name, _)| {
            let k = header
                .iter()
                .position(|h| h == name)
                .ok_or_else(|| anyhow::anyhow!("R output has no column {name}"))?;
            Ok(std::mem::take(&mut cols[k]))
        })
        .collect()
}

#[test]
fn live_cases_match_r() {
    let Ok(rscript) = std::env::var("XENIUM_UCELL_RSCRIPT") else {
        eprintln!(
            "XENIUM_UCELL_RSCRIPT not set: live R comparison skipped (golden test still runs)"
        );
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    // XENIUM_UCELL_KEEP=<dir> keeps the generated inputs and R outputs for inspection.
    let root = std::env::var("XENIUM_UCELL_KEEP")
        .map(PathBuf::from)
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let mut golden = vec![];
    for c in cases() {
        let dir = root.join(&c.name);
        std::fs::create_dir_all(&dir).unwrap();
        write_case(&dir, &c);
        let r = run_r(&rscript, &dir, &c).unwrap_or_else(|e| panic!("{}: {e}", c.name));
        compare(&c.name, &rust_scores(&c).unwrap(), &r);
        let n = c.units.len() * c.sigs.len();
        eprintln!("{}: {} scores identical to R", c.name, n);
        if c.golden {
            golden.push(case_json(&c, &r));
        }
    }
    // Both implementations refuse a signature longer than maxRank.
    let err = cases_error();
    let dir = root.join(&err.name);
    std::fs::create_dir_all(&dir).unwrap();
    write_case(&dir, &err);
    assert!(
        run_r(&rscript, &dir, &err).is_err(),
        "R accepted a signature longer than maxRank"
    );
    assert!(rust_scores(&err).is_err());
    if std::env::var("XENIUM_UCELL_WRITE_GOLDEN").as_deref() == Ok("1") {
        let version = std::process::Command::new(&rscript)
            .args(["-e", "cat(as.character(packageVersion('UCell')))"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let doc = json!({
            "about": "Inputs and R UCell scores for crates/xenium-score/tests/r_parity.rs. Generated by live_cases_match_r with XENIUM_UCELL_WRITE_GOLDEN=1; r_scores come from R, not from xenium-score.",
            "ucell_version": version,
            "cases": golden,
        });
        std::fs::write(golden_path(), serde_json::to_string(&doc).unwrap() + "\n").unwrap();
        eprintln!("wrote {}", golden_path().display());
    }
}
