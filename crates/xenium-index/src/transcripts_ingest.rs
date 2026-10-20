//! Transcript indexing by external sort (bounded memory).
//!
//! Pass 1 streams `transcripts.parquet` once, validates and quantises each row and
//! appends a 20-byte temporary record to one of `BUCKETS` files (by gene).
//! Pass 2 loads one bucket at a time, sorts it by `(gene, tile, y, x, ...)` and
//! appends to the final files. Pass 3 transposes per-(gene, cell) counts into a
//! per-cell table. Peak memory is roughly one bucket, not the whole table.
//!
//! Output (`transcripts/`):
//! * `points.bin`     16-byte [`PointRecord`]s grouped by gene, then tile, then y, x.
//! * `gene_dirs.bin`  per gene: `(tile_key u32, rel_start u32, count u32)` entries.
//! * `genes.bin`      `XGN1 | u32 n | n x GeneEntry (40 bytes)`.
//! * `expr_csc.bin`   per gene: `(cell_key u32, count u32)`, QV >= 20, assigned only.
//! * `expr_csr.bin` + `expr_csr.idx`  per cell: `(gene u32, count u32)`.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use xenium_core::codec::{
    PointRecord, POINT_FLAG_OVERLAPS_NUCLEUS, POINT_FLAG_UNKNOWN_CELL, POINT_RECORD_BYTES, QV_SCALE,
};
use xenium_core::features::FeatureCategory;
use xenium_core::grid::{tile_key, TileGrid};
use xenium_core::manifest::{GeneInfo, TranscriptsInfo};
use xenium_io::columns::{bool_at, f64_at, str_at, u8_at};
use xenium_io::transcripts::{is_unassigned_cell_id, stream_transcripts};

pub const BUCKETS: usize = 128;
const CSR_BUCKETS: usize = 64;
const TMP_BYTES: usize = 4 + POINT_RECORD_BYTES;
pub const GENE_ENTRY_BYTES: usize = 40;
pub const DIR_ENTRY_BYTES: usize = 12;
/// QV threshold used for expression counts (matches the XOA feature matrix definition).
pub const EXPR_QV_MIN: f32 = 20.0;
pub const DENSITY_TILE_BINS: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GeneEntry {
    pub points_offset: u64,
    pub count: u64,
    pub dir_offset: u64,
    pub dir_len: u32,
    pub csc_len: u32,
    pub csc_offset: u64,
}

impl GeneEntry {
    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.points_offset.to_le_bytes());
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend_from_slice(&self.dir_offset.to_le_bytes());
        out.extend_from_slice(&self.dir_len.to_le_bytes());
        out.extend_from_slice(&self.csc_len.to_le_bytes());
        out.extend_from_slice(&self.csc_offset.to_le_bytes());
    }

    pub fn read(b: &[u8]) -> Self {
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        GeneEntry {
            points_offset: u64_at(0),
            count: u64_at(8),
            dir_offset: u64_at(16),
            dir_len: u32_at(24),
            csc_len: u32_at(28),
            csc_offset: u64_at(32),
        }
    }
}

pub fn read_gene_table(path: &Path) -> Result<Vec<GeneEntry>> {
    let b = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if b.len() < 8 || &b[..4] != b"XGN1" {
        bail!("{}: bad gene table", path.display());
    }
    let n = u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize;
    if b.len() != 8 + n * GENE_ENTRY_BYTES {
        bail!("{}: truncated gene table", path.display());
    }
    Ok(b[8..]
        .chunks_exact(GENE_ENTRY_BYTES)
        .map(GeneEntry::read)
        .collect())
}

pub struct TranscriptBuild {
    pub info: TranscriptsInfo,
    pub bounds: Option<[f64; 4]>,
    pub warnings: Vec<String>,
}

struct Pass1Gene {
    name: String,
    category: FeatureCategory,
    count: u64,
    count_qv20: u64,
}

#[derive(Clone, Copy)]
struct SortRec {
    gene: u32,
    tile: u32,
    rec: PointRecord,
}

fn sort_key(a: &SortRec, b: &SortRec) -> std::cmp::Ordering {
    a.gene
        .cmp(&b.gene)
        .then(a.tile.cmp(&b.tile))
        .then(a.rec.y.total_cmp(&b.rec.y))
        .then(a.rec.x.total_cmp(&b.rec.x))
        .then(a.rec.cell_key.cmp(&b.rec.cell_key))
        .then(a.rec.z_centi.cmp(&b.rec.z_centi))
        .then(a.rec.qv_q.cmp(&b.rec.qv_q))
        .then(a.rec.flags.cmp(&b.rec.flags))
}

pub fn build_transcripts(
    path: &Path,
    out_dir: &Path,
    tmp_dir: &Path,
    cell_lookup: &HashMap<&str, u32>,
    n_cells: usize,
    grid: TileGrid,
) -> Result<TranscriptBuild> {
    std::fs::create_dir_all(out_dir)?;
    std::fs::create_dir_all(tmp_dir)?;
    let qv20_q = (EXPR_QV_MIN * QV_SCALE) as u8;

    // ---------------- pass 1 ----------------
    let mut writers: Vec<Option<BufWriter<File>>> = (0..BUCKETS).map(|_| None).collect();
    let mut gene_ids: HashMap<String, u32> = HashMap::new();
    let mut genes: Vec<Pass1Gene> = vec![];
    let (mut rows, mut skipped, mut unassigned, mut unknown) = (0u64, 0u64, 0u64, 0u64);
    let mut bounds: Option<[f64; 4]> = None;
    let mut warnings = vec![];
    let mut buf = Vec::with_capacity(TMP_BYTES);
    let t0 = std::time::Instant::now();
    let cols = stream_transcripts(path, |cols, b| {
        for i in 0..b.len {
            rows += 1;
            let Some(name) = str_at(&b.feature, i).filter(|s| !s.is_empty()) else {
                skipped += 1;
                continue;
            };
            let (x, y) = (f64_at(&b.x, i), f64_at(&b.y, i));
            let Some((tx, ty)) = grid.tile_of(x, y) else {
                skipped += 1;
                continue;
            };
            let g = match gene_ids.get(name) {
                Some(&g) => g,
                None => {
                    let cat = FeatureCategory::classify(
                        name,
                        b.codeword_category.as_ref().and_then(|c| str_at(c, i)),
                        b.is_gene.as_ref().and_then(|c| bool_at(c, i)),
                    );
                    let g = genes.len() as u32;
                    genes.push(Pass1Gene {
                        name: name.to_string(),
                        category: cat,
                        count: 0,
                        count_qv20: 0,
                    });
                    gene_ids.insert(name.to_string(), g);
                    g
                }
            };
            let mut flags = 0u8;
            let cell_key = match b.cell_id.as_ref().and_then(|c| str_at(c, i)) {
                None => {
                    unassigned += 1;
                    0
                }
                Some(s) if is_unassigned_cell_id(s, cols.cell_id_integer) => {
                    unassigned += 1;
                    0
                }
                Some(s) => match cell_lookup.get(s) {
                    Some(&k) => k,
                    None => {
                        unknown += 1;
                        flags |= POINT_FLAG_UNKNOWN_CELL;
                        0
                    }
                },
            };
            if b.overlaps_nucleus
                .as_ref()
                .is_some_and(|c| u8_at(c, i) != 0)
            {
                flags |= POINT_FLAG_OVERLAPS_NUCLEUS;
            }
            let qv_q = match &b.qv {
                Some(q) => PointRecord::quantize_qv(f64_at(q, i) as f32),
                None => u8::MAX,
            };
            let rec = PointRecord {
                x: x as f32,
                y: y as f32,
                cell_key,
                z_centi: b
                    .z
                    .as_ref()
                    .map(|z| PointRecord::quantize_z(f64_at(z, i) as f32))
                    .unwrap_or(0),
                qv_q,
                flags,
            };
            let gi = &mut genes[g as usize];
            gi.count += 1;
            if qv_q >= qv20_q {
                gi.count_qv20 += 1;
            }
            let bb = bounds.get_or_insert([x, y, x, y]);
            bb[0] = bb[0].min(x);
            bb[1] = bb[1].min(y);
            bb[2] = bb[2].max(x);
            bb[3] = bb[3].max(y);
            let _ = tile_key(tx, ty);
            buf.clear();
            buf.extend_from_slice(&g.to_le_bytes());
            rec.write_to(&mut buf);
            let slot = &mut writers[g as usize % BUCKETS];
            if slot.is_none() {
                let f =
                    File::create(tmp_dir.join(format!("bucket_{:03}.bin", g as usize % BUCKETS)))?;
                *slot = Some(BufWriter::with_capacity(256 << 10, f));
            }
            slot.as_mut().unwrap().write_all(&buf)?;
        }
        if rows % 10_000_000 < b.len as u64 {
            tracing::info!("transcripts pass 1: {rows} rows ({:.0?})", t0.elapsed());
        }
        Ok(())
    })?;
    for w in writers.iter_mut().flatten() {
        w.flush()?;
    }
    drop(writers);
    if !cols.has_qv {
        warnings.push("transcripts.parquet has no qv column; all transcripts are treated as passing every QV filter".into());
    }
    if !cols.has_cell_id {
        warnings.push(
            "transcripts.parquet has no cell_id column; all transcripts are unassigned".into(),
        );
    }
    if unknown > 0 {
        warnings.push(format!(
            "{unknown} transcripts reference a cell_id that is not in cells.parquet; they are stored as unassigned with an 'unknown cell' flag"
        ));
    }
    if skipped > 0 {
        warnings.push(format!("{skipped} transcript rows skipped (missing feature name or non-finite / out-of-range coordinates)"));
    }

    // Final gene order: genes first, then controls; alphabetical within each.
    let mut order: Vec<u32> = (0..genes.len() as u32).collect();
    order.sort_by(|&a, &b| {
        let (ga, gb) = (&genes[a as usize], &genes[b as usize]);
        (!ga.category.is_gene(), &ga.name).cmp(&(!gb.category.is_gene(), &gb.name))
    });
    let mut final_of = vec![0u32; genes.len()];
    for (f, &t) in order.iter().enumerate() {
        final_of[t as usize] = f as u32;
    }

    // ---------------- pass 2 ----------------
    let mut points = BufWriter::with_capacity(1 << 20, File::create(out_dir.join("points.bin"))?);
    let mut dirs = BufWriter::with_capacity(1 << 20, File::create(out_dir.join("gene_dirs.bin"))?);
    let mut csc = BufWriter::with_capacity(1 << 20, File::create(out_dir.join("expr_csc.bin"))?);
    let mut csr_writers: Vec<BufWriter<File>> = (0..CSR_BUCKETS)
        .map(|i| {
            File::create(tmp_dir.join(format!("csr_{i:03}.bin")))
                .map(|f| BufWriter::with_capacity(256 << 10, f))
        })
        .collect::<std::io::Result<_>>()?;
    let mut entries = vec![GeneEntry::default(); genes.len()];
    let (mut rec_off, mut dir_off, mut csc_off) = (0u64, 0u64, 0u64);
    let csr_bucket = |cell_key: u32| {
        ((cell_key as usize - 1) * CSR_BUCKETS / n_cells.max(1)).min(CSR_BUCKETS - 1)
    };
    let mut out = Vec::new();
    for bucket in 0..BUCKETS {
        let p = tmp_dir.join(format!("bucket_{bucket:03}.bin"));
        if !p.exists() {
            continue;
        }
        let mut raw = vec![];
        BufReader::new(File::open(&p)?).read_to_end(&mut raw)?;
        std::fs::remove_file(&p)?;
        let mut recs: Vec<SortRec> = raw
            .chunks_exact(TMP_BYTES)
            .map(|c| {
                let tmp = u32::from_le_bytes(c[0..4].try_into().unwrap());
                let rec = PointRecord::from_bytes(&c[4..]);
                let (tx, ty) = grid
                    .tile_of(rec.x as f64, rec.y as f64)
                    .expect("validated in pass 1");
                SortRec {
                    gene: final_of[tmp as usize],
                    tile: tile_key(tx, ty),
                    rec,
                }
            })
            .collect();
        drop(raw);
        recs.sort_unstable_by(sort_key);
        let mut start = 0;
        while start < recs.len() {
            let gene = recs[start].gene;
            let end = start + recs[start..].partition_point(|r| r.gene == gene);
            let group = &recs[start..end];
            let e = &mut entries[gene as usize];
            e.points_offset = rec_off;
            e.count = group.len() as u64;
            e.dir_offset = dir_off;
            // tile directory
            let mut s = 0;
            while s < group.len() {
                let t = group[s].tile;
                let n = group[s..].partition_point(|r| r.tile == t);
                dirs.write_all(&t.to_le_bytes())?;
                dirs.write_all(&(s as u32).to_le_bytes())?;
                dirs.write_all(&(n as u32).to_le_bytes())?;
                e.dir_len += 1;
                s += n;
            }
            dir_off += e.dir_len as u64;
            out.clear();
            for r in group {
                r.rec.write_to(&mut out);
            }
            points.write_all(&out)?;
            rec_off += group.len() as u64;
            // per-gene expression counts (QV >= 20, assigned)
            let mut keys: Vec<u32> = group
                .iter()
                .filter(|r| r.rec.cell_key != 0 && r.rec.qv_q >= qv20_q)
                .map(|r| r.rec.cell_key)
                .collect();
            keys.sort_unstable();
            e.csc_offset = csc_off;
            let mut k = 0;
            while k < keys.len() {
                let key = keys[k];
                let n = keys[k..].partition_point(|&c| c == key) as u32;
                csc.write_all(&key.to_le_bytes())?;
                csc.write_all(&n.to_le_bytes())?;
                let w = &mut csr_writers[csr_bucket(key)];
                w.write_all(&key.to_le_bytes())?;
                w.write_all(&gene.to_le_bytes())?;
                w.write_all(&n.to_le_bytes())?;
                e.csc_len += 1;
                k += n as usize;
            }
            csc_off += e.csc_len as u64;
            start = end;
        }
        tracing::debug!(
            "transcripts pass 2: bucket {}/{BUCKETS} ({:.0?})",
            bucket + 1,
            t0.elapsed()
        );
    }
    points.flush()?;
    dirs.flush()?;
    csc.flush()?;
    for w in csr_writers.iter_mut() {
        w.flush()?;
    }
    drop(csr_writers);
    let mut table = Vec::with_capacity(8 + genes.len() * GENE_ENTRY_BYTES);
    table.extend_from_slice(b"XGN1");
    table.extend_from_slice(&(genes.len() as u32).to_le_bytes());
    for e in &entries {
        e.write(&mut table);
    }
    std::fs::write(out_dir.join("genes.bin"), table)?;

    // ---------------- pass 3: per-cell table ----------------
    let mut csr = BufWriter::with_capacity(1 << 20, File::create(out_dir.join("expr_csr.bin"))?);
    let mut cell_off = vec![0u64; n_cells + 1];
    let mut written = 0u64;
    let mut next_cell = 1usize;
    for i in 0..CSR_BUCKETS {
        let p = tmp_dir.join(format!("csr_{i:03}.bin"));
        let raw = std::fs::read(&p)?;
        std::fs::remove_file(&p)?;
        let mut trip: Vec<(u32, u32, u32)> = raw
            .chunks_exact(12)
            .map(|c| {
                (
                    u32::from_le_bytes(c[0..4].try_into().unwrap()),
                    u32::from_le_bytes(c[4..8].try_into().unwrap()),
                    u32::from_le_bytes(c[8..12].try_into().unwrap()),
                )
            })
            .collect();
        trip.sort_unstable();
        for (cell, gene, n) in trip {
            while next_cell <= cell as usize {
                cell_off[next_cell - 1] = written;
                next_cell += 1;
            }
            csr.write_all(&gene.to_le_bytes())?;
            csr.write_all(&n.to_le_bytes())?;
            written += 1;
        }
    }
    while next_cell <= n_cells + 1 {
        cell_off[next_cell - 1] = written;
        next_cell += 1;
    }
    csr.flush()?;
    let mut idx = Vec::with_capacity(8 + 8 * cell_off.len());
    idx.extend_from_slice(b"XCS1");
    idx.extend_from_slice(&(n_cells as u32).to_le_bytes());
    for o in &cell_off {
        idx.extend_from_slice(&o.to_le_bytes());
    }
    std::fs::write(out_dir.join("expr_csr.idx"), idx)?;
    let _ = std::fs::remove_dir(tmp_dir);

    let gene_infos: Vec<GeneInfo> = order
        .iter()
        .enumerate()
        .map(|(f, &t)| {
            let g = &genes[t as usize];
            GeneInfo {
                id: f as u32,
                name: g.name.clone(),
                category: g.category,
                count: g.count,
                count_qv20: g.count_qv20,
            }
        })
        .collect();
    let indexed: u64 = gene_infos.iter().map(|g| g.count).sum();
    tracing::info!(
        "transcripts: {indexed} indexed of {rows} rows, {} features ({:.0?})",
        genes.len(),
        t0.elapsed()
    );
    Ok(TranscriptBuild {
        info: TranscriptsInfo {
            rows_read: rows,
            indexed,
            skipped_invalid: skipped,
            unassigned,
            unknown_cell_ids: unknown,
            qv_scale: QV_SCALE as f64,
            default_qv_min: 20.0,
            z_available: cols.has_z,
            genes: gene_infos,
            density_max_level: 0,
            density_tile_bins: DENSITY_TILE_BINS,
        },
        bounds,
        warnings,
    })
}
