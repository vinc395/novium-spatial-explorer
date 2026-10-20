//! Streaming image pyramid builder: consumes level-0 row bands and writes JPEG
//! tiles for every level (2x box downsampling), all into one pack per image
//! (`<out>/tiles.{pack,idx}`, keys from [`image_tile_key`]) or, in the older layout,
//! one file per tile (`<out>/<level>/<tx>_<ty>.jpg`). Memory is O(width * tile_size)
//! per level, independent of image height.

use crate::pack::PackWriter;
use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use xenium_core::manifest::Layout;
use xenium_io::tiff_image::BandSource;

/// Base name of an image's tile pack inside `images/<id>/`.
pub const TILE_PACK: &str = "tiles";

/// Pack key of tile `(x, y)` at `level`: `level << 26 | y << 13 | x` (None past
/// 64 levels or 8192 tiles per axis).
pub fn image_tile_key(level: u32, x: u32, y: u32) -> Option<u32> {
    (level < 64 && x < 8192 && y < 8192).then_some(level << 26 | y << 13 | x)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyramidInfo {
    pub width: u32,
    pub height: u32,
    pub levels: u32,
    pub tile_size: u32,
    pub channels: u32,
    pub tiles: u64,
}

pub fn level_count(width: u32, height: u32, tile: u32) -> u32 {
    let mut l = 0;
    let (mut w, mut h) = (width, height);
    while w > tile || h > tile {
        w = w.div_ceil(2);
        h = h.div_ceil(2);
        l += 1;
    }
    l + 1
}

struct Level {
    width: usize,
    acc: Vec<u8>,
    rows: u32,
    tile_row: u32,
    pending: Option<Vec<u8>>,
}

enum Sink {
    Pack(PackWriter),
    /// The image directory; tiles go to `<level>/<tx>_<ty>.jpg` below it.
    Files(PathBuf),
}

struct Builder {
    levels: Vec<Level>,
    tile: u32,
    ch: usize,
    quality: u8,
    tiles: u64,
    sink: Sink,
}

impl Builder {
    fn push_row(&mut self, l: usize, row: &[u8]) -> Result<()> {
        let lv = &mut self.levels[l];
        lv.acc.extend_from_slice(row);
        lv.rows += 1;
        if lv.rows == self.tile {
            self.emit(l)?;
        }
        if l + 1 < self.levels.len() {
            let lv = &mut self.levels[l];
            match lv.pending.take() {
                None => lv.pending = Some(row.to_vec()),
                Some(prev) => {
                    let down = downsample(&prev, Some(row), lv.width, self.ch);
                    self.push_row(l + 1, &down)?;
                }
            }
        }
        Ok(())
    }

    fn emit(&mut self, l: usize) -> Result<()> {
        let (tile, ch, quality) = (self.tile as usize, self.ch, self.quality);
        let lv = &mut self.levels[l];
        if lv.rows == 0 {
            return Ok(());
        }
        let (w, rows, ty) = (lv.width, lv.rows as usize, lv.tile_row);
        let across = w.div_ceil(tile);
        let acc = &lv.acc;
        let jpgs = (0..across)
            .into_par_iter()
            .map(|tx| -> Result<Vec<u8>> {
                let x0 = tx * tile;
                let cw = tile.min(w - x0);
                let mut px = Vec::with_capacity(cw * rows * ch);
                for r in 0..rows {
                    px.extend_from_slice(&acc[(r * w + x0) * ch..(r * w + x0 + cw) * ch]);
                }
                let mut jpg = Vec::with_capacity(px.len() / 8);
                let enc = jpeg_encoder::Encoder::new(&mut jpg, quality);
                let ct = if ch == 3 {
                    jpeg_encoder::ColorType::Rgb
                } else {
                    jpeg_encoder::ColorType::Luma
                };
                enc.encode(&px, cw as u16, rows as u16, ct)?;
                Ok(jpg)
            })
            .collect::<Result<Vec<_>>>()?;
        for (tx, jpg) in jpgs.iter().enumerate() {
            match &mut self.sink {
                Sink::Pack(pack) => {
                    let key = image_tile_key(l as u32, tx as u32, ty).ok_or_else(|| {
                        anyhow!("image too large for a tile pack ({across} tiles across)")
                    })?;
                    pack.insert(key, jpg)?;
                }
                Sink::Files(dir) => {
                    let p = dir.join(l.to_string()).join(format!("{tx}_{ty}.jpg"));
                    std::fs::write(&p, jpg).with_context(|| format!("write {}", p.display()))?;
                }
            }
        }
        self.tiles += across as u64;
        lv.acc.clear();
        lv.rows = 0;
        lv.tile_row += 1;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        for l in 0..self.levels.len() {
            if l + 1 < self.levels.len() {
                if let Some(prev) = self.levels[l].pending.take() {
                    let down = downsample(&prev, None, self.levels[l].width, self.ch);
                    self.push_row(l + 1, &down)?;
                }
            }
            self.emit(l)?;
        }
        Ok(())
    }
}

/// 2x2 box filter of one or two source rows into one row of half width.
fn downsample(a: &[u8], b: Option<&[u8]>, w: usize, ch: usize) -> Vec<u8> {
    let ow = w.div_ceil(2);
    let mut out = vec![0u8; ow * ch];
    for x in 0..ow {
        let x1 = 2 * x;
        let x2 = (2 * x + 1).min(w - 1);
        for c in 0..ch {
            let mut s = a[x1 * ch + c] as u32 + a[x2 * ch + c] as u32;
            let mut n = 2;
            if let Some(b) = b {
                s += b[x1 * ch + c] as u32 + b[x2 * ch + c] as u32;
                n += 2;
            }
            out[x * ch + c] = ((s + n / 2) / n) as u8;
        }
    }
    out
}

pub fn build_pyramid<R: BandSource + ?Sized>(
    reader: &mut R,
    out_dir: &Path,
    tile: u32,
    quality: u8,
    layout: Layout,
) -> Result<PyramidInfo> {
    let (w, h) = (reader.width(), reader.height());
    let n_levels = level_count(w, h, tile);
    let ch = reader.channels();
    let mut levels = vec![];
    let mut lw = w as usize;
    for l in 0..n_levels {
        if layout == Layout::Files {
            std::fs::create_dir_all(out_dir.join(l.to_string()))?;
        }
        levels.push(Level {
            width: lw,
            acc: Vec::with_capacity(lw * tile as usize * ch),
            rows: 0,
            tile_row: 0,
            pending: None,
        });
        lw = lw.div_ceil(2);
    }
    std::fs::create_dir_all(out_dir)?;
    let sink = match layout {
        Layout::Packed => Sink::Pack(PackWriter::create(&out_dir.join(TILE_PACK))?),
        Layout::Files => Sink::Files(out_dir.to_path_buf()),
    };
    let mut b = Builder {
        levels,
        tile,
        ch,
        quality,
        tiles: 0,
        sink,
    };
    let t0 = std::time::Instant::now();
    for band in 0..reader.band_count() {
        let (_y0, rows, px) = reader.read_band(band)?;
        let stride = w as usize * ch;
        for r in 0..rows as usize {
            b.push_row(0, &px[r * stride..(r + 1) * stride])?;
        }
        if band % 16 == 0 {
            tracing::info!(
                "pyramid: band {}/{} ({:.0?})",
                band + 1,
                reader.band_count(),
                t0.elapsed()
            );
        }
    }
    b.finish()?;
    if let Sink::Pack(pack) = b.sink {
        pack.finish()?;
    }
    Ok(PyramidInfo {
        width: w,
        height: h,
        levels: n_levels,
        tile_size: tile,
        channels: ch as u32,
        tiles: b.tiles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_counts() {
        assert_eq!(level_count(512, 512, 512), 1);
        assert_eq!(level_count(513, 10, 512), 2);
        assert_eq!(level_count(100_000, 50_000, 512), 9);
    }

    #[test]
    fn tile_keys_are_unique_and_bounded() {
        assert_eq!(image_tile_key(0, 0, 0), Some(0));
        assert_eq!(image_tile_key(1, 2, 3), Some(1 << 26 | 3 << 13 | 2));
        assert_eq!(image_tile_key(63, 8191, 8191), Some(u32::MAX));
        assert_eq!(image_tile_key(64, 0, 0), None);
        assert_eq!(image_tile_key(0, 8192, 0), None);
        assert_eq!(image_tile_key(0, 0, 8192), None);
    }

    #[test]
    fn downsample_box() {
        let a = [0u8, 10, 20, 30, 40];
        let b = [100u8, 110, 120, 130, 140];
        assert_eq!(downsample(&a, Some(&b), 5, 1), vec![55, 75, 90]);
        assert_eq!(downsample(&a, None, 5, 1), vec![5, 25, 40]);
    }
}
