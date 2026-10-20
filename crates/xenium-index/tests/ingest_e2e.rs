//! End-to-end: synthetic Xenium -> ingest -> validate -> query, plus
//! idempotence and failure-safety of publishing.

use std::collections::HashMap;
use xenium_core::codec::{decode_density, decode_points, decode_polygons};
use xenium_core::manifest::Layout;
use xenium_index::{ingest, GridInput, ImageInput, IngestConfig, RegistrationInput, SectionStore};
use xenium_io::synth::{generate, HeVariant, SynthOptions};

fn config(
    dir: &std::path::Path,
    out: &xenium_io::synth::SynthOutput,
    root: &std::path::Path,
) -> IngestConfig {
    let mut cfg = IngestConfig::new("S1", &out.xenium_dir, root);
    cfg.images.push(ImageInput {
        id: "he".into(),
        label: "H&E".into(),
        path: out.he_tiff.clone(),
        registration: RegistrationInput::Json(out.registration_json.clone()),
        ifds: None,
    });
    cfg.annotations.push(out.annotations_csv.clone());
    cfg.lr_scores.push(out.lr_scores_csv.clone());
    cfg.grids.push(GridInput {
        id: "niches".into(),
        label: "Niches".into(),
        path: out.niche_grid_csv.clone(),
        x_col: "x_um".into(),
        y_col: "y_um".into(),
        spacing_um: None,
        columns: None,
    });
    let _ = dir;
    cfg
}

#[test]
fn synthetic_section_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let out = generate(&tmp.path().join("synth"), &SynthOptions::default()).unwrap();
    let root = tmp.path().join("index");
    let cfg = config(tmp.path(), &out, &root);
    let v1 = ingest(&cfg).unwrap();
    let store = SectionStore::open(&root.join("S1")).unwrap();
    let m = &store.manifest;
    let truth = &out.truth;

    // Cells and stable IDs
    assert_eq!(store.n_cells(), truth.n_cells);
    let k = store.key_by_id(&truth.first_cell_id).unwrap();
    let c = store.centroid(k).unwrap();
    assert!((c[0] as f64 - truth.first_cell_centroid[0]).abs() < 1e-3);
    let details = store.cell_details(k).unwrap();
    assert_eq!(details["cell_id"], truth.first_cell_id.as_str());

    // >64 fine cell types survive without truncation
    let fine = m.field("fine_cell_type").unwrap();
    assert!(
        fine.categories.as_ref().unwrap().len() > 64,
        "{}",
        fine.categories.as_ref().unwrap().len()
    );

    // Transcripts: every row indexed, unassigned and QV counts exact
    let t = m.transcripts.as_ref().unwrap();
    assert_eq!(t.indexed as usize, truth.n_transcripts);
    assert_eq!(t.unassigned as usize, truth.n_unassigned);
    let qv20: u64 = t.genes.iter().map(|g| g.count_qv20).sum();
    assert_eq!(qv20 as usize, truth.n_qv_ge_20);
    let controls: u64 = t
        .genes
        .iter()
        .filter(|g| !g.category.is_gene())
        .map(|g| g.count)
        .sum();
    assert_eq!(controls as usize, truth.n_controls);

    // Tile queries over the whole grid reproduce each gene's total, with and without QV filter
    let all: Vec<u32> = (0..t.genes.len().min(64) as u32).collect();
    let (mut n_all, mut n_q) = (HashMap::new(), HashMap::new());
    let b = m.bounds_um;
    let g = store.grid();
    let (x0, y0, x1, y1) = g.tiles_in_rect(b).unwrap();
    for ty in y0..=y1 {
        for tx in x0..=x1 {
            for (map, q) in [(&mut n_all, 0u8), (&mut n_q, 80u8)] {
                let p = decode_points(&store.points_tile(tx, ty, &all, q).unwrap()).unwrap();
                let mut off = 0;
                for s in &p.segments {
                    *map.entry(s.gene_id).or_insert(0u64) += s.count as u64;
                    for r in &p.records[off..off + s.count as usize] {
                        assert_eq!(g.tile_of(r.x as f64, r.y as f64), Some((tx, ty)));
                        assert!(q == 0 || r.qv_q >= q);
                    }
                    off += s.count as usize;
                }
            }
        }
    }
    for gi in &t.genes {
        assert_eq!(
            n_all.get(&gi.id).copied().unwrap_or(0),
            gi.count,
            "gene {}",
            gi.name
        );
        assert_eq!(
            n_q.get(&gi.id).copied().unwrap_or(0),
            gi.count_qv20,
            "gene {} qv20",
            gi.name
        );
    }

    // Density at the coarsest level sums to the gene total
    let lvl = t.density_max_level;
    let span = 256.0 * (1u64 << lvl) as f64;
    let mut dens = 0u64;
    for dy in (b[1] / span).floor() as i32..=(b[3] / span).floor() as i32 {
        for dx in (b[0] / span).floor() as i32..=(b[2] / span).floor() as i32 {
            dens += decode_density(&store.density_tile(0, lvl, dx, dy, 0).unwrap())
                .unwrap()
                .total as u64;
        }
    }
    assert_eq!(dens, t.genes[0].count);

    // Polygons: a cell crossing a chunk edge is returned once, from its centroid chunk
    let cb = m.cells.as_ref().unwrap().boundaries.as_ref().unwrap();
    let mut seen = HashMap::new();
    for ch in &cb.chunks {
        let raw = store
            .polygon_chunk(false, ch[0] as i32, ch[1] as i32)
            .unwrap()
            .unwrap();
        let (_, poly) = decode_polygons(&raw).unwrap();
        for &key in &poly.cell_keys {
            *seen.entry(key).or_insert(0) += 1;
        }
    }
    assert_eq!(seen.len(), truth.n_cells);
    assert!(seen.values().all(|&n| n == 1));
    assert!(m.images[0]
        .registration
        .matrix_xu_from_px
        .apply(0.0, 0.0)
        .0
        .is_finite());

    // Re-ingest: new version, same content; failure keeps the live version.
    let v2 = ingest(&cfg).unwrap();
    assert_ne!(v1, v2);
    let m2 = SectionStore::open(&root.join("S1")).unwrap().manifest;
    let strip = |mut m: xenium_core::manifest::SectionManifest| {
        m.created_at_unix = 0;
        m
    };
    assert_eq!(strip(m.clone()), strip(m2.clone()));
    for f in [
        "transcripts/points.bin",
        "cells/cell_chunks.pack",
        "cells/fields.pack",
        "cells/fields.idx",
        "images/he/tiles.pack",
        "images/he/tiles.idx",
    ] {
        assert_eq!(
            std::fs::read(v1.join(f)).unwrap(),
            std::fs::read(v2.join(f)).unwrap(),
            "{f} not deterministic"
        );
    }
    let mut bad = cfg.clone();
    let broken = tmp.path().join("broken.csv");
    let id = &truth.first_cell_id;
    std::fs::write(&broken, format!("cell_id,x\n{id},1\n{id},2\n")).unwrap();
    bad.annotations = vec![broken];
    assert!(ingest(&bad).is_err());
    let live = std::fs::canonicalize(root.join("S1")).unwrap();
    assert_eq!(live, std::fs::canonicalize(&v2).unwrap());
    assert!(xenium_index::validate::validate_dir(&root.join("S1"))
        .unwrap()
        .errors
        .is_empty());
}

#[test]
fn mirrored_legacy_section_and_alignment_csv() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = SynthOptions {
        he: HeVariant::Mirrored,
        legacy_columns: true,
        cols: 12,
        rows: 9,
        ..Default::default()
    };
    let out = generate(&tmp.path().join("synth"), &opts).unwrap();
    let root = tmp.path().join("index");
    let mut cfg = IngestConfig::new("S2", &out.xenium_dir, &root);
    cfg.images.push(ImageInput {
        id: "he".into(),
        label: "H&E".into(),
        path: out.he_tiff.clone(),
        registration: RegistrationInput::AlignmentCsv {
            path: out.alignment_csv.clone(),
            units: xenium_core::registration::AlignmentOutputUnits::XeniumMorphologyPx,
        },
        ifds: None,
    });
    ingest(&cfg).unwrap();
    let store = SectionStore::open(&root.join("S2")).unwrap();
    let reg = &store.manifest.images[0].registration;
    assert!(reg.is_reflection);
    // The CSV adapter must reproduce the ground-truth px -> µm transform.
    let truth = xenium_core::Affine2::try_from(out.truth.matrix_xu_from_px.clone()).unwrap();
    for (x, y) in [(0.0, 0.0), (500.0, 250.0), (1000.0, 900.0)] {
        let (a, b) = reg.matrix_xu_from_px.apply(x, y);
        let (c, d) = truth.apply(x, y);
        assert!((a - c).abs() < 1e-6 && (b - d).abs() < 1e-6);
    }
    let t = store.manifest.transcripts.as_ref().unwrap();
    assert!(t
        .genes
        .iter()
        .any(|g| g.name.starts_with("NegControlProbe") && !g.category.is_gene()));
}

#[test]
fn corrupt_expression_index_is_rejected_not_allocated() {
    let tmp = tempfile::tempdir().unwrap();
    let out = generate(
        &tmp.path().join("synth"),
        &SynthOptions {
            cols: 6,
            rows: 5,
            ..Default::default()
        },
    )
    .unwrap();
    let root = tmp.path().join("index");
    ingest(&IngestConfig::new("C", &out.xenium_dir, &root)).unwrap();
    let live = std::fs::canonicalize(root.join("C")).unwrap();
    let idx = live.join("transcripts/expr_csr.idx");
    let mut b = std::fs::read(&idx).unwrap();
    // offsets[2] = 2^40 (the reviewer's reproduction of an 8 TB allocation)
    b[8 + 16..8 + 24].copy_from_slice(&(1u64 << 40).to_le_bytes());
    std::fs::write(&idx, b).unwrap();
    let r = xenium_index::validate::validate_dir(&root.join("C")).unwrap();
    assert!(!r.errors.is_empty(), "corruption must fail validation");
    assert!(SectionStore::open(&root.join("C")).is_err());
}

#[test]
fn annotation_inference_and_id_like_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let out = generate(
        &tmp.path().join("synth"),
        &SynthOptions {
            cols: 6,
            rows: 5,
            ..Default::default()
        },
    )
    .unwrap();
    let ids: Vec<String> = {
        let t = xenium_io::cells::read_cells(&out.xenium_dir.join("cells.parquet")).unwrap();
        t.ids
    };
    let mut csv = String::from("cell_id,score,barcode,label\n");
    for (i, id) in ids.iter().enumerate() {
        let score = if i == 0 {
            "inf".to_string()
        } else {
            format!("{i}.5")
        };
        // > MAX_CATEGORIES distinct values is impossible with 30 cells, so check the numeric path
        csv.push_str(&format!(
            "{id},{score},BC{i},{}\n",
            if i % 2 == 0 { "x" } else { "y" }
        ));
    }
    let path = tmp.path().join("ann.csv");
    std::fs::write(&path, csv).unwrap();
    let mut cfg = IngestConfig::new("D", &out.xenium_dir, &tmp.path().join("index"));
    cfg.annotations.push(path);
    ingest(&cfg).unwrap();
    let m = SectionStore::open(&tmp.path().join("index/D"))
        .unwrap()
        .manifest;
    assert_eq!(
        m.field("score").unwrap().kind,
        xenium_core::manifest::FieldKind::Numeric
    );
    assert_eq!(
        m.field("label").unwrap().categories.as_ref().unwrap().len(),
        2
    );
}

#[test]
fn registration_qc_detects_a_shift() {
    let tmp = tempfile::tempdir().unwrap();
    let out = generate(
        &tmp.path().join("synth"),
        &SynthOptions {
            cols: 16,
            rows: 12,
            ..Default::default()
        },
    )
    .unwrap();
    let root = tmp.path().join("index");
    let truth = xenium_core::Affine2::try_from(out.truth.matrix_xu_from_px.clone()).unwrap();
    for (id, shift) in [("OK", 0.0), ("SHIFTED", 8.0)] {
        // A wrong transform places the H&E 8 µm too far right.
        let m = xenium_core::Affine2::translation(shift, 0.0).compose(&truth);
        let mut cfg = IngestConfig::new(id, &out.xenium_dir, &root);
        cfg.images.push(ImageInput {
            id: "he".into(),
            label: "H&E".into(),
            path: out.he_tiff.clone(),
            registration: RegistrationInput::Spec(
                xenium_core::registration::RegistrationSpec::Affine {
                    direction: xenium_core::registration::AffineDirection::XuFromPx,
                    matrix: m.to_rows3(),
                },
            ),
            ifds: None,
        });
        ingest(&cfg).unwrap();
        let store = SectionStore::open(&root.join(id)).unwrap();
        let qc = xenium_index::qc::registration_qc(&store, None, 5000, 16.0, 1.0, 2).unwrap();
        let g = &qc.global;
        assert!(
            (g.best_dx_um - shift).abs() <= 1.5 && g.best_dy_um.abs() <= 1.5,
            "{id}: {g:?}"
        );
        assert_eq!(qc.verdict.starts_with("OK"), shift == 0.0, "{}", qc.verdict);
    }
}

#[test]
fn morphology_channels_are_indexed_on_the_xenium_grid() {
    let tmp = tempfile::tempdir().unwrap();
    let out = generate(
        &tmp.path().join("synth"),
        &SynthOptions {
            cols: 12,
            rows: 8,
            morphology: true,
            ..Default::default()
        },
    )
    .unwrap();
    let root = tmp.path().join("index");
    let mut cfg = config(tmp.path(), &out, &root);
    cfg.morphology = Some(out.xenium_dir.join("morphology_focus"));
    ingest(&cfg).unwrap();
    let store = SectionStore::open(&root.join("S1")).unwrap();
    let m = &store.manifest;
    let names: Vec<&str> = m
        .images
        .iter()
        .filter_map(|i| i.channel.as_ref().map(|c| c.name.as_str()))
        .collect();
    assert_eq!(
        names,
        ["DAPI", "ATP1A1/CD45/E-Cadherin", "18S", "alphaSMA/Vimentin"]
    );
    let dapi = m.image("morph-dapi").unwrap();
    let ch = dapi.channel.as_ref().unwrap();
    assert!(ch.window[0] < ch.window[1]);
    assert_eq!(ch.color, [70, 130, 255]);
    assert!((dapi.registration.approx_pixel_size_um - 1.0).abs() < 1e-9);
    assert!(
        !m.warnings.iter().any(|w| w.contains("morphology")),
        "{:?}",
        m.warnings
    );

    // Intensity at a pixel of the level-0 DAPI pyramid (1 µm/px from the origin).
    let tile = dapi.tile_size;
    let at = |x_um: f64, y_um: f64| -> u8 {
        let (px, py) = (x_um.floor() as u32, y_um.floor() as u32);
        let jpg = store
            .image_tile("morph-dapi", 0, px / tile, py / tile)
            .unwrap();
        let mut d = zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(jpg));
        let pixels = d.decode().unwrap();
        let (w, h) = d.dimensions().unwrap();
        let ch = pixels.len() / (w * h); // the decoder may expand gray to RGB
        pixels[(((py % tile) as usize) * w + (px % tile) as usize) * ch]
    };
    // Nuclei (at the cell centroids) are bright; empty background near the origin is dark.
    let mut bright = 0;
    for k in 1..=store.n_cells() as u32 {
        let c = store.centroid(k).unwrap();
        if at(c[0] as f64, c[1] as f64) > 180 {
            bright += 1;
        }
    }
    assert!(
        bright as f64 > 0.95 * store.n_cells() as f64,
        "{bright} of {} nuclei bright",
        store.n_cells()
    );
    assert!(at(5.0, 5.0) < 40);
    // Registration QC still uses the H&E, not a fluorescence channel, by default.
    let qc = xenium_index::qc::registration_qc(&store, None, 2000, 10.0, 1.0, 1).unwrap();
    assert_eq!(
        (qc.image_id.as_str(), qc.signal.as_str()),
        ("he", "darkness")
    );
    // On DAPI it measures brightness, which peaks at the nucleus centres.
    let qc =
        xenium_index::qc::registration_qc(&store, Some("morph-dapi"), 2000, 10.0, 1.0, 2).unwrap();
    assert_eq!(qc.signal, "brightness");
    assert!(qc.verdict.starts_with("OK"), "{}", qc.verdict);
    assert!(qc.global.contrast_at_zero > 1.0, "{:?}", qc.global);

    // Each image is one tile pack (two files), not a file per tile.
    let live = std::fs::canonicalize(root.join("S1")).unwrap();
    for im in &m.images {
        assert_eq!(im.tile_layout, Layout::Packed);
        let mut files: Vec<String> = std::fs::read_dir(live.join("images").join(&im.id))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files, ["tiles.idx", "tiles.pack"], "{}", im.id);
    }

    // Validation reads the pack index: a missing tile and a non-JPEG tile (tile 0,0 of
    // each level is always among the spot-checked ones) are both reported.
    let errors = |root: &std::path::Path| {
        xenium_index::validate::validate_dir(&root.join("S1"))
            .unwrap()
            .errors
    };
    assert!(errors(&root).is_empty(), "{:?}", errors(&root));
    // Drop tile (0, 0) of level 0 (key 0, the first index entry) from one pack...
    let idx_path = live.join("images/morph-18s/tiles.idx");
    let mut idx = std::fs::read(&idx_path).unwrap();
    let n = u32::from_le_bytes(idx[4..8].try_into().unwrap());
    assert_eq!(u32::from_le_bytes(idx[8..12].try_into().unwrap()), 0);
    idx.drain(8..24);
    idx[4..8].copy_from_slice(&(n - 1).to_le_bytes());
    std::fs::write(&idx_path, idx).unwrap();
    // ...and break the JPEG signature of the first tile written to another.
    let pack_path = live.join("images/morph-dapi/tiles.pack");
    let mut pack = std::fs::read(&pack_path).unwrap();
    pack[..2].copy_from_slice(b"XX");
    std::fs::write(&pack_path, pack).unwrap();
    let e = errors(&root).join(" ");
    assert!(
        e.contains("morph-18s: 1 missing / 0 invalid / 0 unexpected"),
        "{e}"
    );
    assert!(
        e.contains("morph-dapi: 0 missing / 1 invalid / 0 unexpected"),
        "{e}"
    );
    std::fs::remove_file(live.join("images/morph-atp1a1-cd45-e-cadherin/tiles.idx")).unwrap();
    assert!(errors(&root)
        .join(" ")
        .contains("image morph-atp1a1-cd45-e-cadherin: tile pack unreadable"));
}

#[test]
fn packed_index_has_few_files_and_older_per_file_indexes_still_read() {
    let tmp = tempfile::tempdir().unwrap();
    let out = generate(
        &tmp.path().join("synth"),
        &SynthOptions {
            cols: 12,
            rows: 8,
            ..Default::default()
        },
    )
    .unwrap();
    let (packed, files) = (tmp.path().join("packed"), tmp.path().join("files"));
    ingest(&config(tmp.path(), &out, &packed)).unwrap();
    let mut cfg = config(tmp.path(), &out, &files);
    cfg.layout = Layout::Files;
    ingest(&cfg).unwrap();
    // Packed: a few dozen files per section, whatever the number of tiles and fields.
    let count_files = |dir: &std::path::Path| {
        fn walk(d: &std::path::Path) -> usize {
            std::fs::read_dir(d)
                .unwrap()
                .map(|e| {
                    let p = e.unwrap().path();
                    if p.is_dir() {
                        walk(&p)
                    } else {
                        1
                    }
                })
                .sum()
        }
        walk(&std::fs::canonicalize(dir).unwrap())
    };
    assert!(
        count_files(&packed.join("S1")) <= 30,
        "{}",
        count_files(&packed.join("S1"))
    );
    assert!(count_files(&files.join("S1")) > count_files(&packed.join("S1")));
    // An index written before packed tiles: schema 1, no layouts, one file per blob.
    let live = std::fs::canonicalize(files.join("S1")).unwrap();
    let mut m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(live.join("manifest.json")).unwrap()).unwrap();
    assert!(m["images"][0].get("tile_layout").is_none());
    assert!(m["cells"].get("field_layout").is_none());
    m["schema_version"] = 1.into();
    std::fs::write(live.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();

    let (p, f) = (
        SectionStore::open(&packed.join("S1")).unwrap(),
        SectionStore::open(&files.join("S1")).unwrap(),
    );
    assert_eq!(f.manifest.schema_version, 1);
    let he = p.manifest.image("he").unwrap();
    assert_eq!(he.tile_layout, Layout::Packed);
    assert_eq!(f.manifest.image("he").unwrap().tile_layout, Layout::Files);
    // Same tiles from both layouts, on every level.
    let mut n = 0;
    for l in 0..he.levels {
        let ts = he.tile_size * (1 << l);
        for y in 0..he.height.div_ceil(ts) {
            for x in 0..he.width.div_ceil(ts) {
                let a = p.image_tile("he", l, x, y).unwrap();
                assert!(a.starts_with(&[0xFF, 0xD8]));
                assert_eq!(a, f.image_tile("he", l, x, y).unwrap(), "{l}/{x}_{y}");
                n += 1;
            }
        }
    }
    assert!(n > 1);
    // Same fields and cell details from both layouts.
    let cells = p.manifest.cells.as_ref().unwrap();
    assert_eq!(cells.field_layout, Layout::Packed);
    assert!(cells.fields.len() > 3);
    for fi in &cells.fields {
        assert_eq!(
            p.field_bytes(&fi.id).unwrap(),
            f.field_bytes(&fi.id).unwrap(),
            "{}",
            fi.id
        );
    }
    for k in [1, 2, p.n_cells() as u32] {
        assert_eq!(p.cell_details(k).unwrap(), f.cell_details(k).unwrap());
    }
    assert!(p.field_bytes("nope").is_err() && f.field_bytes("nope").is_err());
    for s in [&p, &f] {
        assert!(s.image_tile("he", he.levels, 0, 0).is_err());
        assert!(s
            .image_tile("he", 0, he.width.div_ceil(he.tile_size), 0)
            .is_err());
        assert!(s.image_tile("nope", 0, 0, 0).is_err());
    }

    // Validation lists the tile directories of the per-file layout.
    let errors = || {
        xenium_index::validate::validate_dir(&files.join("S1"))
            .unwrap()
            .errors
    };
    assert!(errors().is_empty(), "{:?}", errors());
    std::fs::write(live.join("images/he/0/0_0.jpg"), b"not a jpeg").unwrap();
    assert!(errors().join(" ").contains("he: 0 missing / 1 invalid"));
    std::fs::remove_file(live.join("images/he/0/0_0.jpg")).unwrap();
    assert!(errors().join(" ").contains("he: 1 missing / 0 invalid"));
    // A schema-1 manifest cannot claim packed tiles.
    m["images"][0]["tile_layout"] = "packed".into();
    std::fs::write(live.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    let e = SectionStore::open(&files.join("S1")).err().unwrap();
    assert!(
        format!("{e:#}").contains("packed tiles or fields need schema_version 2"),
        "{e:#}"
    );

    // A field pack that does not match the manifest is reported.
    let errors = || {
        xenium_index::validate::validate_dir(&packed.join("S1"))
            .unwrap()
            .errors
    };
    assert!(errors().is_empty(), "{:?}", errors());
    let plive = std::fs::canonicalize(packed.join("S1")).unwrap();
    let idx_path = plive.join("cells/fields.idx");
    let mut idx = std::fs::read(&idx_path).unwrap();
    let n = u32::from_le_bytes(idx[4..8].try_into().unwrap());
    idx.truncate(idx.len() - 16);
    idx[4..8].copy_from_slice(&(n - 1).to_le_bytes());
    std::fs::write(&idx_path, idx).unwrap();
    let e = errors().join(" ");
    assert!(
        e.contains(&format!("field pack has {} entries for {n} fields", n - 1)),
        "{e}"
    );
}
