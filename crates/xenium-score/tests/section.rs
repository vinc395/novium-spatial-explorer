//! UCell over a synthetic section: chunked cell scoring, bin scoring against a
//! brute-force recount of the raw transcripts, jobs, cache, summaries, Parquet export,
//! and (with `XENIUM_UCELL_RSCRIPT`) parity with R on the section's expression.

use arrow::array::{Array, AsArray};
use arrow::datatypes::Float64Type;
use std::collections::HashMap;
use std::path::Path;
use xenium_index::{ingest, GridInput, IngestConfig, SectionStore};
use xenium_io::synth::{generate, SynthOptions};
use xenium_io::transcripts::stream_transcripts;
use xenium_score::cache::{Level, ResultCache, SignatureSnapshot};
use xenium_score::engine::{run, JobSpec};
use xenium_score::export::{bins_parquet, cells_parquet, METADATA_KEY};
use xenium_score::progress::{Cancelled, Progress};
use xenium_score::section::{coverage, score_bins, score_cells, BinGrid, SectionPanel};
use xenium_score::summary::{summarize, GroupBy};
use xenium_score::ucell::{Panel, UcellParams};

struct Fx {
    _tmp: tempfile::TempDir,
    store: SectionStore,
    xenium: std::path::PathBuf,
    state: std::path::PathBuf,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let out = generate(
        &tmp.path().join("synth"),
        &SynthOptions {
            cols: 24,
            rows: 16,
            ..Default::default()
        },
    )
    .unwrap();
    let root = tmp.path().join("index");
    let mut cfg = IngestConfig::new("S", &out.xenium_dir, &root);
    cfg.annotations.push(out.annotations_csv.clone());
    cfg.grids.push(GridInput {
        id: "niches".into(),
        label: "Niches".into(),
        path: out.niche_grid_csv.clone(),
        x_col: "x_um".into(),
        y_col: "y_um".into(),
        spacing_um: None,
        columns: None,
    });
    ingest(&cfg).unwrap();
    let store = SectionStore::open(&std::fs::canonicalize(root.join("S")).unwrap()).unwrap();
    Fx {
        state: tmp.path().join("state"),
        xenium: out.xenium_dir,
        store,
        _tmp: tmp,
    }
}

fn sigs() -> Vec<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    vec![
        s(&["CD68", "CXCL8", "IL1B"]),
        s(&["CD3E", "CD8A", "IL7R", "CD68-"]),
        s(&["COL1A1", "COL3A1", "PDGFRA", "NOT_IN_PANEL"]),
        s(&["MKI67"]),
    ]
}

#[test]
fn cell_scores_match_per_cell_reference_and_ignore_controls() {
    let fx = fixture();
    let panel = SectionPanel::from_manifest(&fx.store.manifest).unwrap();
    assert_eq!(
        panel.n_genes(),
        30,
        "synthetic panel: 30 genes, controls excluded"
    );
    let p = UcellParams::default();
    let sc = panel.scorer(&sigs(), &p).unwrap();
    let cols = score_cells(&fx.store, &sc, &Progress::new()).unwrap();
    // Reference: one cell at a time through the public per-cell accessor.
    let (mut scratch, mut row) = (vec![], vec![0.0; 4]);
    let mut controls_seen = false;
    for key in 1..=fx.store.n_cells() as u32 {
        let e: Vec<(u32, f64)> = fx
            .store
            .cell_expression(key)
            .unwrap()
            .into_iter()
            .map(|(g, c)| (g, c as f64))
            .collect();
        controls_seen |= e.iter().any(|x| !panel.is_gene(x.0));
        sc.score_unit(&e, &mut scratch, &mut row);
        for (col, v) in cols.iter().zip(&row) {
            assert_eq!(col[key as usize - 1].to_bits(), v.to_bits());
        }
    }
    assert!(
        controls_seen,
        "fixture should include control-probe counts in cells"
    );
    assert!(cols[0].iter().all(|v| (0.0..=1.0).contains(v)));
}

#[test]
fn coverage_reports_found_missing_controls_and_case() {
    let fx = fixture();
    let panel = SectionPanel::from_manifest(&fx.store.manifest).unwrap();
    let genes: Vec<String> = ["CD68", "cd3e", "NegControlProbe_00001", "NOPE", "IL1B-"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let c = coverage(&fx.store, &panel, &genes, &UcellParams::default());
    assert_eq!(
        (
            c.positive_found,
            c.positive_total,
            c.negative_found,
            c.negative_total
        ),
        (1, 4, 1, 1)
    );
    assert_eq!(c.imputed, 3);
    assert_eq!(c.max_rank_effective, 30);
    let st: HashMap<&str, &str> = c
        .genes
        .iter()
        .map(|g| (g.gene.as_str(), g.status.as_str()))
        .collect();
    assert_eq!(st["NegControlProbe_00001"], "not_a_gene");
    assert_eq!(st["NOPE"], "missing");
    assert_eq!(c.genes[1].suggestion.as_deref(), Some("CD3E"));
    assert!(c.genes[0].fraction_detected.unwrap() > 0.0);
    assert!(c.error.is_none());
}

#[test]
fn bin_scores_match_a_recount_of_the_raw_transcripts() {
    let fx = fixture();
    let panel = SectionPanel::from_manifest(&fx.store.manifest).unwrap();
    let p = UcellParams {
        max_rank: 20,
        ..Default::default()
    };
    let sc = panel.scorer(&sigs(), &p).unwrap();
    let bin = 40.0;
    let min_tx = 5;
    let b = score_bins(&fx.store, &panel, &sc, bin, min_tx, &Progress::new()).unwrap();
    assert_eq!(
        b.grid,
        BinGrid::for_bounds(fx.store.manifest.bounds_um, bin).unwrap()
    );
    // Recount from transcripts.parquet: QV >= 20, panel genes, position only.
    let grid = b.grid;
    let mut bins: HashMap<usize, HashMap<u32, f64>> = HashMap::new();
    stream_transcripts(&fx.xenium.join("transcripts.parquet"), |_, t| {
        let f = t.feature.as_string::<i32>();
        let (x, y) = (
            t.x.as_primitive::<Float64Type>(),
            t.y.as_primitive::<Float64Type>(),
        );
        let qv = t.qv.as_ref().unwrap().as_primitive::<Float64Type>();
        for i in 0..t.len {
            let Some(g) = panel.lookup(f.value(i)) else {
                continue;
            };
            if qv.is_null(i) || qv.value(i) < 20.0 {
                continue;
            }
            // Positions are stored as f32 in the index.
            let (px, py) = (x.value(i) as f32 as f64, y.value(i) as f32 as f64);
            let col = ((px - grid.origin_um[0]) / bin).floor() as usize;
            let row = ((py - grid.origin_um[1]) / bin).floor() as usize;
            *bins
                .entry(row * grid.width as usize + col)
                .or_default()
                .entry(g)
                .or_default() += 1.0;
        }
        Ok(())
    })
    .unwrap();
    let (mut scratch, mut row) = (vec![], vec![0.0; 4]);
    let mut n_scored = 0;
    for i in 0..grid.len() {
        let mut e: Vec<(u32, f64)> = bins
            .get(&i)
            .map(|m| m.iter().map(|(g, c)| (*g, *c)).collect())
            .unwrap_or_default();
        e.sort_by_key(|x| x.0);
        let total: f64 = e.iter().map(|x| x.1).sum();
        assert_eq!(b.n_transcripts[i] as f64, total, "bin {i} transcript count");
        if total >= min_tx as f64 {
            n_scored += 1;
            sc.score_unit(&e, &mut scratch, &mut row);
            for (k, (col, v)) in b.scores.iter().zip(&row).enumerate() {
                assert_eq!(col[i].to_bits(), v.to_bits(), "bin {i} sig {k}");
            }
        } else {
            assert!(b.scores.iter().all(|c| c[i].is_nan()));
        }
    }
    assert!(n_scored > 10);
}

#[test]
fn jobs_cache_results_and_can_be_cancelled() {
    let fx = fixture();
    let cache = ResultCache::new(&fx.state);
    let spec = JobSpec {
        signatures: sigs()
            .into_iter()
            .enumerate()
            .map(|(i, g)| SignatureSnapshot {
                id: None,
                name: format!("s{i}"),
                genes: g,
            })
            .collect(),
        params: UcellParams::default(),
        level: Level::Cell,
    };
    let cancelled = Progress::new();
    cancelled.cancel();
    let err = run(&fx.store, "v1", &cache, &spec, &cancelled).unwrap_err();
    assert!(err.downcast_ref::<Cancelled>().is_some());
    assert!(
        cache.list("S", "v1").unwrap().is_empty(),
        "nothing stored for a cancelled job"
    );

    let metas = run(&fx.store, "v1", &cache, &spec, &Progress::new()).unwrap();
    assert_eq!(metas.len(), 4);
    assert_eq!(cache.list("S", "v1").unwrap().len(), 4);
    assert_eq!(metas[2].coverage.imputed, 1);
    // A second run is served from the cache (nothing rescored).
    let p = Progress::new();
    let again = run(&fx.store, "v1", &cache, &spec, &p).unwrap();
    assert_eq!(
        again.iter().map(|m| m.computed_unix).collect::<Vec<_>>(),
        metas.iter().map(|m| m.computed_unix).collect::<Vec<_>>()
    );
    assert_eq!(
        p.snapshot().phase,
        "",
        "no scoring phase for cached results"
    );
    // A values file cut short (e.g. a crash before the data reached the disk) is not
    // served and is recomputed on the next run.
    let bin_path = fx
        .state
        .join("ucell/S/v1")
        .join(format!("{}.bin", metas[3].key));
    let full = std::fs::read(&bin_path).unwrap();
    std::fs::write(&bin_path, &full[..full.len() / 2]).unwrap();
    assert_eq!(cache.list("S", "v1").unwrap().len(), 3);
    run(&fx.store, "v1", &cache, &spec, &Progress::new()).unwrap();
    assert_eq!(std::fs::read(&bin_path).unwrap(), full);
    assert_eq!(cache.list("S", "v1").unwrap().len(), 4);
    // Deleting a result removes it from the list; the next run recomputes it.
    assert!(cache.delete("S", "v1", &metas[1].key).unwrap());
    assert!(!cache.delete("S", "v1", &metas[1].key).unwrap());
    assert_eq!(cache.list("S", "v1").unwrap().len(), 3);
    run(&fx.store, "v1", &cache, &spec, &Progress::new()).unwrap();
    assert_eq!(cache.list("S", "v1").unwrap().len(), 4);
    // Same genes under another name: reused, relabelled.
    let mut renamed = spec.clone();
    renamed.signatures[0].name = "renamed".into();
    let r = run(&fx.store, "v1", &cache, &renamed, &Progress::new()).unwrap();
    assert_eq!(
        (r[0].key.as_str(), r[0].signature.name.as_str()),
        (metas[0].key.as_str(), "renamed")
    );
    assert_eq!(r[0].computed_unix, metas[0].computed_unix);
    // Storing a result for a newer index version removes the old version's results.
    let bins = JobSpec {
        level: Level::Bin {
            bin_um: 50.0,
            min_transcripts: 3,
        },
        ..spec.clone()
    };
    let bm = run(&fx.store, "v2", &cache, &bins, &Progress::new()).unwrap();
    assert!(cache.list("S", "v1").unwrap().is_empty());
    assert!(bm[0].grid.is_some());

    // Summaries by a categorical cell field and by a grid field (centroid join).
    let v = cache
        .values(&run(&fx.store, "v2", &cache, &spec, &Progress::new()).unwrap()[0])
        .unwrap();
    let by_type = summarize(
        &fx.store,
        &v.scores,
        &GroupBy::parse("field:parent_cell_type").unwrap(),
    )
    .unwrap();
    let n: usize = by_type.groups.iter().map(|g| g.n).sum();
    assert_eq!(
        n + by_type.n_unassigned + by_type.n_no_score,
        fx.store.n_cells()
    );
    let grid = &fx.store.manifest.grids[0];
    let cat = grid.fields.iter().find(|f| f.categories.is_some()).unwrap();
    let by_niche = summarize(
        &fx.store,
        &v.scores,
        &GroupBy::parse(&format!("grid:niches/{}", cat.id)).unwrap(),
    )
    .unwrap();
    assert!(!by_niche.groups.is_empty());
    assert!(by_niche.join.contains("centroid"));

    // Parquet export.
    let all = cache.list("S", "v2").unwrap();
    let cells: Vec<_> = all
        .iter()
        .filter(|m| m.level == Level::Cell)
        .map(|m| (m.clone(), cache.values(m).unwrap()))
        .collect();
    let pq = cells_parquet(&fx.store, &cells).unwrap();
    check_parquet(
        &pq,
        fx.store.n_cells(),
        &[
            "section_id",
            "cell_id",
            "cell_key",
            "x_um",
            "y_um",
            "s0_UCell",
        ],
    );
    let binned: Vec<_> = all
        .iter()
        .filter(|m| m.level != Level::Cell)
        .map(|m| (m.clone(), cache.values(m).unwrap()))
        .collect();
    let pq = bins_parquet("S", &binned).unwrap();
    let ntx = binned[0]
        .1
        .n_transcripts
        .as_ref()
        .unwrap()
        .iter()
        .filter(|&&n| n > 0)
        .count();
    check_parquet(
        &pq,
        ntx,
        &["bin_col", "bin_row", "n_transcripts", "s1_UCell"],
    );
}

fn check_parquet(bytes: &[u8], rows: usize, cols: &[&str]) {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let r = SerializedFileReader::new(bytes::Bytes::copy_from_slice(bytes)).unwrap();
    let md = r.metadata().file_metadata();
    assert_eq!(md.num_rows() as usize, rows);
    let names: Vec<String> = md
        .schema_descr()
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    for c in cols {
        assert!(
            names.iter().any(|n| n == c),
            "missing column {c} in {names:?}"
        );
    }
    assert!(md
        .key_value_metadata()
        .unwrap()
        .iter()
        .any(|kv| kv.key == METADATA_KEY));
}

/// Live parity on the section's own expression matrix (genes x cells from expr_csr).
#[test]
fn section_cell_scores_match_r() {
    let Ok(rscript) = std::env::var("XENIUM_UCELL_RSCRIPT") else {
        eprintln!("XENIUM_UCELL_RSCRIPT not set: skipped");
        return;
    };
    let fx = fixture();
    let panel = SectionPanel::from_manifest(&fx.store.manifest).unwrap();
    let p = UcellParams {
        max_rank: 15,
        ..Default::default()
    };
    let sc = panel.scorer(&sigs(), &p).unwrap();
    let cols = score_cells(&fx.store, &sc, &Progress::new()).unwrap();
    let dir = fx._tmp.path().join("rcase");
    std::fs::create_dir_all(&dir).unwrap();
    write_section_case(&fx.store, &panel, &dir, &p);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ucell_reference.R");
    let o = std::process::Command::new(rscript)
        .arg(script)
        .arg(&dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = std::fs::read_to_string(dir.join("r_scores.tsv")).unwrap();
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap().split('\t').collect();
    let mut n = 0;
    for (u, l) in lines.enumerate() {
        let v: Vec<&str> = l.split('\t').collect();
        for (k, col) in cols.iter().enumerate() {
            let j = header.iter().position(|h| *h == format!("s{k}")).unwrap();
            let r: f64 = v[j].parse().unwrap();
            assert_eq!(
                r.to_bits(),
                col[u].to_bits(),
                "cell {u} sig {k}: R {r} vs {}",
                col[u]
            );
            n += 1;
        }
    }
    eprintln!("{n} section scores identical to R");
}

fn write_section_case(store: &SectionStore, panel: &SectionPanel, dir: &Path, p: &UcellParams) {
    use std::fmt::Write;
    let m = &store.manifest;
    let genes = panel.gene_ids();
    let row_of: HashMap<u32, usize> = genes.iter().enumerate().map(|(i, g)| (*g, i + 1)).collect();
    let names: Vec<&str> = genes
        .iter()
        .map(|g| {
            m.transcripts.as_ref().unwrap().genes[*g as usize]
                .name
                .as_str()
        })
        .collect();
    let mut body = String::new();
    let mut nnz = 0;
    for key in 1..=store.n_cells() as u32 {
        for (g, c) in store.cell_expression(key).unwrap() {
            if let Some(r) = row_of.get(&g) {
                writeln!(body, "{r} {key} {c}").unwrap();
                nnz += 1;
            }
        }
    }
    let head = format!(
        "%%MatrixMarket matrix coordinate integer general\n{} {} {nnz}\n",
        genes.len(),
        store.n_cells()
    );
    std::fs::write(dir.join("matrix.mtx"), head + &body).unwrap();
    std::fs::write(dir.join("genes.txt"), names.join("\n") + "\n").unwrap();
    let units: Vec<String> = (1..=store.n_cells()).map(|k| format!("c{k}")).collect();
    std::fs::write(dir.join("units.txt"), units.join("\n") + "\n").unwrap();
    let sigs: serde_json::Map<String, serde_json::Value> = sigs()
        .into_iter()
        .enumerate()
        .map(|(i, g)| (format!("s{i}"), serde_json::json!(g)))
        .collect();
    std::fs::write(
        dir.join("signatures.json"),
        serde_json::Value::Object(sigs).to_string(),
    )
    .unwrap();
    std::fs::write(dir.join("params.json"), serde_json::to_string(p).unwrap()).unwrap();
}
