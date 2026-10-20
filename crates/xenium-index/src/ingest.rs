//! `ingest`: build a complete, validated index for one section and publish it.

use crate::pack::PackWriter;
use crate::publish;
use crate::pyramid::build_pyramid;
use crate::stats::{categorical, field_info, sanitize_id};
use crate::tables_ingest::{
    import_annotations, import_grid, import_lr_scores, ColumnSpec, GridSpec, ImportedField,
};
use crate::transcripts_ingest::{build_transcripts, DENSITY_TILE_BINS};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;
use xenium_core::codec::{
    encode_centroids, encode_field, encode_grid_raster, encode_polygons, FieldColumn, PolygonChunk,
    ShapeKind,
};
use xenium_core::grid::{tile_from_key, tile_key, TileGrid};
use xenium_core::manifest::{
    default_channel_color, CellsInfo, ChannelInfo, FieldInfo, ImageInfo, ImageKind, Layout,
    SectionManifest, ShapeLayerInfo, SourceFingerprint, SCHEMA_VERSION,
};
use xenium_core::registration::{parse_alignment_csv, AlignmentOutputUnits, RegistrationSpec};
use xenium_core::CellDictionary;
use xenium_io::boundaries::for_each_polygon;
use xenium_io::cells::read_cells;
use xenium_io::experiment::read_experiment;
use xenium_io::fingerprint::fingerprint;
use xenium_io::tables::read_table;
use xenium_io::tiff_image::{probe, BandReader};
use xenium_io::XeniumFiles;

#[derive(Debug, Clone)]
pub enum RegistrationInput {
    /// A `registration.json` file (see `RegistrationSpec`).
    Json(PathBuf),
    /// A 10x Xenium Explorer `*_imagealignment.csv`.
    AlignmentCsv {
        path: PathBuf,
        units: AlignmentOutputUnits,
    },
    Spec(RegistrationSpec),
}

#[derive(Debug, Clone)]
pub struct ImageInput {
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    pub registration: RegistrationInput,
    pub ifds: Option<Vec<usize>>,
}

#[derive(Debug, Clone)]
pub struct GridInput {
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    pub x_col: String,
    pub y_col: String,
    pub spacing_um: Option<f64>,
    pub columns: Option<Vec<ColumnSpec>>,
}

#[derive(Debug, Clone)]
pub struct IngestConfig {
    pub section_id: String,
    pub display_name: Option<String>,
    pub xenium_dir: PathBuf,
    pub index_root: PathBuf,
    pub tile_size_um: f64,
    pub image_tile_px: u32,
    pub jpeg_quality: u8,
    pub images: Vec<ImageInput>,
    pub annotations: Vec<PathBuf>,
    pub annotation_id_column: String,
    /// Columns to import from the annotation tables (each from the first table that has it).
    pub annotation_columns: Option<Vec<ColumnSpec>>,
    pub lr_scores: Vec<PathBuf>,
    /// Column bundles (`*.json`, see `bundle_ingest`).
    pub column_bundles: Vec<PathBuf>,
    pub grids: Vec<GridInput>,
    pub skip_transcripts: bool,
    pub keep_versions: usize,
    /// Xenium `morphology_focus` directory: every `*.ome.tif` channel becomes an image
    /// (`morph-<channel>`) on the Xenium pixel grid.
    pub morphology: Option<PathBuf>,
    /// Storage of image tiles and cell fields (default: packs; `Files` is the
    /// older one-file-per-tile layout).
    pub layout: Layout,
}

impl IngestConfig {
    pub fn new(section_id: &str, xenium_dir: &Path, index_root: &Path) -> Self {
        IngestConfig {
            section_id: section_id.to_string(),
            display_name: None,
            xenium_dir: xenium_dir.to_path_buf(),
            index_root: index_root.to_path_buf(),
            tile_size_um: xenium_core::grid::DEFAULT_TILE_SIZE_UM,
            image_tile_px: 512,
            jpeg_quality: 85,
            images: vec![],
            annotations: vec![],
            annotation_id_column: "cell_id".into(),
            annotation_columns: None,
            lr_scores: vec![],
            column_bundles: vec![],
            grids: vec![],
            skip_transcripts: false,
            keep_versions: 2,
            morphology: None,
            layout: Layout::Packed,
        }
    }
}

/// `(cell_key, label_id, input sequence, ring)` collected per chunk before sorting.
type ChunkPolygon = (u32, Option<i64>, u64, Vec<[f32; 2]>);

fn base_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reserve `base`, or `base_2`, `base_3`, ... if two source columns sanitise to the same id.
fn unique_id(base: &str, taken: &mut BTreeSet<String>) -> String {
    let mut id = base.to_string();
    let mut n = 2;
    while !taken.insert(id.clone()) {
        id = format!("{base}_{n}");
        n += 1;
    }
    id
}

/// Writes cell fields in manifest order: into one pack keyed by the field's position
/// in `cells.fields`, or (older layout) one file per field.
enum FieldSink {
    Pack(PackWriter),
    Files(PathBuf),
}

impl FieldSink {
    fn create(cells_dir: &Path, layout: Layout) -> Result<Self> {
        let base = cells_dir.join("fields");
        Ok(match layout {
            Layout::Packed => FieldSink::Pack(PackWriter::create(&base)?),
            Layout::Files => {
                std::fs::create_dir_all(&base)?;
                FieldSink::Files(base)
            }
        })
    }

    /// Store `f` and append its info to `fields`; its position there is its key.
    fn push(&mut self, fields: &mut Vec<FieldInfo>, f: ImportedField) -> Result<()> {
        let bytes = encode_field(&f.column);
        match self {
            FieldSink::Pack(p) => p.add(fields.len() as u32, &bytes)?,
            FieldSink::Files(dir) => std::fs::write(dir.join(format!("{}.bin", f.info.id)), bytes)?,
        }
        fields.push(f.info);
        Ok(())
    }

    fn finish(self) -> Result<()> {
        if let FieldSink::Pack(p) = self {
            p.finish()?;
        }
        Ok(())
    }
}

struct Report {
    steps: Vec<serde_json::Value>,
    t: Instant,
}

impl Report {
    fn step(&mut self, name: &str, detail: serde_json::Value) {
        tracing::info!("{name}: done ({:.1?})", self.t.elapsed());
        self.steps.push(
            json!({"step": name, "elapsed_s": self.t.elapsed().as_secs_f64(), "detail": detail}),
        );
    }
}

/// Build, validate and publish. Returns the published version directory.
pub fn ingest(cfg: &IngestConfig) -> Result<PathBuf> {
    if !xenium_core::manifest::is_safe_id(&cfg.section_id) || cfg.section_id.contains(':') {
        bail!("section id {:?} must match [A-Za-z0-9_.-]+", cfg.section_id);
    }
    let mut seen_ids = BTreeSet::new();
    for img in &cfg.images {
        if !xenium_core::manifest::is_safe_id(&img.id) || !seen_ids.insert(img.id.clone()) {
            bail!("image id {:?} is invalid or duplicated", img.id);
        }
    }
    for g in &cfg.grids {
        if !xenium_core::manifest::is_safe_id(&g.id) || !seen_ids.insert(g.id.clone()) {
            bail!("grid id {:?} is invalid or duplicated", g.id);
        }
    }
    std::fs::create_dir_all(&cfg.index_root)?;
    let lock = publish::lock_section(&cfg.index_root, &cfg.section_id)?;
    let version = publish::new_version_name();
    let staging = publish::create_staging(&cfg.index_root, &cfg.section_id, &version)?;
    let discard = |staging: &Path| {
        let _ = std::fs::remove_dir_all(staging);
        // Remove the per-section versions dir if this was the first (failed) attempt.
        let _ = std::fs::remove_dir(publish::versions_dir(&cfg.index_root, &cfg.section_id));
    };
    if let Err(e) = build(cfg, &staging) {
        discard(&staging);
        return Err(
            e.context("ingest failed; the previously published index (if any) is unchanged")
        );
    }
    if let Err(e) = crate::validate::validate_dir(&staging).and_then(|r| {
        if r.errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!("validation errors: {:#?}", r.errors))
        }
    }) {
        discard(&staging);
        return Err(e.context(
            "new index failed validation; the previously published index (if any) is unchanged",
        ));
    }
    publish::publish(
        &cfg.index_root,
        &cfg.section_id,
        &staging,
        &version,
        cfg.keep_versions,
        &lock,
    )
}

fn build(cfg: &IngestConfig, out: &Path) -> Result<()> {
    let mut rep = Report {
        steps: vec![],
        t: Instant::now(),
    };
    let grid = TileGrid::new(cfg.tile_size_um);
    let files = XeniumFiles::discover(&cfg.xenium_dir)?;
    let mut warnings: Vec<String> = files
        .unsupported
        .iter()
        .map(|u| format!("not used: {u}"))
        .collect();
    if !files.morphology_images.is_empty() && cfg.morphology.is_none() {
        warnings.push(format!(
            "{} Xenium morphology image(s) found but not indexed (pass --morphology <xenium>/morphology_focus to add the channels)",
            files.morphology_images.len()
        ));
    }
    let mut sources: Vec<SourceFingerprint> = vec![];
    let xenium = match &files.experiment {
        Some(p) => {
            sources.push(fingerprint("experiment.xenium", p)?);
            read_experiment(p)?
        }
        None => {
            warnings.push("experiment.xenium not found: Xenium pixel size unknown".into());
            Default::default()
        }
    };

    // ---------------- cells ----------------
    let cells_path = files.require_cells()?;
    sources.push(fingerprint("cells", cells_path)?);
    let table = read_cells(cells_path)?;
    if table.dropped_rows > 0 {
        warnings.push(format!(
            "cells.parquet: {} rows dropped (missing cell_id or non-finite centroid)",
            table.dropped_rows
        ));
    }
    for s in &table.skipped_columns {
        warnings.push(format!("cells.parquet: column skipped: {s}"));
    }
    let dict = CellDictionary::from_ids(table.ids.clone()).context("cells.parquet cell_id")?;
    let lookup = dict.lookup_map();
    let n = dict.len();
    if n == 0 {
        bail!("cells.parquet contains no usable cells");
    }
    let key_of_row: Vec<u32> = table.ids.iter().map(|id| lookup[id.as_str()]).collect();
    let mut centroids = vec![0f32; 2 * n];
    for (row, &k) in key_of_row.iter().enumerate() {
        let i = (k - 1) as usize;
        centroids[2 * i] = table.centroids[row][0] as f32;
        centroids[2 * i + 1] = table.centroids[row][1] as f32;
    }
    let cells_dir = out.join("cells");
    std::fs::create_dir_all(&cells_dir)?;
    let mut field_sink = FieldSink::create(&cells_dir, cfg.layout)?;
    std::fs::write(cells_dir.join("ids.bin"), dict.to_bytes())?;
    std::fs::write(
        cells_dir.join("centroids.bin"),
        encode_centroids(&centroids),
    )?;
    let mut fields: Vec<FieldInfo> = vec![];
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for (name, vals) in &table.numeric {
        let mut v = vec![f32::NAN; n];
        for (row, &k) in key_of_row.iter().enumerate() {
            v[(k - 1) as usize] = vals[row] as f32;
        }
        let id = unique_id(&sanitize_id(name), &mut taken);
        let f = ImportedField {
            info: field_info(
                &id,
                name,
                "cells",
                "cells.parquet",
                &FieldColumn::Numeric(v.clone()),
                None,
            ),
            column: FieldColumn::Numeric(v),
        };
        field_sink.push(&mut fields, f)?;
    }
    for (name, vals) in &table.categorical {
        let mut v: Vec<Option<String>> = vec![None; n];
        for (row, &k) in key_of_row.iter().enumerate() {
            v[(k - 1) as usize] = vals[row].clone();
        }
        let (col, cats) = categorical(&v);
        let id = unique_id(&sanitize_id(name), &mut taken);
        let f = ImportedField {
            info: field_info(&id, name, "cells", "cells.parquet", &col, Some(cats)),
            column: col,
        };
        field_sink.push(&mut fields, f)?;
    }
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut grow = |b: [f64; 4]| {
        bounds[0] = bounds[0].min(b[0]);
        bounds[1] = bounds[1].min(b[1]);
        bounds[2] = bounds[2].max(b[2]);
        bounds[3] = bounds[3].max(b[3]);
    };
    for c in centroids.chunks_exact(2) {
        grow([c[0] as f64, c[1] as f64, c[0] as f64, c[1] as f64]);
    }
    rep.step(
        "cells",
        json!({"cells": n, "fields": fields.len(), "id_was_integer": table.id_was_integer}),
    );

    // ---------------- polygons ----------------
    let shape_info = |kind: ShapeKind,
                      path: &Option<PathBuf>,
                      role: &str,
                      sources: &mut Vec<SourceFingerprint>,
                      warnings: &mut Vec<String>|
     -> Result<Option<(ShapeLayerInfo, [f64; 4])>> {
        let Some(path) = path else {
            return Ok(None);
        };
        sources.push(fingerprint(role, path)?);
        let mut chunks: BTreeMap<u32, Vec<ChunkPolygon>> = BTreeMap::new();
        let (mut unknown, mut seq, mut margin, mut verts) = (0u64, 0u64, 0f64, 0u64);
        let mut b = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        let stats = for_each_polygon(path, |p| {
            let Some(&key) = lookup.get(p.cell_id.as_str()) else {
                unknown += 1;
                return Ok(());
            };
            let (cx, cy) = (
                centroids[2 * (key as usize - 1)] as f64,
                centroids[2 * (key as usize - 1) + 1] as f64,
            );
            let (tx, ty) = grid
                .tile_of(cx, cy)
                .ok_or_else(|| anyhow!("centroid out of grid range"))?;
            for v in &p.ring {
                let (x, y) = (v[0] as f64, v[1] as f64);
                margin = margin.max((x - cx).abs()).max((y - cy).abs());
                b[0] = b[0].min(x);
                b[1] = b[1].min(y);
                b[2] = b[2].max(x);
                b[3] = b[3].max(y);
            }
            verts += p.ring.len() as u64;
            seq += 1;
            chunks
                .entry(tile_key(tx, ty))
                .or_default()
                .push((key, p.label_id, seq, p.ring));
            Ok(())
        })?;
        if unknown > 0 {
            warnings.push(format!(
                "{role}: {unknown} polygons belong to cell_ids not in cells.parquet; skipped"
            ));
        }
        if stats.degenerate + stats.non_finite + stats.null_ids > 0 {
            warnings.push(format!(
                "{role}: skipped {} degenerate (<3 vertices), {} non-finite polygons and {} rows without cell_id",
                stats.degenerate, stats.non_finite, stats.null_ids
            ));
        }
        let base = cells_dir.join(match kind {
            ShapeKind::Cell => "cell_chunks",
            ShapeKind::Nucleus => "nucleus_chunks",
        });
        let mut pack = PackWriter::create(&base)?;
        let mut chunk_list = vec![];
        let mut polygons = 0u64;
        for (key, mut polys) in chunks {
            polys.sort_by_key(|p| (p.0, p.1, p.2));
            let mut c = PolygonChunk::new();
            for (k, _, _, ring) in &polys {
                c.push(*k, ring);
            }
            polygons += c.len() as u64;
            let (tx, ty) = tile_from_key(key);
            chunk_list.push([tx as i64, ty as i64, c.len() as i64]);
            pack.add(key, &encode_polygons(kind, &c))?;
        }
        pack.finish()?;
        Ok(Some((
            ShapeLayerInfo {
                polygons,
                vertices: verts,
                query_margin_um: (margin * 1.0001 + 1e-3).ceil(),
                chunks: chunk_list,
                skipped_polygons: unknown + stats.degenerate + stats.non_finite,
            },
            b,
        )))
    };
    let boundaries = shape_info(
        ShapeKind::Cell,
        &files.cell_boundaries,
        "cell_boundaries",
        &mut sources,
        &mut warnings,
    )?;
    let nuclei = shape_info(
        ShapeKind::Nucleus,
        &files.nucleus_boundaries,
        "nucleus_boundaries",
        &mut sources,
        &mut warnings,
    )?;
    for (_, b) in boundaries.iter().chain(nuclei.iter()) {
        if b[0].is_finite() {
            grow(*b);
        }
    }
    if boundaries.is_none() {
        warnings.push("cell_boundaries.parquet not found: cell polygons unavailable".into());
    }
    rep.step(
        "polygons",
        json!({"cells": boundaries.as_ref().map(|b| b.0.polygons), "nuclei": nuclei.as_ref().map(|b| b.0.polygons)}),
    );

    // ---------------- annotations & LR ----------------
    let mut annotation_done: BTreeSet<String> = BTreeSet::new();
    for path in &cfg.annotations {
        sources.push(fingerprint("annotations", path)?);
        let t = read_table(path)?;
        let r = import_annotations(
            &t,
            &cfg.annotation_id_column,
            &dict,
            &base_name(path),
            &taken,
            cfg.annotation_columns.as_deref(),
            &mut annotation_done,
        )?;
        warnings.extend(r.warnings);
        for f in r.fields {
            if !taken.insert(f.info.id.clone()) {
                bail!(
                    "{}: field id {:?} is already used",
                    path.display(),
                    f.info.id
                );
            }
            field_sink.push(&mut fields, f)?;
        }
    }
    if let Some(specs) = &cfg.annotation_columns {
        let missing: Vec<&str> = specs
            .iter()
            .filter(|sp| !annotation_done.iter().any(|c| sp.matches(c)))
            .map(|sp| sp.pattern.as_str())
            .collect();
        if !missing.is_empty() {
            bail!("requested annotation columns not found in any annotation table: {missing:?}");
        }
    }
    for path in &cfg.lr_scores {
        sources.push(fingerprint("lr_scores", path)?);
        let t = read_table(path)?;
        let r = import_lr_scores(&t, &dict, &base_name(path))?;
        warnings.extend(r.warnings);
        for f in r.fields {
            if !taken.insert(f.info.id.clone()) {
                bail!(
                    "{}: field id {:?} is already used",
                    path.display(),
                    f.info.id
                );
            }
            field_sink.push(&mut fields, f)?;
        }
    }
    for path in &cfg.column_bundles {
        sources.push(fingerprint("column_bundle", path)?);
        let r = crate::bundle_ingest::import_column_bundle(path, &dict, &taken)?;
        warnings.extend(r.warnings);
        for f in r.fields {
            taken.insert(f.info.id.clone());
            field_sink.push(&mut fields, f)?;
        }
    }
    field_sink.finish()?;
    rep.step("annotations", json!({"fields": fields.len()}));

    // ---------------- grids ----------------
    let mut grids = vec![];
    for g in &cfg.grids {
        sources.push(fingerprint("grid", &g.path)?);
        let t = read_table(&g.path)?;
        let spec = GridSpec {
            id: &g.id,
            label: &g.label,
            x_col: &g.x_col,
            y_col: &g.y_col,
            spacing: g.spacing_um,
            select: g.columns.as_deref(),
        };
        let ig = import_grid(&t, &spec, &base_name(&g.path))?;
        warnings.extend(ig.warnings);
        let gdir = out.join("grids").join(&g.id);
        std::fs::create_dir_all(&gdir)?;
        for (info, raster) in ig.info.fields.iter().zip(&ig.rasters) {
            std::fs::write(
                gdir.join(format!("{}.bin", info.id)),
                encode_grid_raster(ig.info.width, ig.info.height, raster)?,
            )?;
        }
        grids.push(ig.info);
    }
    rep.step("grids", json!({"grids": grids.len()}));

    // ---------------- transcripts ----------------
    let transcripts = match (&files.transcripts, cfg.skip_transcripts) {
        (Some(p), false) => {
            sources.push(fingerprint("transcripts", p)?);
            let b = build_transcripts(
                p,
                &out.join("transcripts"),
                &out.join("tmp"),
                &lookup,
                n,
                grid,
            )?;
            if let Some(tb) = b.bounds {
                grow(tb);
            }
            warnings.extend(b.warnings);
            rep.step(
                "transcripts",
                json!({"indexed": b.info.indexed, "genes": b.info.genes.len()}),
            );
            Some(b.info)
        }
        (Some(_), true) => {
            warnings.push("transcripts skipped by request (--skip-transcripts)".into());
            None
        }
        (None, _) => {
            warnings.push("transcripts.parquet not found".into());
            None
        }
    };
    drop(lookup);

    // ---------------- images ----------------
    let mut images = vec![];
    for img in &cfg.images {
        sources.push(fingerprint(&format!("image:{}", img.id), &img.path)?);
        let (spec, prov) = match &img.registration {
            RegistrationInput::Json(p) => {
                sources.push(fingerprint(&format!("registration:{}", img.id), p)?);
                let text = std::fs::read_to_string(p)?;
                let v: serde_json::Value = serde_json::from_str(&text)?;
                let prov = v
                    .get("provenance")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                (
                    RegistrationSpec::from_json(&text)?,
                    format!("{} {prov}", base_name(p)).trim().to_string(),
                )
            }
            RegistrationInput::AlignmentCsv { path, units } => {
                sources.push(fingerprint(&format!("alignment:{}", img.id), path)?);
                let m = parse_alignment_csv(&std::fs::read_to_string(path)?)?;
                (
                    RegistrationSpec::XeniumExplorerAlignment {
                        matrix: m,
                        output_units: *units,
                    },
                    base_name(path),
                )
            }
            RegistrationInput::Spec(s) => (s.clone(), "inline".to_string()),
        };
        let reg = spec
            .resolve(xenium.pixel_size_um, prov)
            .with_context(|| format!("registration for image {:?}", img.id))?;
        let p = probe(&img.path)?;
        let mut reader = BandReader::open(&img.path, img.ifds.as_deref())?;
        let mut notes = vec![format!(
            "level-0 IFD(s) {:?}; {}x{} px; {} channel(s)",
            reader.ifd_indices(),
            reader.width,
            reader.height,
            reader.channels()
        )];
        if let Some((lo, hi)) = reader.window16() {
            notes.push(format!(
                "16-bit samples windowed linearly from [{lo}, {hi}] to 8-bit"
            ));
        }
        if let Some((px, _)) = p.ome_physical_size_um {
            let ratio = reg.approx_pixel_size_um / px;
            let msg = format!(
                "OME metadata pixel size {px:.4} µm; registration implies {:.4} µm/px",
                reg.approx_pixel_size_um
            );
            if (ratio - 1.0).abs() > 0.05 {
                warnings.push(format!(
                    "image {}: {msg} (differs by {:.0}%) — check registration units",
                    img.id,
                    (ratio - 1.0) * 100.0
                ));
            }
            notes.push(msg);
        }
        if reg.is_reflection {
            notes.push("registration includes a reflection (mirror)".into());
        }
        let info = build_pyramid(
            &mut reader,
            &out.join("images").join(&img.id),
            cfg.image_tile_px,
            cfg.jpeg_quality,
            cfg.layout,
        )
        .with_context(|| format!("build pyramid for {}", img.path.display()))?;
        let bounds_um = reg.matrix_xu_from_px.transform_rect_bounds([
            0.0,
            0.0,
            info.width as f64,
            info.height as f64,
        ]);
        images.push(ImageInfo {
            id: img.id.clone(),
            label: img.label.clone(),
            kind: if info.channels == 1 {
                ImageKind::Gray
            } else {
                ImageKind::Rgb
            },
            width: info.width,
            height: info.height,
            tile_size: info.tile_size,
            levels: info.levels,
            format: "jpeg".into(),
            registration: reg,
            bounds_um,
            source_file: base_name(&img.path),
            notes,
            channel: None,
            tile_layout: cfg.layout,
        });
        rep.step(
            &format!("image:{}", img.id),
            json!({"levels": info.levels, "tiles": info.tiles}),
        );
    }

    // ---------------- morphology channels ----------------
    if let Some(dir) = &cfg.morphology {
        let files = xenium_io::morphology::channel_files(dir)?;
        let mut used: BTreeSet<String> = images.iter().map(|i: &ImageInfo| i.id.clone()).collect();
        for (i, path) in files.iter().enumerate() {
            let t0 = Instant::now();
            let ch = xenium_io::morphology::MorphologyChannel::open(path)?;
            sources.push(fingerprint("morphology", path)?);
            let px_um = ch.pixel_size_um.or(xenium.pixel_size_um).ok_or_else(|| {
                anyhow!(
                    "{}: no pixel size in the OME metadata or experiment.xenium",
                    path.display()
                )
            })?;
            let mut notes = vec![];
            if let (Some(o), Some(x)) = (ch.pixel_size_um, xenium.pixel_size_um) {
                if (o / x - 1.0).abs() > 0.01 {
                    notes.push(format!("OME pixel size {o} µm differs from experiment.xenium ({x} µm); the OME value is used"));
                }
            }
            let reg = RegistrationSpec::AlreadyAligned {
                pixel_size_um: px_um,
            }
            .resolve(
                xenium.pixel_size_um,
                format!("Xenium morphology pixel grid ({})", base_name(path)),
            )?;
            let (lo, hi) = ch
                .estimate_window()
                .with_context(|| format!("contrast window for {}", path.display()))?;
            notes.push(format!(
                "{}x{} px, {}-bit JPEG 2000; intensities [{lo}, {hi}] (0.5th-99.9th percentile of non-zero pixels) mapped linearly to 8-bit",
                ch.width(),
                ch.height(),
                ch.tiff.bits_per_sample
            ));
            let mut id = format!("morph-{}", slug(&ch.name));
            while !used.insert(id.clone()) {
                id.push('x');
            }
            let mut bands = ch.bands((lo, hi));
            let info = build_pyramid(
                &mut bands,
                &out.join("images").join(&id),
                cfg.image_tile_px,
                cfg.jpeg_quality,
                cfg.layout,
            )
            .with_context(|| format!("build pyramid for {}", path.display()))?;
            let bounds_um = reg.matrix_xu_from_px.transform_rect_bounds([
                0.0,
                0.0,
                info.width as f64,
                info.height as f64,
            ]);
            images.push(ImageInfo {
                id: id.clone(),
                label: ch.name.clone(),
                kind: ImageKind::Gray,
                width: info.width,
                height: info.height,
                tile_size: info.tile_size,
                levels: info.levels,
                format: "jpeg".into(),
                registration: reg,
                bounds_um,
                source_file: base_name(path),
                notes,
                channel: Some(ChannelInfo {
                    name: ch.name.clone(),
                    window: [lo as u32, hi as u32],
                    color: default_channel_color(&ch.name, i),
                }),
                tile_layout: cfg.layout,
            });
            rep.step(
                &format!("image:{id}"),
                json!({"levels": info.levels, "tiles": info.tiles, "window": [lo, hi], "seconds": t0.elapsed().as_secs_f64()}),
            );
        }
    }

    if !bounds[0].is_finite() {
        bail!("no spatial data found");
    }
    let extent = (bounds[2] - bounds[0]).max(bounds[3] - bounds[1]).max(1.0);
    let transcripts = transcripts.map(|mut t| {
        let mut l = 0u32;
        while (DENSITY_TILE_BINS as f64) * (1u64 << l) as f64 * 2.0 < extent && l < 16 {
            l += 1;
        }
        t.density_max_level = l.max(1);
        t
    });
    let manifest = SectionManifest {
        schema_version: SCHEMA_VERSION,
        section_id: cfg.section_id.clone(),
        display_name: cfg
            .display_name
            .clone()
            .unwrap_or_else(|| cfg.section_id.clone()),
        created_by: format!("xenium-web {}", env!("CARGO_PKG_VERSION")),
        created_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        world_frame: "xenium_um".into(),
        bounds_um: bounds,
        grid,
        xenium,
        sources,
        cells: Some(CellsInfo {
            count: n as u64,
            boundaries: boundaries.map(|b| b.0),
            nuclei: nuclei.map(|b| b.0),
            fields,
            field_layout: cfg.layout,
        }),
        transcripts,
        images,
        grids,
        warnings: warnings.clone(),
    };
    manifest.validate().map_err(|e| anyhow!(e))?;
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::write(
        out.join("import_report.json"),
        serde_json::to_vec_pretty(&json!({
            "section_id": cfg.section_id,
            "xenium_files": {
                "experiment": files.experiment.as_deref().map(base_name),
                "cells": files.cells.as_deref().map(base_name),
                "cell_boundaries": files.cell_boundaries.as_deref().map(base_name),
                "nucleus_boundaries": files.nucleus_boundaries.as_deref().map(base_name),
                "transcripts": files.transcripts.as_deref().map(base_name),
                "morphology_images": files.morphology_images.iter().map(|p| base_name(p)).collect::<Vec<_>>(),
            },
            "steps": rep.steps,
            "warnings": warnings,
        }))?,
    )?;
    Ok(())
}

/// Lower-case id fragment: letters and digits, other runs become `-`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "channel".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::unique_id;
    use std::collections::BTreeSet;

    #[test]
    fn colliding_field_ids_get_suffixes() {
        let mut taken = BTreeSet::new();
        assert_eq!(unique_id("a_b", &mut taken), "a_b");
        assert_eq!(unique_id("a_b", &mut taken), "a_b_2");
        assert_eq!(unique_id("a_b", &mut taken), "a_b_3");
    }
}
