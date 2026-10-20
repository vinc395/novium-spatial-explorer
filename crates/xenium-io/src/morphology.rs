//! Xenium morphology channels (`morphology_focus/ch*.ome.tif`): one 16-bit, tiled,
//! JPEG 2000-compressed OME-TIFF per channel on the morphology pixel grid (Xenium µm =
//! pixel x pixel size). Tiles are decoded in parallel (OpenJPEG, pure-Rust port),
//! mapped to 8-bit through a per-channel intensity window, and handed to the pyramid
//! builder as row bands. `write_ome_jpeg2000` writes the same layout for synthetic
//! test data (encoded with an independent lossless JPEG 2000 implementation).

use crate::ome_tiles::{TiledTiff, COMPRESSION_JPEG2000};
use crate::tiff_image::{ome_physical_size_um, BandSource};
use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// Decode one JPEG 2000 tile (J2K codestream or JP2 file) to 16-bit samples.
pub fn decode_tile(bytes: &[u8]) -> Result<(u32, u32, Vec<u16>)> {
    let img = jpeg2k::Image::from_bytes(bytes).map_err(|e| anyhow!("JPEG 2000 decode: {e}"))?;
    let c = img
        .components()
        .first()
        .ok_or_else(|| anyhow!("JPEG 2000 tile has no components"))?;
    if c.is_signed() {
        bail!("signed JPEG 2000 samples are not supported");
    }
    let v = c.data().iter().map(|&x| x.clamp(0, 65535) as u16).collect();
    Ok((c.width(), c.height(), v))
}

/// The channel files of a `morphology_focus` directory, in file-name order.
pub fn channel_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            n.ends_with(".ome.tif") || n.ends_with(".ome.tiff")
        })
        .collect();
    v.sort();
    if v.is_empty() {
        bail!("{}: no *.ome.tif morphology channels", dir.display());
    }
    Ok(v)
}

/// Lookup table mapping raw intensities linearly from `[lo, hi]` to `[0, 255]`.
pub fn window_lut(lo: u16, hi: u16) -> Vec<u8> {
    let span = (hi.max(lo + 1) - lo) as f64;
    (0..=u16::MAX)
        .map(|v| {
            if v <= lo {
                0
            } else if v >= hi {
                255
            } else {
                (((v - lo) as f64 / span) * 255.0).round() as u8
            }
        })
        .collect()
}

pub struct MorphologyChannel {
    pub tiff: TiledTiff,
    pub name: String,
    /// µm per pixel from the OME metadata, when present.
    pub pixel_size_um: Option<f64>,
}

impl MorphologyChannel {
    pub fn open(path: &Path) -> Result<Self> {
        let tiff = TiledTiff::open(path)?;
        let what = path.display();
        if tiff.samples_per_pixel != 1 {
            bail!(
                "{what}: {} samples per pixel; one grayscale channel per file expected",
                tiff.samples_per_pixel
            );
        }
        if tiff.bits_per_sample != 16 && tiff.bits_per_sample != 8 {
            bail!(
                "{what}: {} bits per sample are not supported (8 or 16 expected)",
                tiff.bits_per_sample
            );
        }
        if tiff.sample_format != 1 {
            bail!("{what}: only unsigned integer samples are supported");
        }
        if tiff.compression != COMPRESSION_JPEG2000 {
            bail!("{what}: compression {} is not supported for morphology channels (JPEG 2000 expected)", tiff.compression);
        }
        let name = tiff.channel_name();
        let pixel_size_um = tiff
            .description
            .as_deref()
            .and_then(ome_physical_size_um)
            .map(|(x, _)| x);
        Ok(MorphologyChannel {
            tiff,
            name,
            pixel_size_um,
        })
    }

    pub fn width(&self) -> u32 {
        self.tiff.levels[0].width
    }

    pub fn height(&self) -> u32 {
        self.tiff.levels[0].height
    }

    /// Decoded tile, cropped to the image (edge tiles are padded in the file).
    fn tile(&self, level: usize, tx: u32, ty: u32) -> Result<(usize, usize, Vec<u16>)> {
        let l = &self.tiff.levels[level];
        let (w, h, px) = decode_tile(&self.tiff.read_tile(level, tx, ty)?).with_context(|| {
            format!(
                "{}: tile ({tx}, {ty}) of level {level}",
                self.tiff.path.display()
            )
        })?;
        let cw = (l.width - tx * l.tile_width).min(l.tile_width).min(w) as usize;
        let ch = (l.height - ty * l.tile_height).min(l.tile_height).min(h) as usize;
        if cw == w as usize {
            return Ok((cw, ch, px[..cw * ch].to_vec()));
        }
        let mut out = Vec::with_capacity(cw * ch);
        for r in 0..ch {
            out.extend_from_slice(&px[r * w as usize..r * w as usize + cw]);
        }
        Ok((cw, ch, out))
    }

    /// Intensity window: the 0.5th and 99.9th percentiles of the non-zero pixels of
    /// the smallest stored level at least 8000 px wide (full resolution otherwise).
    pub fn estimate_window(&self) -> Result<(u16, u16)> {
        let levels = &self.tiff.levels;
        let lv = (0..levels.len())
            .rev()
            .find(|&i| levels[i].width >= 8000)
            .unwrap_or(0);
        let l = &levels[lv];
        let tiles: Vec<(u32, u32)> = (0..l.tiles_down())
            .flat_map(|ty| (0..l.tiles_across()).map(move |tx| (tx, ty)))
            .collect();
        let hist = tiles
            .par_iter()
            .map(|&(tx, ty)| -> Result<Vec<u64>> {
                let (_, _, px) = self.tile(lv, tx, ty)?;
                let mut h = vec![0u64; 65536];
                for v in px {
                    h[v as usize] += 1;
                }
                Ok(h)
            })
            .try_reduce(
                || vec![0u64; 65536],
                |mut a, b| {
                    for (x, y) in a.iter_mut().zip(b) {
                        *x += y;
                    }
                    Ok(a)
                },
            )?;
        let total: u64 = hist[1..].iter().sum();
        if total == 0 {
            return Ok((0, 1));
        }
        let pct = |q: f64| -> u16 {
            let target = (total as f64 * q).ceil().max(1.0) as u64;
            let mut acc = 0;
            for (v, &n) in hist.iter().enumerate().skip(1) {
                acc += n;
                if acc >= target {
                    return v as u16;
                }
            }
            u16::MAX
        };
        let lo = pct(0.005);
        let hi = pct(0.999).max(lo.saturating_add(1));
        Ok((lo, hi))
    }

    pub fn bands(&self, window: (u16, u16)) -> MorphologyBands<'_> {
        MorphologyBands {
            ch: self,
            lut: window_lut(window.0, window.1),
        }
    }
}

/// Full-resolution 8-bit row bands (one band per row of TIFF tiles).
pub struct MorphologyBands<'a> {
    ch: &'a MorphologyChannel,
    lut: Vec<u8>,
}

impl BandSource for MorphologyBands<'_> {
    fn width(&self) -> u32 {
        self.ch.width()
    }
    fn height(&self) -> u32 {
        self.ch.height()
    }
    fn channels(&self) -> usize {
        1
    }
    fn band_count(&self) -> u32 {
        self.ch.tiff.levels[0].tiles_down()
    }
    fn read_band(&mut self, b: u32) -> Result<(u32, u32, Vec<u8>)> {
        let l = &self.ch.tiff.levels[0];
        let y0 = b * l.tile_height;
        let rows = l.tile_height.min(l.height - y0) as usize;
        let w = l.width as usize;
        let lut = &self.lut;
        let tiles: Vec<(usize, usize, Vec<u16>)> = (0..l.tiles_across())
            .into_par_iter()
            .map(|tx| self.ch.tile(0, tx, b))
            .collect::<Result<_>>()?;
        let mut out = vec![0u8; w * rows];
        let mut x0 = 0;
        for (cw, ch, px) in tiles {
            for r in 0..ch.min(rows) {
                let dst = &mut out[r * w + x0..r * w + x0 + cw];
                for (d, &v) in dst.iter_mut().zip(&px[r * cw..(r + 1) * cw]) {
                    *d = lut[v as usize];
                }
            }
            x0 += cw;
        }
        Ok((y0, rows as u32, out))
    }
}

// ---------------------------------------------------------------- synthetic writer

/// One stored level: width, height and 16-bit pixels (row-major).
pub struct Level16 {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u16>,
}

/// Write a tiled BigTIFF with JPEG 2000 tiles (compression 34712): `levels[0]` as the
/// main image and the rest as SubIFDs, with `description` (OME-XML) on the first IFD.
pub fn write_ome_jpeg2000(
    path: &Path,
    levels: &[Level16],
    tile: u32,
    description: &str,
) -> Result<()> {
    struct Ifd {
        width: u32,
        height: u32,
        offsets: Vec<u64>,
        counts: Vec<u64>,
    }
    let mut buf: Vec<u8> = vec![0; 16];
    buf[..4].copy_from_slice(b"II\x2b\x00");
    buf[4..6].copy_from_slice(&8u16.to_le_bytes());
    let desc_off = buf.len() as u64;
    buf.extend_from_slice(description.as_bytes());
    buf.push(0);
    let opts = pure_j2k::Jpeg2kOptions::default();
    let mut ifds = vec![];
    for lv in levels {
        let (across, down) = (lv.width.div_ceil(tile), lv.height.div_ceil(tile));
        let (mut offsets, mut counts) = (vec![], vec![]);
        for ty in 0..down {
            for tx in 0..across {
                let mut t = vec![0u16; (tile * tile) as usize];
                for r in 0..tile {
                    let y = ty * tile + r;
                    if y >= lv.height {
                        break;
                    }
                    for c in 0..tile {
                        let x = tx * tile + c;
                        if x < lv.width {
                            t[(r * tile + c) as usize] = lv.pixels[(y * lv.width + x) as usize];
                        }
                    }
                }
                let mut j2k = vec![];
                pure_j2k::encode(&t, tile, tile, &opts, &mut j2k)
                    .map_err(|e| anyhow!("JPEG 2000 encode: {e:?}"))?;
                offsets.push(buf.len() as u64);
                counts.push(j2k.len() as u64);
                buf.extend_from_slice(&j2k);
            }
        }
        ifds.push(Ifd {
            width: lv.width,
            height: lv.height,
            offsets,
            counts,
        });
    }
    // value arrays
    let mut arrays = vec![];
    for i in &ifds {
        let a = buf.len() as u64;
        for o in &i.offsets {
            buf.extend_from_slice(&o.to_le_bytes());
        }
        let b = buf.len() as u64;
        for c in &i.counts {
            buf.extend_from_slice(&c.to_le_bytes());
        }
        arrays.push((a, b));
    }
    let entry = |buf: &mut Vec<u8>, tag: u16, typ: u16, count: u64, value: u64| {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&typ.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&value.to_le_bytes());
    };
    let write_ifd = |buf: &mut Vec<u8>, i: usize, sub_ifds: Option<(u64, u64)>| -> u64 {
        let at = buf.len() as u64;
        let f = &ifds[i];
        let n = f.offsets.len() as u64;
        let (oa, ca) = arrays[i];
        let mut e: Vec<(u16, u16, u64, u64)> = vec![
            (254, 4, 1, if i == 0 { 0 } else { 1 }),
            (256, 4, 1, f.width as u64),
            (257, 4, 1, f.height as u64),
            (258, 3, 1, 16),
            (259, 3, 1, COMPRESSION_JPEG2000 as u64),
            (262, 3, 1, 1),
            (277, 3, 1, 1),
            (284, 3, 1, 1),
            (322, 3, 1, tile as u64),
            (323, 3, 1, tile as u64),
            (324, 16, n, if n == 1 { ifds[i].offsets[0] } else { oa }),
            (325, 16, n, if n == 1 { ifds[i].counts[0] } else { ca }),
            (339, 3, 1, 1),
        ];
        if i == 0 {
            e.push((270, 2, description.len() as u64 + 1, desc_off));
            if let Some((count, off)) = sub_ifds {
                e.push((330, 18, count, off));
            }
        }
        e.sort_by_key(|x| x.0);
        buf.extend_from_slice(&(e.len() as u64).to_le_bytes());
        for (tag, typ, count, value) in e {
            entry(buf, tag, typ, count, value);
        }
        buf.extend_from_slice(&0u64.to_le_bytes());
        at
    };
    let subs: Vec<u64> = (1..ifds.len())
        .map(|i| write_ifd(&mut buf, i, None))
        .collect();
    let sub_ifds = match subs.len() {
        0 => None,
        1 => Some((1, subs[0])),
        n => {
            let at = buf.len() as u64;
            for s in &subs {
                buf.extend_from_slice(&s.to_le_bytes());
            }
            Some((n as u64, at))
        }
    };
    let first = write_ifd(&mut buf, 0, sub_ifds);
    buf[8..16].copy_from_slice(&first.to_le_bytes());
    std::fs::write(path, buf).with_context(|| format!("write {}", path.display()))
}

/// 2x2 mean downsample of a 16-bit image.
pub fn halve(l: &Level16) -> Level16 {
    let (w, h) = (l.width.div_ceil(2), l.height.div_ceil(2));
    let mut px = vec![0u16; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0u32;
            let mut n = 0u32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (sx, sy) = (2 * x + dx, 2 * y + dy);
                if sx < l.width && sy < l.height {
                    s += l.pixels[(sy * l.width + sx) as usize] as u32;
                    n += 1;
                }
            }
            px[(y * w + x) as usize] = (s / n.max(1)) as u16;
        }
    }
    Level16 {
        width: w,
        height: h,
        pixels: px,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_tiles_decode_losslessly_with_openjpeg() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ch0000_dapi.ome.tif");
        let (w, h) = (300u32, 170u32);
        let l0 = Level16 {
            width: w,
            height: h,
            pixels: (0..w * h).map(|i| ((i * 37) % 4093) as u16 * 13).collect(),
        };
        let l1 = halve(&l0);
        let xml = r#"<OME><Image><Pixels PhysicalSizeX="1.5" PhysicalSizeY="1.5"><Channel ID="C0" Name="DAPI"/><Channel ID="C1" Name="18S"/><TiffData FirstC="0"><UUID FileName="ch0000_dapi.ome.tif">u</UUID></TiffData></Pixels></Image></OME>"#;
        write_ome_jpeg2000(&p, &[l0, l1], 128, xml).unwrap();
        let ch = MorphologyChannel::open(&p).unwrap();
        assert_eq!(ch.name, "DAPI");
        assert_eq!(ch.pixel_size_um, Some(1.5));
        assert_eq!(
            (ch.width(), ch.height(), ch.tiff.levels.len()),
            (300, 170, 2)
        );
        // Lossless round trip through two independent codecs, edge tiles cropped.
        let (cw, chh, px) = ch.tile(0, 2, 1).unwrap();
        assert_eq!((cw, chh), (300 - 256, 170 - 128));
        assert_eq!(px[0], (((128 * w + 256) * 37) % 4093) as u16 * 13);
        // Bands assemble full rows through the window.
        let mut bands = ch.bands((0, 65535));
        assert_eq!(bands.band_count(), 2);
        let (y0, rows, b) = bands.read_band(1).unwrap();
        assert_eq!((y0, rows, b.len()), (128, 42, 300 * 42));
        let (lo, hi) = ch.estimate_window().unwrap();
        assert!(lo < hi);
    }

    #[test]
    fn window_lut_is_linear_and_clamped() {
        let l = window_lut(100, 355);
        assert_eq!((l[0], l[100], l[355], l[60000]), (0, 0, 255, 255));
        assert_eq!(l[228], 128);
    }
}
