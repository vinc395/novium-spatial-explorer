//! Read-only access to one published section index. All reads are positional
//! (`pread`), so a `SectionStore` is shared across request threads without locks
//! (apart from a small LRU of per-gene tile directories).

use crate::pack::PackReader;
use crate::pyramid::{image_tile_key, TILE_PACK};
use crate::transcripts_ingest::{read_gene_table, GeneEntry, DENSITY_TILE_BINS, DIR_ENTRY_BYTES};
use anyhow::{anyhow, bail, Context, Result};
use lru::LruCache;
use rayon::prelude::*;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::File;
use std::num::NonZeroUsize;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use xenium_core::codec::{
    decode_centroids, encode_density, encode_field, encode_gene_tiles, encode_points, DensityTile,
    FieldColumn, PointRecord, Segment, POINT_RECORD_BYTES, XPT_FLAG_QV_FILTERED,
};
use xenium_core::grid::{tile_key, TileGrid};
use xenium_core::manifest::{FieldInfo, FieldKind, Layout, SectionManifest};
use xenium_core::CellDictionary;

/// Upper bound on records in one transcript tile response.
pub const MAX_POINTS_PER_RESPONSE: usize = 4_000_000;
pub const MAX_GENES_PER_REQUEST: usize = 64;
pub const MAX_EXPORT_CELLS: usize = 2_000_000;
/// Upper bound on `cells x (fields + 1)` values in one CSV export.
pub const MAX_EXPORT_VALUES: usize = 40_000_000;
/// Tile coordinates must fit the 16-bit biased key packing.
pub const MAX_TILE_INDEX: i32 = 32_767;

pub fn check_tile(tx: i32, ty: i32) -> Result<()> {
    if tx.unsigned_abs() > MAX_TILE_INDEX as u32 || ty.unsigned_abs() > MAX_TILE_INDEX as u32 {
        return Err(bad_request("tile index out of range"));
    }
    Ok(())
}

/// Request-level errors; the server maps them to 404 / 400 / 413.
#[derive(Debug)]
pub enum StoreError {
    NotFound(String),
    BadRequest(String),
    TooLarge(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound(s) => write!(f, "not found: {s}"),
            StoreError::BadRequest(s) => write!(f, "bad request: {s}"),
            StoreError::TooLarge(s) => write!(f, "too large: {s}"),
        }
    }
}

impl std::error::Error for StoreError {}

fn not_found(s: impl Into<String>) -> anyhow::Error {
    StoreError::NotFound(s.into()).into()
}

fn bad_request(s: impl Into<String>) -> anyhow::Error {
    StoreError::BadRequest(s.into()).into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    pub tile_key: u32,
    pub start: u32,
    pub count: u32,
}

/// A file plus its length; every positional read is bounds-checked before any
/// allocation, so a corrupt offset can never trigger a huge allocation.
struct Blob {
    file: File,
    len: u64,
}

impl Blob {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let len = file.metadata()?.len();
        Ok(Blob { file, len })
    }

    fn read(&self, off: u64, len: u64) -> Result<Vec<u8>> {
        let end = off
            .checked_add(len)
            .ok_or_else(|| anyhow!("read range overflow"))?;
        if end > self.len {
            bail!(
                "corrupt index: read {off}+{len} beyond file length {}",
                self.len
            );
        }
        let mut buf = vec![0u8; len as usize];
        self.file.read_exact_at(&mut buf, off)?;
        Ok(buf)
    }
}

struct Points {
    points: Blob,
    dirs: Blob,
    csc: Blob,
    csr: Blob,
    csr_idx: Vec<u64>,
    genes: Vec<GeneEntry>,
    dir_cache: Mutex<LruCache<u32, Arc<Vec<DirEntry>>>>,
}

pub struct SectionStore {
    pub dir: PathBuf,
    pub manifest: SectionManifest,
    pub manifest_json: Vec<u8>,
    dict: CellDictionary,
    centroids: Vec<f32>,
    cell_pack: Option<PackReader>,
    nucleus_pack: Option<PackReader>,
    /// Field columns keyed by position in `cells.fields` (packed layout).
    field_pack: Option<PackReader>,
    points: Option<Points>,
    /// Tile packs of packed images, opened on first use.
    image_packs: Mutex<HashMap<String, Arc<PackReader>>>,
}

fn read_exact_at(f: &File, len: usize, off: u64) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    f.read_exact_at(&mut buf, off)?;
    Ok(buf)
}

/// Structural checks of the transcript files that are cheap (O(genes + cells)) and
/// guarantee every later read stays inside its file.
fn check_points(
    p: &Points,
    t: &xenium_core::manifest::TranscriptsInfo,
    n_cells: usize,
) -> Result<()> {
    if p.points.len != t.indexed * POINT_RECORD_BYTES as u64 {
        bail!(
            "points.bin size {} does not match {} indexed records",
            p.points.len,
            t.indexed
        );
    }
    for (i, (g, info)) in p.genes.iter().zip(&t.genes).enumerate() {
        if g.count != info.count {
            bail!("gene {i}: count differs from manifest");
        }
    }
    // Per-gene ranges are laid out in external-sort bucket order, not gene order:
    // sorted by offset they must tile each file exactly (no gaps, no overlap).
    let tiles = |name: &str, ranges: Vec<(u64, u64)>, file_units: u64| -> Result<()> {
        let mut r = ranges;
        r.sort_unstable();
        let mut next = 0u64;
        for (off, len) in r {
            if off != next {
                bail!("{name}: per-gene ranges overlap or leave gaps");
            }
            next = off
                .checked_add(len)
                .ok_or_else(|| anyhow!("{name}: range overflow"))?;
        }
        if next != file_units {
            bail!("{name}: per-gene ranges do not cover the file");
        }
        Ok(())
    };
    let nonempty = |v: Vec<(u64, u64)>| v.into_iter().filter(|r| r.1 > 0).collect::<Vec<_>>();
    tiles(
        "points.bin",
        nonempty(p.genes.iter().map(|g| (g.points_offset, g.count)).collect()),
        t.indexed,
    )?;
    tiles(
        "gene_dirs.bin",
        nonempty(
            p.genes
                .iter()
                .map(|g| (g.dir_offset, g.dir_len as u64))
                .collect(),
        ),
        p.dirs.len / DIR_ENTRY_BYTES as u64,
    )?;
    tiles(
        "expr_csc.bin",
        nonempty(
            p.genes
                .iter()
                .map(|g| (g.csc_offset, g.csc_len as u64))
                .collect(),
        ),
        p.csc.len / 8,
    )?;
    if !p.dirs.len.is_multiple_of(DIR_ENTRY_BYTES as u64) || !p.csc.len.is_multiple_of(8) {
        bail!("transcript index files have partial records");
    }
    if p.csr_idx.len() != n_cells + 1
        || p.csr_idx[0] != 0
        || p.csr_idx.windows(2).any(|w| w[0] > w[1])
        || p.csr_idx[n_cells] * 8 != p.csr.len
    {
        bail!("expr_csr.idx is not a monotone offset table matching expr_csr.bin");
    }
    Ok(())
}

impl SectionStore {
    pub fn open(dir: &Path) -> Result<Self> {
        let manifest_json = std::fs::read(dir.join("manifest.json"))
            .with_context(|| format!("read {}", dir.join("manifest.json").display()))?;
        let manifest: SectionManifest =
            serde_json::from_slice(&manifest_json).context("parse manifest.json")?;
        manifest
            .validate()
            .map_err(|e| anyhow!("{}: {e}", dir.display()))?;
        let dict = CellDictionary::from_bytes(&std::fs::read(dir.join("cells/ids.bin"))?)
            .map_err(|e| anyhow!("cells/ids.bin: {e}"))?;
        let centroids = decode_centroids(&std::fs::read(dir.join("cells/centroids.bin"))?)?;
        if centroids.len() != 2 * dict.len() {
            bail!("centroid count does not match cell dictionary");
        }
        let cells = manifest.cells.as_ref();
        let cell_pack = match cells.and_then(|c| c.boundaries.as_ref()) {
            Some(_) => Some(PackReader::open(&dir.join("cells/cell_chunks"))?),
            None => None,
        };
        let nucleus_pack = match cells.and_then(|c| c.nuclei.as_ref()) {
            Some(_) => Some(PackReader::open(&dir.join("cells/nucleus_chunks"))?),
            None => None,
        };
        let field_pack = match cells.map(|c| c.field_layout) {
            Some(Layout::Packed) => Some(PackReader::open(&dir.join("cells/fields"))?),
            _ => None,
        };
        let points = match &manifest.transcripts {
            None => None,
            Some(t) => {
                let td = dir.join("transcripts");
                let genes = read_gene_table(&td.join("genes.bin"))?;
                if genes.len() != t.genes.len() {
                    bail!("gene table does not match manifest");
                }
                let idx = std::fs::read(td.join("expr_csr.idx"))?;
                if idx.len() < 8 || &idx[..4] != b"XCS1" || idx.len() != 8 + 8 * (dict.len() + 1) {
                    bail!("bad expr_csr.idx");
                }
                let p = Points {
                    points: Blob::open(&td.join("points.bin"))?,
                    dirs: Blob::open(&td.join("gene_dirs.bin"))?,
                    csc: Blob::open(&td.join("expr_csc.bin"))?,
                    csr: Blob::open(&td.join("expr_csr.bin"))?,
                    csr_idx: idx[8..]
                        .chunks_exact(8)
                        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                        .collect(),
                    genes,
                    dir_cache: Mutex::new(LruCache::new(NonZeroUsize::new(512).unwrap())),
                };
                check_points(&p, t, dict.len())
                    .with_context(|| format!("{}: transcript index", dir.display()))?;
                Some(p)
            }
        };
        Ok(SectionStore {
            dir: dir.to_path_buf(),
            manifest,
            manifest_json,
            dict,
            centroids,
            cell_pack,
            nucleus_pack,
            field_pack,
            points,
            image_packs: Mutex::new(HashMap::new()),
        })
    }

    pub fn n_cells(&self) -> usize {
        self.dict.len()
    }

    pub fn grid(&self) -> TileGrid {
        self.manifest.grid
    }

    pub fn cell_dictionary(&self) -> &CellDictionary {
        &self.dict
    }

    pub fn centroid(&self, key: u32) -> Option<[f32; 2]> {
        let i = (key as usize).checked_sub(1)?;
        Some([*self.centroids.get(2 * i)?, *self.centroids.get(2 * i + 1)?])
    }

    pub fn centroids_bytes(&self) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.dir.join("cells/centroids.bin"))?)
    }

    pub fn cell_ids_bytes(&self) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.dir.join("cells/ids.bin"))?)
    }

    /// Position of a field in `cells.fields` (its key in the field pack).
    fn field_key(&self, id: &str) -> Result<u32> {
        self.manifest
            .cells
            .as_ref()
            .and_then(|c| c.fields.iter().position(|f| f.id == id))
            .map(|i| i as u32)
            .ok_or_else(|| not_found(format!("field {id:?}")))
    }

    fn field_path(&self, id: &str) -> PathBuf {
        self.dir.join("cells/fields").join(format!("{id}.bin"))
    }

    pub fn field_bytes(&self, id: &str) -> Result<Vec<u8>> {
        let key = self.field_key(id)?;
        match &self.field_pack {
            Some(pack) => pack
                .get(key)?
                .ok_or_else(|| anyhow!("corrupt index: field {id:?} missing from the field pack")),
            None => Ok(std::fs::read(self.field_path(id))?),
        }
    }

    /// Number of entries in the field pack (packed layout).
    pub(crate) fn field_pack_len(&self) -> Option<usize> {
        self.field_pack.as_ref().map(|p| p.len())
    }

    pub fn polygon_chunk(&self, nucleus: bool, tx: i32, ty: i32) -> Result<Option<Vec<u8>>> {
        let pack = if nucleus {
            &self.nucleus_pack
        } else {
            &self.cell_pack
        };
        let pack = pack.as_ref().ok_or_else(|| not_found("polygon layer"))?;
        check_tile(tx, ty)?;
        pack.get(tile_key(tx, ty))
    }

    fn points(&self) -> Result<&Points> {
        self.points.as_ref().ok_or_else(|| not_found("transcripts"))
    }

    pub fn n_genes(&self) -> usize {
        self.points.as_ref().map(|p| p.genes.len()).unwrap_or(0)
    }

    pub fn gene_dir(&self, gene: u32) -> Result<Arc<Vec<DirEntry>>> {
        let p = self.points()?;
        let e = *p
            .genes
            .get(gene as usize)
            .ok_or_else(|| not_found(format!("gene {gene}")))?;
        if let Some(d) = p.dir_cache.lock().unwrap().get(&gene) {
            return Ok(d.clone());
        }
        let raw = p.dirs.read(
            e.dir_offset * DIR_ENTRY_BYTES as u64,
            e.dir_len as u64 * DIR_ENTRY_BYTES as u64,
        )?;
        let d: Arc<Vec<DirEntry>> = Arc::new(
            raw.chunks_exact(DIR_ENTRY_BYTES)
                .map(|c| DirEntry {
                    tile_key: u32::from_le_bytes(c[0..4].try_into().unwrap()),
                    start: u32::from_le_bytes(c[4..8].try_into().unwrap()),
                    count: u32::from_le_bytes(c[8..12].try_into().unwrap()),
                })
                .collect(),
        );
        p.dir_cache.lock().unwrap().put(gene, d.clone());
        Ok(d)
    }

    /// Read the whole point file sequentially in 16 MB blocks (and discard it), so a
    /// following pass with many small per-gene reads is served from the page cache
    /// instead of seeking on a slow disk. `on_block(bytes)` returns false to stop.
    pub fn warm_points(&self, mut on_block: impl FnMut(u64) -> bool) -> Result<u64> {
        let p = self.points()?;
        const BLOCK: u64 = 16 << 20;
        let mut buf = vec![0u8; BLOCK as usize];
        let mut off = 0;
        while off < p.points.len {
            let n = BLOCK.min(p.points.len - off) as usize;
            p.points.file.read_exact_at(&mut buf[..n], off)?;
            off += n as u64;
            if !on_block(n as u64) {
                break;
            }
        }
        Ok(p.points.len)
    }

    pub fn points_bytes(&self) -> u64 {
        self.points.as_ref().map(|p| p.points.len).unwrap_or(0)
    }

    /// Number of cells with at least one QV>=20 transcript of `gene` (its CSC length).
    pub fn gene_cells_detected(&self, gene: u32) -> Option<u32> {
        let p = self.points.as_ref()?;
        p.genes.get(gene as usize).map(|e| e.csc_len)
    }

    /// Per-cell expression for the `n` cells with keys `first_key..first_key + n`:
    /// raw `(gene u32, count u32)` records (sorted by gene within each cell) and each
    /// cell's record range as offsets relative to the block start (`n + 1` values).
    pub fn csr_block(&self, first_key: u32, n: usize) -> Result<(Vec<u64>, Vec<u8>)> {
        let p = self.points()?;
        let a = (first_key as usize)
            .checked_sub(1)
            .filter(|&a| a + n <= self.n_cells())
            .ok_or_else(|| bad_request("cell key range out of bounds"))?;
        let offs = &p.csr_idx[a..=a + n];
        let raw = p.csr.read(offs[0] * 8, (offs[n] - offs[0]) * 8)?;
        Ok((offs.iter().map(|o| o - offs[0]).collect(), raw))
    }

    /// Raw 16-byte point records of `gene` in tile rows `ty0..=ty1` (every tile
    /// column). Directory entries are sorted by tile key, which is row-major, so the
    /// rows' records are one contiguous range.
    pub fn gene_records_in_rows(&self, gene: u32, ty0: i32, ty1: i32) -> Result<Vec<u8>> {
        check_tile(0, ty0)?;
        check_tile(0, ty1)?;
        let dir = self.gene_dir(gene)?;
        let lo = dir.partition_point(|e| e.tile_key < tile_key(-MAX_TILE_INDEX - 1, ty0));
        let hi = dir.partition_point(|e| e.tile_key <= tile_key(MAX_TILE_INDEX, ty1));
        if lo >= hi {
            return Ok(vec![]);
        }
        let start = dir[lo].start as u64;
        let end = dir[hi - 1].start as u64 + dir[hi - 1].count as u64;
        if end < start {
            bail!("corrupt index: gene {gene} directory is not in record order");
        }
        self.read_records(gene, start, (end - start) as usize)
    }

    pub fn gene_tiles_bytes(&self, gene: u32) -> Result<Vec<u8>> {
        let d = self.gene_dir(gene)?;
        let entries: Vec<(u32, u32)> = d.iter().map(|e| (e.tile_key, e.count)).collect();
        Ok(encode_gene_tiles(gene, &entries))
    }

    fn read_records(&self, gene: u32, start: u64, count: usize) -> Result<Vec<u8>> {
        let p = self.points()?;
        let e = p.genes[gene as usize];
        if start + count as u64 > e.count {
            bail!("corrupt index: gene {gene} directory points past its records");
        }
        p.points.read(
            (e.points_offset + start) * POINT_RECORD_BYTES as u64,
            (count * POINT_RECORD_BYTES) as u64,
        )
    }

    fn check_genes(&self, genes: &[u32]) -> Result<Vec<u32>> {
        let mut g = genes.to_vec();
        g.sort_unstable();
        g.dedup();
        if g.len() > MAX_GENES_PER_REQUEST {
            return Err(StoreError::TooLarge(format!(
                "at most {MAX_GENES_PER_REQUEST} genes per request"
            ))
            .into());
        }
        if let Some(bad) = g.iter().find(|&&x| x as usize >= self.n_genes()) {
            return Err(bad_request(format!("unknown gene id {bad}")));
        }
        Ok(g)
    }

    /// XPT1 payload with the requested genes' points in tile `(tx, ty)`.
    pub fn points_tile(&self, tx: i32, ty: i32, genes: &[u32], qv_min_q: u8) -> Result<Vec<u8>> {
        let genes = self.check_genes(genes)?;
        check_tile(tx, ty)?;
        let key = tile_key(tx, ty);
        let mut segments = vec![];
        let mut bytes = vec![];
        let mut total = 0usize;
        for g in genes {
            let dir = self.gene_dir(g)?;
            let Ok(i) = dir.binary_search_by_key(&key, |e| e.tile_key) else {
                continue;
            };
            let e = dir[i];
            total += e.count as usize;
            if total > MAX_POINTS_PER_RESPONSE {
                return Err(StoreError::TooLarge(format!(
                    "tile has more than {MAX_POINTS_PER_RESPONSE} points for the selected genes"
                ))
                .into());
            }
            let raw = self.read_records(g, e.start as u64, e.count as usize)?;
            let before = bytes.len();
            if qv_min_q == 0 {
                bytes.extend_from_slice(&raw);
            } else {
                for r in raw.chunks_exact(POINT_RECORD_BYTES) {
                    if r[14] >= qv_min_q {
                        bytes.extend_from_slice(r);
                    }
                }
            }
            let n = ((bytes.len() - before) / POINT_RECORD_BYTES) as u32;
            if n > 0 {
                segments.push(Segment {
                    gene_id: g,
                    count: n,
                });
            }
        }
        let flags = if qv_min_q > 0 {
            XPT_FLAG_QV_FILTERED
        } else {
            0
        };
        Ok(encode_points(&segments, &bytes, flags)?)
    }

    /// XDT1 density tile: counts per `2^level` µm bin over a 256 x 256 bin tile.
    pub fn density_tile(
        &self,
        gene: u32,
        level: u32,
        dx: i32,
        dy: i32,
        qv_min_q: u8,
    ) -> Result<Vec<u8>> {
        let t = self
            .manifest
            .transcripts
            .as_ref()
            .ok_or_else(|| not_found("transcripts"))?;
        self.check_genes(&[gene])?;
        if level > t.density_max_level + 1 || level > 20 {
            return Err(bad_request(format!("density level {level} out of range")));
        }
        if dx.unsigned_abs() > MAX_TILE_INDEX as u32 || dy.unsigned_abs() > MAX_TILE_INDEX as u32 {
            return Err(bad_request("density tile index out of range"));
        }
        let bin = (1u64 << level) as f64;
        let bins = DENSITY_TILE_BINS as usize;
        let span = bins as f64 * bin;
        let (x0, y0) = (dx as f64 * span, dy as f64 * span);
        let mut counts = vec![0u32; bins * bins];
        let grid = self.grid();
        let eps = 1e-6 * span;
        if let Some((tx0, ty0, tx1, ty1)) =
            grid.tiles_in_rect([x0, y0, x0 + span - eps, y0 + span - eps])
        {
            let dir = self.gene_dir(gene)?;
            for ty in ty0..=ty1 {
                let lo = dir.partition_point(|e| e.tile_key < tile_key(tx0, ty));
                let hi = dir.partition_point(|e| e.tile_key <= tile_key(tx1, ty));
                if lo >= hi {
                    continue;
                }
                let start = dir[lo].start as u64;
                let end = dir[hi - 1].start as u64 + dir[hi - 1].count as u64;
                let raw = self.read_records(gene, start, (end - start) as usize)?;
                for r in raw.chunks_exact(POINT_RECORD_BYTES) {
                    let rec = PointRecord::from_bytes(r);
                    if rec.qv_q < qv_min_q {
                        continue;
                    }
                    let bx = ((rec.x as f64 - x0) / bin).floor();
                    let by = ((rec.y as f64 - y0) / bin).floor();
                    if bx >= 0.0 && by >= 0.0 && (bx as usize) < bins && (by as usize) < bins {
                        counts[by as usize * bins + bx as usize] += 1;
                    }
                }
            }
        }
        let total = counts
            .iter()
            .map(|&c| c as u64)
            .sum::<u64>()
            .min(u32::MAX as u64) as u32;
        let max_count = counts.iter().copied().max().unwrap_or(0);
        Ok(encode_density(&DensityTile {
            level: level as u16,
            width: bins as u32,
            height: bins as u32,
            max_count,
            total,
            counts,
        }))
    }

    /// Dense per-cell counts (QV >= 20, assigned transcripts) for one gene, as XFD1.
    pub fn expression_field(&self, gene: u32) -> Result<Vec<u8>> {
        self.check_genes(&[gene])?;
        let p = self.points()?;
        let e = p.genes[gene as usize];
        let raw = p.csc.read(e.csc_offset * 8, e.csc_len as u64 * 8)?;
        let mut v = vec![0f32; self.n_cells()];
        for c in raw.chunks_exact(8) {
            let key = u32::from_le_bytes(c[0..4].try_into().unwrap()) as usize;
            let n = u32::from_le_bytes(c[4..8].try_into().unwrap());
            if key >= 1 && key <= v.len() {
                v[key - 1] = n as f32;
            }
        }
        Ok(encode_field(&FieldColumn::Numeric(v)))
    }

    /// `(gene_id, count)` for one cell (QV >= 20).
    pub fn cell_expression(&self, key: u32) -> Result<Vec<(u32, u32)>> {
        let Some(p) = self.points.as_ref() else {
            return Ok(vec![]);
        };
        let i = (key as usize)
            .checked_sub(1)
            .filter(|&i| i < self.n_cells())
            .ok_or_else(|| not_found("cell key"))?;
        let (a, b) = (p.csr_idx[i], p.csr_idx[i + 1]);
        let raw = p.csr.read(a * 8, (b - a) * 8)?;
        Ok(raw
            .chunks_exact(8)
            .map(|c| {
                (
                    u32::from_le_bytes(c[0..4].try_into().unwrap()),
                    u32::from_le_bytes(c[4..8].try_into().unwrap()),
                )
            })
            .collect())
    }

    fn field_value(&self, id: &str, key: u32) -> Result<Value> {
        let info = self
            .manifest
            .field(id)
            .ok_or_else(|| not_found(format!("field {id:?}")))?;
        let off = 16 + 4 * (key as u64 - 1);
        let b = match &self.field_pack {
            Some(pack) => pack
                .get_range(self.field_key(id)?, off, 4)?
                .ok_or_else(|| {
                    anyhow!("corrupt index: field {id:?} missing from the field pack")
                })?,
            None => read_exact_at(&File::open(self.field_path(id))?, 4, off)?,
        };
        Ok(match info.kind {
            FieldKind::Numeric => {
                let v = f32::from_le_bytes(b.try_into().unwrap());
                if v.is_finite() {
                    json!(v)
                } else {
                    Value::Null
                }
            }
            FieldKind::Categorical => {
                let c = u32::from_le_bytes(b.try_into().unwrap()) as usize;
                match info
                    .categories
                    .as_ref()
                    .and_then(|cats| cats.get(c.wrapping_sub(1)))
                {
                    Some(cat) if c > 0 => json!(cat.name),
                    _ => Value::Null,
                }
            }
        })
    }

    pub fn key_by_id(&self, id: &str) -> Option<u32> {
        self.dict.key_of(id)
    }

    pub fn cell_details(&self, key: u32) -> Result<Value> {
        self.cell_details_where(key, |_| true)
    }

    /// Cell details with the values of the fields `keep` accepts. Each value is one
    /// read, so a viewer loads wide groups (hundreds of LR columns) only on demand.
    pub fn cell_details_where(&self, key: u32, keep: impl Fn(&FieldInfo) -> bool) -> Result<Value> {
        let id = self
            .dict
            .id_of(key)
            .ok_or_else(|| not_found(format!("cell key {key}")))?;
        let c = self.centroid(key).unwrap();
        let wanted: Vec<&FieldInfo> = self
            .manifest
            .cells
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .filter(|f| keep(f))
            .collect();
        // Many reads in flight let the disk reorder them (hundreds of LR columns on a
        // spinning USB disk: several times faster than one after another).
        let values = wanted
            .par_iter()
            .map(|f| self.field_value(&f.id, key))
            .collect::<Result<Vec<_>>>()?;
        let fields: Map<String, Value> = wanted.iter().map(|f| f.id.clone()).zip(values).collect();
        let mut expr = self.cell_expression(key)?;
        let genes = self.manifest.transcripts.as_ref().map(|t| &t.genes);
        let info = |g: u32| genes.and_then(|v| v.get(g as usize));
        let is_gene = |g: u32| info(g).is_some_and(|i| i.category.is_gene());
        let total: u64 = expr.iter().map(|e| e.1 as u64).sum();
        let total_genes: u64 = expr
            .iter()
            .filter(|e| is_gene(e.0))
            .map(|e| e.1 as u64)
            .sum();
        expr.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let top: Vec<Value> = expr
            .iter()
            .take(30)
            .map(|&(g, n)| json!({"gene_id": g, "gene": info(g).map(|i| i.name.as_str()), "count": n, "is_gene": is_gene(g)}))
            .collect();
        Ok(json!({
            "section_id": self.manifest.section_id,
            "cell_key": key,
            "cell_id": id,
            "centroid_um": [c[0], c[1]],
            "fields": fields,
            "expression": {
                "qv_min": crate::transcripts_ingest::EXPR_QV_MIN,
                "total_genes": total_genes,
                "total_all_features": total,
                "n_features": expr.len(),
                "top": top
            },
        }))
    }

    pub fn grid_raster(&self, grid_id: &str, field_id: &str) -> Result<Vec<u8>> {
        let g = self
            .manifest
            .grid(grid_id)
            .ok_or_else(|| not_found(format!("grid {grid_id:?}")))?;
        if !g.fields.iter().any(|f| f.id == field_id) {
            return Err(not_found(format!("grid field {field_id:?}")));
        }
        Ok(std::fs::read(
            self.dir
                .join("grids")
                .join(grid_id)
                .join(format!("{field_id}.bin")),
        )?)
    }

    /// One JPEG tile: from the image's tile pack, or its own file in indexes written
    /// before packed tiles.
    pub fn image_tile(&self, image_id: &str, level: u32, x: u32, y: u32) -> Result<Vec<u8>> {
        let im = self
            .manifest
            .image(image_id)
            .ok_or_else(|| not_found(format!("image {image_id:?}")))?;
        if level >= im.levels {
            return Err(not_found("image level"));
        }
        let ts = im.tile_size as u64 * (1u64 << level);
        if x as u64 * ts >= im.width as u64 || y as u64 * ts >= im.height as u64 {
            return Err(not_found("image tile"));
        }
        match im.tile_layout {
            Layout::Packed => {
                let key = image_tile_key(level, x, y).ok_or_else(|| not_found("image tile"))?;
                self.image_pack(image_id)?
                    .get(key)?
                    .ok_or_else(|| not_found("image tile"))
            }
            Layout::Files => {
                let p = self
                    .image_dir(image_id)
                    .join(level.to_string())
                    .join(format!("{x}_{y}.jpg"));
                std::fs::read(&p).map_err(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => not_found("image tile"),
                    _ => anyhow!(e).context(format!("read {}", p.display())),
                })
            }
        }
    }

    /// `images/<id>/` of an image listed in the manifest.
    pub(crate) fn image_dir(&self, image_id: &str) -> PathBuf {
        self.dir.join("images").join(image_id)
    }

    /// The tile pack of a packed image listed in the manifest.
    pub(crate) fn image_pack(&self, image_id: &str) -> Result<Arc<PackReader>> {
        if let Some(p) = self.image_packs.lock().unwrap().get(image_id) {
            return Ok(p.clone());
        }
        // Opened outside the lock: a slow disk must not hold up other images' tiles.
        let p = Arc::new(PackReader::open(&self.image_dir(image_id).join(TILE_PACK))?);
        Ok(self
            .image_packs
            .lock()
            .unwrap()
            .entry(image_id.to_string())
            .or_insert(p)
            .clone())
    }

    /// CSV with original IDs, centroids and the requested fields.
    pub fn export_csv(&self, keys: &[u32], field_ids: &[String]) -> Result<String> {
        let mut keys = keys.to_vec();
        keys.sort_unstable();
        keys.dedup();
        let mut seen = std::collections::HashSet::new();
        let field_ids: Vec<String> = field_ids
            .iter()
            .filter(|f| seen.insert(f.as_str()))
            .cloned()
            .collect();
        if keys.len() > MAX_EXPORT_CELLS
            || keys.len().saturating_mul(field_ids.len() + 1) > MAX_EXPORT_VALUES
        {
            return Err(StoreError::TooLarge(format!(
                "export limited to {MAX_EXPORT_CELLS} cells and {MAX_EXPORT_VALUES} values"
            ))
            .into());
        }
        for f in &field_ids {
            if self.manifest.field(f).is_none() {
                return Err(bad_request(format!("unknown field {f:?}")));
            }
        }
        let columns: Vec<(String, FieldKind, Vec<u8>)> = field_ids
            .iter()
            .map(|f| {
                Ok((
                    f.clone(),
                    self.manifest.field(f).unwrap().kind,
                    self.field_bytes(f)?,
                ))
            })
            .collect::<Result<_>>()?;
        let mut out = String::from("section_id,cell_id,x_um,y_um");
        for (f, _, _) in &columns {
            out.push(',');
            out.push_str(&csv_escape(f));
        }
        out.push('\n');
        for &k in &keys {
            let id = self
                .dict
                .id_of(k)
                .ok_or_else(|| bad_request(format!("unknown cell key {k}")))?;
            let c = self.centroid(k).unwrap();
            out.push_str(&format!(
                "{},{},{},{}",
                csv_escape(&self.manifest.section_id),
                csv_escape(id),
                c[0],
                c[1]
            ));
            for (f, kind, bytes) in &columns {
                let o = 16 + 4 * (k as usize - 1);
                let raw: [u8; 4] = bytes[o..o + 4].try_into().unwrap();
                out.push(',');
                match kind {
                    FieldKind::Numeric => {
                        let v = f32::from_le_bytes(raw);
                        if v.is_finite() {
                            out.push_str(&v.to_string());
                        }
                    }
                    FieldKind::Categorical => {
                        let code = u32::from_le_bytes(raw) as usize;
                        let cats = self.manifest.field(f).and_then(|i| i.categories.as_ref());
                        if let Some(cat) = cats.and_then(|c| c.get(code.wrapping_sub(1))) {
                            if code > 0 {
                                out.push_str(&csv_escape(&cat.name));
                            }
                        }
                    }
                }
            }
            out.push('\n');
        }
        Ok(out)
    }
}

fn csv_escape(s: &str) -> String {
    // Also neutralise spreadsheet formula injection.
    let s = if s.starts_with(['=', '+', '-', '@']) && s.parse::<f64>().is_err() {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}
