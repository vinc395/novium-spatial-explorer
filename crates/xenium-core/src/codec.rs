//! Versioned little-endian binary formats streamed to the browser.
//!
//! All headers are encoded field-by-field (no transmuted structs) and every
//! payload section starts on a 4-byte boundary so the browser can create typed
//! array views without copying. Decoders validate exact lengths.
//!
//! | magic | content |
//! |-------|---------|
//! | `XPT1` | transcript points for one tile, grouped by gene |
//! | `XCC1` | cell or nucleus polygons for one chunk |
//! | `XCE1` | all cell centroids in key order |
//! | `XFD1` | one per-cell field column in key order |
//! | `XDT1` | one transcript density tile |
//! | `XGT1` | per-tile transcript counts for one gene |
//! | `XGR1` | one raster field of a segmentation-free grid |

use thiserror::Error;

pub const VERSION: u16 = 1;
/// Hard cap on records in one response, enforced by encoders and decoders.
pub const MAX_RECORDS: usize = 50_000_000;

#[derive(Debug, Error, PartialEq)]
pub enum CodecError {
    #[error("bad magic: expected {expected}, got {got:?}")]
    Magic {
        expected: &'static str,
        got: Vec<u8>,
    },
    #[error("unsupported version {0}")]
    Version(u16),
    #[error("length mismatch: expected {expected} bytes, got {got}")]
    Length { expected: usize, got: usize },
    #[error("buffer too short for header")]
    Truncated,
    #[error("invalid content: {0}")]
    Invalid(&'static str),
}

struct W(Vec<u8>);

impl W {
    fn with_capacity(n: usize) -> Self {
        W(Vec::with_capacity(n))
    }
    fn magic(&mut self, m: &[u8; 4]) {
        self.0.extend_from_slice(m);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

struct R<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> R<'a> {
    fn new(b: &'a [u8]) -> Self {
        R { b, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.pos.checked_add(n).ok_or(CodecError::Truncated)?;
        if end > self.b.len() {
            return Err(CodecError::Truncated);
        }
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn magic(&mut self, m: &'static [u8; 4], name: &'static str) -> Result<(), CodecError> {
        let got = self.take(4)?;
        if got != m {
            return Err(CodecError::Magic {
                expected: name,
                got: got.to_vec(),
            });
        }
        Ok(())
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn version(&mut self) -> Result<(), CodecError> {
        let v = self.u16()?;
        if v != VERSION {
            return Err(CodecError::Version(v));
        }
        Ok(())
    }
    fn expect_len(&self, total: usize) -> Result<(), CodecError> {
        if self.b.len() != total {
            return Err(CodecError::Length {
                expected: total,
                got: self.b.len(),
            });
        }
        Ok(())
    }
}

fn f32s(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn u32s(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

// ---------------------------------------------------------------------------
// Transcript point record (16 bytes, also the on-disk record layout)
// ---------------------------------------------------------------------------

pub const POINT_RECORD_BYTES: usize = 16;
/// QV is stored as `floor(qv * 4)`, so `qv >= t` is exact for `t` a multiple of 0.25.
pub const QV_SCALE: f32 = 4.0;
/// z is stored in units of 0.01 µm.
pub const Z_SCALE: f32 = 100.0;
pub const POINT_FLAG_OVERLAPS_NUCLEUS: u8 = 1;
/// `cell_id` was present but not found in cells.parquet (key stored as 0).
pub const POINT_FLAG_UNKNOWN_CELL: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointRecord {
    pub x: f32,
    pub y: f32,
    pub cell_key: u32,
    pub z_centi: i16,
    pub qv_q: u8,
    pub flags: u8,
}

impl PointRecord {
    pub fn quantize_qv(qv: f32) -> u8 {
        if !qv.is_finite() || qv <= 0.0 {
            return 0;
        }
        (qv * QV_SCALE).floor().min(255.0) as u8
    }

    pub fn quantize_z(z: f32) -> i16 {
        if !z.is_finite() {
            return 0;
        }
        (z * Z_SCALE)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16
    }

    pub fn qv(&self) -> f32 {
        self.qv_q as f32 / QV_SCALE
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.x.to_le_bytes());
        out.extend_from_slice(&self.y.to_le_bytes());
        out.extend_from_slice(&self.cell_key.to_le_bytes());
        out.extend_from_slice(&self.z_centi.to_le_bytes());
        out.push(self.qv_q);
        out.push(self.flags);
    }

    pub fn to_bytes(&self) -> [u8; POINT_RECORD_BYTES] {
        let mut v = Vec::with_capacity(POINT_RECORD_BYTES);
        self.write_to(&mut v);
        v.try_into().unwrap()
    }

    pub fn from_bytes(b: &[u8]) -> Self {
        PointRecord {
            x: f32::from_le_bytes(b[0..4].try_into().unwrap()),
            y: f32::from_le_bytes(b[4..8].try_into().unwrap()),
            cell_key: u32::from_le_bytes(b[8..12].try_into().unwrap()),
            z_centi: i16::from_le_bytes(b[12..14].try_into().unwrap()),
            qv_q: b[14],
            flags: b[15],
        }
    }
}

/// A gene segment inside an `XPT1` payload: `count` records of `gene_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub gene_id: u32,
    pub count: u32,
}

pub const XPT_FLAG_QV_FILTERED: u16 = 1;

/// Encode an `XPT1` payload from pre-encoded record bytes (`16 * sum(count)` bytes).
pub fn encode_points(
    segments: &[Segment],
    record_bytes: &[u8],
    flags: u16,
) -> Result<Vec<u8>, CodecError> {
    let n: usize = segments.iter().map(|s| s.count as usize).sum();
    if n * POINT_RECORD_BYTES != record_bytes.len() {
        return Err(CodecError::Length {
            expected: n * POINT_RECORD_BYTES,
            got: record_bytes.len(),
        });
    }
    if n > MAX_RECORDS {
        return Err(CodecError::Invalid("too many records"));
    }
    let mut w = W::with_capacity(16 + 8 * segments.len() + record_bytes.len());
    w.magic(b"XPT1");
    w.u16(VERSION);
    w.u16(flags);
    w.u32(n as u32);
    w.u32(segments.len() as u32);
    for s in segments {
        w.u32(s.gene_id);
        w.u32(s.count);
    }
    w.0.extend_from_slice(record_bytes);
    Ok(w.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedPoints {
    pub flags: u16,
    pub segments: Vec<Segment>,
    pub records: Vec<PointRecord>,
}

pub fn decode_points(b: &[u8]) -> Result<DecodedPoints, CodecError> {
    let mut r = R::new(b);
    r.magic(b"XPT1", "XPT1")?;
    r.version()?;
    let flags = r.u16()?;
    let n = r.u32()? as usize;
    let s = r.u32()? as usize;
    if n > MAX_RECORDS || s > MAX_RECORDS {
        return Err(CodecError::Invalid("count exceeds limit"));
    }
    r.expect_len(16 + 8 * s + POINT_RECORD_BYTES * n)?;
    let mut segments = Vec::with_capacity(s);
    let mut total = 0usize;
    for _ in 0..s {
        let seg = Segment {
            gene_id: r.u32()?,
            count: r.u32()?,
        };
        total += seg.count as usize;
        segments.push(seg);
    }
    if total != n {
        return Err(CodecError::Invalid(
            "segment counts do not sum to record_count",
        ));
    }
    let records = r
        .take(POINT_RECORD_BYTES * n)?
        .chunks_exact(POINT_RECORD_BYTES)
        .map(PointRecord::from_bytes)
        .collect();
    Ok(DecodedPoints {
        flags,
        segments,
        records,
    })
}

// ---------------------------------------------------------------------------
// Polygon chunk (cells / nuclei)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ShapeKind {
    Cell = 0,
    Nucleus = 1,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PolygonChunk {
    pub cell_keys: Vec<u32>,
    /// `n_polygons + 1` vertex offsets; the last equals the vertex count.
    pub starts: Vec<u32>,
    /// Interleaved x, y in µm; rings are open (no repeated closing vertex).
    pub xy: Vec<f32>,
}

impl PolygonChunk {
    pub fn new() -> Self {
        PolygonChunk {
            cell_keys: vec![],
            starts: vec![0],
            xy: vec![],
        }
    }

    pub fn push(&mut self, key: u32, ring: &[[f32; 2]]) {
        self.cell_keys.push(key);
        for p in ring {
            self.xy.extend_from_slice(p);
        }
        self.starts.push((self.xy.len() / 2) as u32);
    }

    pub fn len(&self) -> usize {
        self.cell_keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cell_keys.is_empty()
    }
}

pub fn encode_polygons(kind: ShapeKind, c: &PolygonChunk) -> Vec<u8> {
    let n = c.cell_keys.len();
    let v = c.xy.len() / 2;
    let mut w = W::with_capacity(16 + 4 * n + 4 * (n + 1) + 8 * v);
    w.magic(b"XCC1");
    w.u16(VERSION);
    w.u16(kind as u16);
    w.u32(n as u32);
    w.u32(v as u32);
    for &k in &c.cell_keys {
        w.u32(k);
    }
    for &s in &c.starts {
        w.u32(s);
    }
    for &f in &c.xy {
        w.f32(f);
    }
    w.0
}

pub fn decode_polygons(b: &[u8]) -> Result<(u16, PolygonChunk), CodecError> {
    let mut r = R::new(b);
    r.magic(b"XCC1", "XCC1")?;
    r.version()?;
    let kind = r.u16()?;
    let n = r.u32()? as usize;
    let v = r.u32()? as usize;
    if n > MAX_RECORDS || v > MAX_RECORDS {
        return Err(CodecError::Invalid("count exceeds limit"));
    }
    r.expect_len(16 + 4 * n + 4 * (n + 1) + 8 * v)?;
    let cell_keys = u32s(r.take(4 * n)?);
    let starts = u32s(r.take(4 * (n + 1))?);
    let xy = f32s(r.take(8 * v)?);
    if starts[0] != 0 || starts[n] as usize != v || starts.windows(2).any(|w| w[0] > w[1]) {
        return Err(CodecError::Invalid("polygon start indices"));
    }
    Ok((
        kind,
        PolygonChunk {
            cell_keys,
            starts,
            xy,
        },
    ))
}

// ---------------------------------------------------------------------------
// Centroids, field columns, density tiles, gene tile counts, grid rasters
// ---------------------------------------------------------------------------

pub fn encode_centroids(xy: &[f32]) -> Vec<u8> {
    let n = xy.len() / 2;
    let mut w = W::with_capacity(16 + 4 * xy.len());
    w.magic(b"XCE1");
    w.u16(VERSION);
    w.u16(0);
    w.u32(n as u32);
    w.u32(0);
    for &f in xy {
        w.f32(f);
    }
    w.0
}

pub fn decode_centroids(b: &[u8]) -> Result<Vec<f32>, CodecError> {
    let mut r = R::new(b);
    r.magic(b"XCE1", "XCE1")?;
    r.version()?;
    r.u16()?;
    let n = r.u32()? as usize;
    r.u32()?;
    r.expect_len(16 + 8 * n)?;
    Ok(f32s(r.take(8 * n)?))
}

/// A per-cell field column indexed by `key - 1`.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldColumn {
    /// NaN = missing.
    Numeric(Vec<f32>),
    /// 0 = missing, `c` = `categories[c - 1]`.
    Categorical { codes: Vec<u32>, n_categories: u32 },
}

pub fn encode_field(col: &FieldColumn) -> Vec<u8> {
    let (kind, n, ncat) = match col {
        FieldColumn::Numeric(v) => (0u16, v.len(), 0u32),
        FieldColumn::Categorical {
            codes,
            n_categories,
        } => (1u16, codes.len(), *n_categories),
    };
    let mut w = W::with_capacity(16 + 4 * n);
    w.magic(b"XFD1");
    w.u16(VERSION);
    w.u16(kind);
    w.u32(n as u32);
    w.u32(ncat);
    match col {
        FieldColumn::Numeric(v) => v.iter().for_each(|&f| w.f32(f)),
        FieldColumn::Categorical { codes, .. } => codes.iter().for_each(|&c| w.u32(c)),
    }
    w.0
}

pub fn decode_field(b: &[u8]) -> Result<FieldColumn, CodecError> {
    let mut r = R::new(b);
    r.magic(b"XFD1", "XFD1")?;
    r.version()?;
    let kind = r.u16()?;
    let n = r.u32()? as usize;
    let ncat = r.u32()?;
    r.expect_len(16 + 4 * n)?;
    let body = r.take(4 * n)?;
    match kind {
        0 => Ok(FieldColumn::Numeric(f32s(body))),
        1 => {
            let codes = u32s(body);
            if codes.iter().any(|&c| c > ncat) {
                return Err(CodecError::Invalid("category code out of range"));
            }
            Ok(FieldColumn::Categorical {
                codes,
                n_categories: ncat,
            })
        }
        _ => Err(CodecError::Invalid("unknown field kind")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DensityTile {
    pub level: u16,
    pub width: u32,
    pub height: u32,
    pub max_count: u32,
    pub total: u32,
    pub counts: Vec<u32>,
}

pub fn encode_density(t: &DensityTile) -> Vec<u8> {
    let mut w = W::with_capacity(24 + 4 * t.counts.len());
    w.magic(b"XDT1");
    w.u16(VERSION);
    w.u16(t.level);
    w.u32(t.width);
    w.u32(t.height);
    w.u32(t.max_count);
    w.u32(t.total);
    t.counts.iter().for_each(|&c| w.u32(c));
    w.0
}

pub fn decode_density(b: &[u8]) -> Result<DensityTile, CodecError> {
    let mut r = R::new(b);
    r.magic(b"XDT1", "XDT1")?;
    r.version()?;
    let level = r.u16()?;
    let width = r.u32()?;
    let height = r.u32()?;
    let max_count = r.u32()?;
    let total = r.u32()?;
    let n = (width as usize)
        .checked_mul(height as usize)
        .filter(|&n| n <= MAX_RECORDS)
        .ok_or(CodecError::Invalid("density size"))?;
    r.expect_len(24 + 4 * n)?;
    Ok(DensityTile {
        level,
        width,
        height,
        max_count,
        total,
        counts: u32s(r.take(4 * n)?),
    })
}

/// `(tile_key, count)` pairs for one gene.
pub fn encode_gene_tiles(gene_id: u32, entries: &[(u32, u32)]) -> Vec<u8> {
    let mut w = W::with_capacity(16 + 8 * entries.len());
    w.magic(b"XGT1");
    w.u16(VERSION);
    w.u16(0);
    w.u32(entries.len() as u32);
    w.u32(gene_id);
    for &(k, c) in entries {
        w.u32(k);
        w.u32(c);
    }
    w.0
}

pub fn decode_gene_tiles(b: &[u8]) -> Result<(u32, Vec<(u32, u32)>), CodecError> {
    let mut r = R::new(b);
    r.magic(b"XGT1", "XGT1")?;
    r.version()?;
    r.u16()?;
    let n = r.u32()? as usize;
    let gene = r.u32()?;
    r.expect_len(16 + 8 * n)?;
    let v = u32s(r.take(8 * n)?);
    Ok((gene, v.chunks_exact(2).map(|c| (c[0], c[1])).collect()))
}

/// Raster field of a grid layer: numeric f32 (NaN = missing) or categorical u32 codes.
pub fn encode_grid_raster(
    width: u32,
    height: u32,
    col: &FieldColumn,
) -> Result<Vec<u8>, CodecError> {
    let n = (width as usize) * (height as usize);
    let (kind, len, ncat) = match col {
        FieldColumn::Numeric(v) => (0u16, v.len(), 0),
        FieldColumn::Categorical {
            codes,
            n_categories,
        } => (1u16, codes.len(), *n_categories),
    };
    if len != n {
        return Err(CodecError::Length {
            expected: n,
            got: len,
        });
    }
    let mut w = W::with_capacity(20 + 4 * n);
    w.magic(b"XGR1");
    w.u16(VERSION);
    w.u16(kind);
    w.u32(width);
    w.u32(height);
    w.u32(ncat);
    match col {
        FieldColumn::Numeric(v) => v.iter().for_each(|&f| w.f32(f)),
        FieldColumn::Categorical { codes, .. } => codes.iter().for_each(|&c| w.u32(c)),
    }
    Ok(w.0)
}

pub fn decode_grid_raster(b: &[u8]) -> Result<(u32, u32, FieldColumn), CodecError> {
    let mut r = R::new(b);
    r.magic(b"XGR1", "XGR1")?;
    r.version()?;
    let kind = r.u16()?;
    let width = r.u32()?;
    let height = r.u32()?;
    let ncat = r.u32()?;
    let n = (width as usize)
        .checked_mul(height as usize)
        .filter(|&n| n <= MAX_RECORDS)
        .ok_or(CodecError::Invalid("grid size"))?;
    r.expect_len(20 + 4 * n)?;
    let body = r.take(4 * n)?;
    let col = match kind {
        0 => FieldColumn::Numeric(f32s(body)),
        1 => FieldColumn::Categorical {
            codes: u32s(body),
            n_categories: ncat,
        },
        _ => return Err(CodecError::Invalid("unknown grid kind")),
    };
    Ok((width, height, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(x: f32, y: f32, k: u32, qv: f32) -> PointRecord {
        PointRecord {
            x,
            y,
            cell_key: k,
            z_centi: PointRecord::quantize_z(12.345),
            qv_q: PointRecord::quantize_qv(qv),
            flags: POINT_FLAG_OVERLAPS_NUCLEUS,
        }
    }

    #[test]
    fn point_record_golden_bytes() {
        let r = PointRecord {
            x: 1.0,
            y: -2.0,
            cell_key: 0x01020304,
            z_centi: -2,
            qv_q: 80,
            flags: 3,
        };
        assert_eq!(
            r.to_bytes(),
            [0, 0, 0x80, 0x3f, 0, 0, 0, 0xc0, 4, 3, 2, 1, 0xfe, 0xff, 80, 3]
        );
        assert_eq!(PointRecord::from_bytes(&r.to_bytes()), r);
    }

    #[test]
    fn qv_quantization_preserves_threshold_comparisons() {
        for &(qv, t) in &[
            (19.99f32, 20.0f32),
            (20.0, 20.0),
            (20.24, 20.25),
            (40.0, 20.0),
            (0.0, 0.25),
        ] {
            let q = PointRecord::quantize_qv(qv);
            assert_eq!(qv >= t, q as f32 >= t * QV_SCALE, "qv={qv} t={t}");
        }
        assert_eq!(PointRecord::quantize_qv(f32::NAN), 0);
        assert_eq!(PointRecord::quantize_qv(1000.0), 255);
        assert_eq!(PointRecord::quantize_z(f32::INFINITY), 0);
    }

    #[test]
    fn points_round_trip_empty_and_corrupt() {
        let recs = [
            rec(1.5, 2.5, 7, 30.0),
            rec(100.25, 0.0, 0, 10.0),
            rec(3.0, 4.0, 9, 20.0),
        ];
        let mut bytes = vec![];
        recs.iter().for_each(|r| r.write_to(&mut bytes));
        let segs = [
            Segment {
                gene_id: 4,
                count: 2,
            },
            Segment {
                gene_id: 9,
                count: 1,
            },
        ];
        let enc = encode_points(&segs, &bytes, 0).unwrap();
        assert_eq!(enc.len(), 16 + 16 + 48);
        let dec = decode_points(&enc).unwrap();
        assert_eq!(dec.segments, segs);
        assert_eq!(dec.records, recs);

        let empty = encode_points(&[], &[], 0).unwrap();
        assert_eq!(decode_points(&empty).unwrap().records.len(), 0);

        assert!(matches!(
            decode_points(&enc[..enc.len() - 1]),
            Err(CodecError::Length { .. })
        ));
        let mut bad = enc.clone();
        bad[4] = 2; // version
        assert_eq!(decode_points(&bad), Err(CodecError::Version(2)));
        let mut bad = enc.clone();
        bad[16 + 4] = 3; // first segment count 2 -> 3
        assert!(decode_points(&bad).is_err());
        assert!(encode_points(&segs, &bytes[..16], 0).is_err());
        assert!(decode_points(b"XPT").is_err());
    }

    #[test]
    fn polygons_round_trip() {
        let mut c = PolygonChunk::new();
        c.push(5, &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]);
        c.push(5, &[[2.0, 2.0], [3.0, 2.0], [3.0, 3.0], [2.0, 3.0]]);
        let enc = encode_polygons(ShapeKind::Nucleus, &c);
        let (kind, dec) = decode_polygons(&enc).unwrap();
        assert_eq!(kind, 1);
        assert_eq!(dec, c);
        assert_eq!(dec.starts, vec![0, 3, 7]);
        let mut bad = enc.clone();
        let off = 16 + 8; // starts[0]
        bad[off] = 1;
        assert!(decode_polygons(&bad).is_err());
        assert_eq!(
            decode_polygons(&encode_polygons(ShapeKind::Cell, &PolygonChunk::new()))
                .unwrap()
                .1
                .len(),
            0
        );
    }

    #[test]
    fn fields_density_tiles_grids_round_trip() {
        let num = FieldColumn::Numeric(vec![1.0, f32::NAN, -3.5]);
        match decode_field(&encode_field(&num)).unwrap() {
            FieldColumn::Numeric(v) => {
                assert_eq!(v[0], 1.0);
                assert!(v[1].is_nan());
                assert_eq!(v[2], -3.5);
            }
            _ => panic!(),
        }
        let cat = FieldColumn::Categorical {
            codes: vec![0, 1, 70],
            n_categories: 70,
        };
        assert_eq!(decode_field(&encode_field(&cat)).unwrap(), cat);
        let bad = FieldColumn::Categorical {
            codes: vec![71],
            n_categories: 70,
        };
        assert!(decode_field(&encode_field(&bad)).is_err());

        let d = DensityTile {
            level: 3,
            width: 2,
            height: 2,
            max_count: 9,
            total: 12,
            counts: vec![1, 2, 0, 9],
        };
        assert_eq!(decode_density(&encode_density(&d)).unwrap(), d);

        let gt = vec![
            (crate::grid::tile_key(1, 2), 10),
            (crate::grid::tile_key(3, 2), 1),
        ];
        assert_eq!(
            decode_gene_tiles(&encode_gene_tiles(7, &gt)).unwrap(),
            (7, gt)
        );

        let g = FieldColumn::Categorical {
            codes: vec![0, 1, 2, 2, 1, 0],
            n_categories: 2,
        };
        let enc = encode_grid_raster(3, 2, &g).unwrap();
        assert_eq!(decode_grid_raster(&enc).unwrap(), (3, 2, g.clone()));
        assert!(encode_grid_raster(4, 2, &g).is_err());

        assert_eq!(
            decode_centroids(&encode_centroids(&[1.0, 2.0, 3.0, 4.0])).unwrap(),
            vec![1.0, 2.0, 3.0, 4.0]
        );
    }
}
