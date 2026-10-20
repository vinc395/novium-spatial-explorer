//! Structural and consistency validation of an index directory. Used before
//! publishing and by `xenium-cli validate`.

use crate::pack::PackReader;
use crate::pyramid::image_tile_key;
use crate::store::SectionStore;
use crate::transcripts_ingest::{read_gene_table, DIR_ENTRY_BYTES};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use xenium_core::codec::{
    decode_field, decode_grid_raster, decode_polygons, FieldColumn, PointRecord, POINT_RECORD_BYTES,
};
use xenium_core::grid::{tile_from_key, tile_key};
use xenium_core::manifest::Layout;

#[derive(Debug, Default, Serialize)]
pub struct ValidationReport {
    pub section_id: String,
    pub passed: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

macro_rules! check {
    ($r:expr, $cond:expr, $ok:expr, $($err:tt)+) => {
        if $cond { $r.passed.push($ok.to_string()) } else { $r.errors.push(format!($($err)+)) }
    };
}

pub fn validate_dir(dir: &Path) -> Result<ValidationReport> {
    let mut r = ValidationReport::default();
    let store = match SectionStore::open(dir) {
        Ok(s) => s,
        Err(e) => {
            r.errors.push(format!("cannot open index: {e:#}"));
            return Ok(r);
        }
    };
    let m = &store.manifest;
    r.section_id = m.section_id.clone();
    r.warnings = m.warnings.clone();
    let n = store.n_cells();
    let cells = m.cells.as_ref().unwrap();
    check!(
        r,
        cells.count as usize == n,
        "cell count matches dictionary",
        "manifest cell count {} != dictionary {}",
        cells.count,
        n
    );

    let mut ids = std::collections::HashSet::new();
    let dup: Vec<&str> = cells
        .fields
        .iter()
        .map(|f| f.id.as_str())
        .filter(|id| !ids.insert(*id))
        .collect();
    check!(
        r,
        dup.is_empty(),
        "field ids unique",
        "duplicate field ids: {dup:?}"
    );

    if let Some(n_packed) = store.field_pack_len() {
        check!(
            r,
            n_packed == cells.fields.len(),
            "field pack matches the manifest",
            "field pack has {n_packed} entries for {} fields",
            cells.fields.len()
        );
    }
    for f in &cells.fields {
        match store
            .field_bytes(&f.id)
            .map_err(|e| e.to_string())
            .and_then(|b| decode_field(&b).map_err(|e| e.to_string()))
        {
            Ok(FieldColumn::Numeric(v)) => check!(
                r,
                v.len() == n,
                format!("field {} length", f.id),
                "field {} has {} values, expected {n}",
                f.id,
                v.len()
            ),
            Ok(FieldColumn::Categorical {
                codes,
                n_categories,
            }) => {
                let ncat = f.categories.as_ref().map(|c| c.len()).unwrap_or(0);
                check!(
                    r,
                    codes.len() == n && n_categories as usize == ncat,
                    format!("field {} length/categories", f.id),
                    "field {}: {} codes / {} categories, expected {n} / {ncat}",
                    f.id,
                    codes.len(),
                    n_categories
                );
            }
            Err(e) => r.errors.push(format!("field {}: {e}", f.id)),
        }
    }

    for (name, info, nucleus) in [
        ("cell_chunks", &cells.boundaries, false),
        ("nucleus_chunks", &cells.nuclei, true),
    ] {
        let Some(info) = info else { continue };
        let pack = PackReader::open(&dir.join("cells").join(name))?;
        let (mut polys, mut bad_key, mut outside, mut wrong_chunk) = (0u64, 0u64, 0u64, 0u64);
        for key in pack.keys() {
            let (tx, ty) = tile_from_key(key);
            let (kind, c) = decode_polygons(&pack.get(key)?.unwrap())?;
            if kind != nucleus as u16 {
                r.errors
                    .push(format!("{name}: chunk ({tx},{ty}) has wrong kind {kind}"));
            }
            polys += c.len() as u64;
            for (i, &k) in c.cell_keys.iter().enumerate() {
                let Some(cen) = store.centroid(k) else {
                    bad_key += 1;
                    continue;
                };
                if store.grid().tile_of(cen[0] as f64, cen[1] as f64) != Some((tx, ty)) {
                    wrong_chunk += 1;
                }
                for v in
                    c.xy[2 * c.starts[i] as usize..2 * c.starts[i + 1] as usize].chunks_exact(2)
                {
                    let d = ((v[0] - cen[0]).abs()).max((v[1] - cen[1]).abs()) as f64;
                    if d > info.query_margin_um {
                        outside += 1;
                    }
                }
            }
        }
        check!(
            r,
            polys == info.polygons,
            format!("{name}: polygon count"),
            "{name}: {polys} polygons, manifest says {}",
            info.polygons
        );
        check!(
            r,
            pack.len() == info.chunks.len(),
            format!("{name}: chunk count"),
            "{name}: {} chunks, manifest says {}",
            pack.len(),
            info.chunks.len()
        );
        check!(
            r,
            bad_key == 0,
            format!("{name}: cell keys valid"),
            "{name}: {bad_key} polygons with invalid cell keys"
        );
        check!(
            r,
            wrong_chunk == 0,
            format!("{name}: centroid chunking"),
            "{name}: {wrong_chunk} polygons stored outside their centroid's chunk"
        );
        check!(
            r,
            outside == 0,
            format!("{name}: query margin covers all vertices"),
            "{name}: {outside} vertices beyond query margin"
        );
    }

    if let Some(t) = &m.transcripts {
        let td = dir.join("transcripts");
        let genes = read_gene_table(&td.join("genes.bin"))?;
        let total: u64 = genes.iter().map(|g| g.count).sum();
        check!(
            r,
            total == t.indexed,
            "transcript count matches gene table",
            "gene table sums to {total}, manifest indexed {}",
            t.indexed
        );
        let pts_len = std::fs::metadata(td.join("points.bin"))?.len();
        check!(
            r,
            pts_len == t.indexed * POINT_RECORD_BYTES as u64,
            "points.bin size",
            "points.bin is {pts_len} bytes, expected {}",
            t.indexed * POINT_RECORD_BYTES as u64
        );
        let dirs_len = std::fs::metadata(td.join("gene_dirs.bin"))?.len();
        let dir_total: u64 = genes.iter().map(|g| g.dir_len as u64).sum();
        check!(
            r,
            dirs_len == dir_total * DIR_ENTRY_BYTES as u64,
            "gene_dirs.bin size",
            "gene_dirs.bin size mismatch"
        );
        let mut bad = 0;
        for (i, (g, info)) in genes.iter().zip(&t.genes).enumerate() {
            if g.count != info.count {
                bad += 1;
                continue;
            }
            let dir = store.gene_dir(i as u32)?;
            let sum: u64 = dir.iter().map(|e| e.count as u64).sum();
            let ordered = dir
                .windows(2)
                .all(|w| w[0].tile_key < w[1].tile_key && w[0].start + w[0].count == w[1].start);
            if sum != g.count || !ordered || dir.first().is_some_and(|e| e.start != 0) {
                bad += 1;
                continue;
            }
            // Spot-check that records lie in their directory tile.
            for e in dir.iter().take(8) {
                let raw = store.points_tile(
                    tile_from_key(e.tile_key).0,
                    tile_from_key(e.tile_key).1,
                    &[i as u32],
                    0,
                )?;
                let dec = xenium_core::codec::decode_points(&raw)?;
                if dec.records.len() != e.count as usize
                    || dec.records.iter().any(|p: &PointRecord| {
                        store
                            .grid()
                            .tile_of(p.x as f64, p.y as f64)
                            .map(|(x, y)| tile_key(x, y))
                            != Some(e.tile_key)
                    })
                {
                    bad += 1;
                }
            }
        }
        check!(
            r,
            bad == 0,
            "gene directories consistent",
            "{bad} genes with inconsistent directories/records"
        );

        // Expression tables: keys/genes in range, per-row sorting, and equal totals.
        let n_genes = genes.len() as u32;
        let csc = std::fs::read(td.join("expr_csc.bin"))?;
        let (mut csc_sum, mut bad_csc) = (0u64, 0u64);
        for (g, e) in genes.iter().enumerate() {
            let s = (e.csc_offset * 8) as usize;
            let rows: Vec<(u32, u32)> = csc[s..s + e.csc_len as usize * 8]
                .chunks_exact(8)
                .map(|c| {
                    (
                        u32::from_le_bytes(c[0..4].try_into().unwrap()),
                        u32::from_le_bytes(c[4..8].try_into().unwrap()),
                    )
                })
                .collect();
            if rows
                .iter()
                .any(|&(k, c)| k == 0 || k as usize > n || c == 0)
                || rows.windows(2).any(|w| w[0].0 >= w[1].0)
            {
                bad_csc += 1;
            }
            csc_sum += rows.iter().map(|r| r.1 as u64).sum::<u64>();
            let _ = g;
        }
        let csr = std::fs::read(td.join("expr_csr.bin"))?;
        let mut csr_sum = 0u64;
        let mut bad_csr = 0u64;
        for c in csr.chunks_exact(8) {
            let gene = u32::from_le_bytes(c[0..4].try_into().unwrap());
            if gene >= n_genes {
                bad_csr += 1;
            }
            csr_sum += u32::from_le_bytes(c[4..8].try_into().unwrap()) as u64;
        }
        check!(
            r,
            bad_csc == 0 && bad_csr == 0,
            "expression tables in range",
            "{bad_csc} genes / {bad_csr} cell entries out of range"
        );
        check!(
            r,
            csc_sum == csr_sum,
            "expression tables agree",
            "per-gene total {csc_sum} != per-cell total {csr_sum}"
        );
    }

    // Tile presence comes from the pack index or, in the per-file layout, one
    // directory listing per level: reading tens of thousands of tiles back is slow on a
    // USB or network disk. The JPEG signature is spot-checked on up to 8 tiles per level.
    for im in &m.images {
        let pack = match im.tile_layout {
            Layout::Packed => match store.image_pack(&im.id) {
                Ok(p) => Some(p),
                Err(e) => {
                    r.errors
                        .push(format!("image {}: tile pack unreadable: {e:#}", im.id));
                    continue;
                }
            },
            Layout::Files => None,
        };
        let (mut missing, mut not_jpeg, mut checked, mut expected) = (0u64, 0, 0, 0u64);
        for l in 0..im.levels {
            let ts = im.tile_size as u64 * (1 << l);
            let across = (im.width as u64).div_ceil(ts);
            let down = (im.height as u64).div_ceil(ts);
            let listed: HashSet<String> = match pack {
                Some(_) => HashSet::new(),
                None => std::fs::read_dir(store.image_dir(&im.id).join(l.to_string()))
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            let n = across * down;
            expected += n;
            let step = n.div_ceil(8).max(1);
            for i in 0..n {
                let (x, y) = ((i % across) as u32, (i / across) as u32);
                let present = match &pack {
                    Some(p) => image_tile_key(l, x, y).is_some_and(|k| p.contains(k)),
                    None => listed.contains(&format!("{x}_{y}.jpg")),
                };
                if !present {
                    missing += 1;
                } else if i % step == 0 {
                    checked += 1;
                    let ok = store
                        .image_tile(&im.id, l, x, y)
                        .is_ok_and(|b| b.starts_with(&[0xFF, 0xD8]));
                    if !ok {
                        not_jpeg += 1;
                    }
                }
            }
        }
        // Every pack entry is one of the expected tiles (no stray keys).
        let extra = pack
            .as_ref()
            .map_or(0, |p| p.len() as u64 - (expected - missing));
        check!(
            r,
            missing == 0 && not_jpeg == 0 && extra == 0,
            format!(
                "image {}: all tiles present ({checked} spot-checked)",
                im.id
            ),
            "image {}: {missing} missing / {not_jpeg} invalid / {extra} unexpected tiles",
            im.id
        );
    }

    for g in &m.grids {
        for f in &g.fields {
            match store
                .grid_raster(&g.id, &f.id)
                .map_err(|e| e.to_string())
                .and_then(|b| decode_grid_raster(&b).map_err(|e| e.to_string()))
            {
                Ok((w, h, _)) => check!(
                    r,
                    w == g.width && h == g.height,
                    format!("grid {}/{} size", g.id, f.id),
                    "grid {}/{} is {w}x{h}",
                    g.id,
                    f.id
                ),
                Err(e) => r.errors.push(format!("grid {}/{}: {e}", g.id, f.id)),
            }
        }
    }
    Ok(r)
}
