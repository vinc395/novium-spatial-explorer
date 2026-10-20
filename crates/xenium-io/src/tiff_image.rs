//! TIFF / OME-TIFF level-0 reader producing 8-bit row bands for pyramid building.
//!
//! Supported: tiled or stripped; uncompressed, LZW, Deflate, PackBits, JPEG
//! (including YCbCr JPEG, converted here); 8/16-bit RGB(A) chunky or planar;
//! 8/16-bit grayscale; BigTIFF; three single-channel IFDs combined as RGB when the
//! caller names them explicitly. Not supported (explicit error): JPEG 2000,
//! palette, CMYK, Lab, float samples.
//!
//! The level-0 image is the top-level IFD with the most pixels unless the caller
//! picks one. Existing pyramid levels/SubIFDs are not used; the indexer rebuilds a
//! pyramid so every downstream tile is in a known, uniform format.

use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use tiff::decoder::{ChunkType, Decoder, DecodingResult, Limits};
use tiff::tags::{CompressionMethod, PhotometricInterpretation, PlanarConfiguration, Tag};
use tiff::ColorType;

#[derive(Debug, Clone, serde::Serialize)]
pub struct IfdInfo {
    pub index: usize,
    pub width: u32,
    pub height: u32,
    pub color: String,
    pub compression: u16,
    pub tiled: bool,
    pub chunk: (u32, u32),
    pub planar: bool,
    pub photometric: u16,
    pub sub_ifds: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TiffProbe {
    pub ifds: Vec<IfdInfo>,
    pub is_ome: bool,
    /// `(x, y)` µm per pixel from the first OME `<Pixels>` element, if present.
    pub ome_physical_size_um: Option<(f64, f64)>,
}

fn open_decoder(path: &Path) -> Result<Decoder<BufReader<File>>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(Decoder::new(BufReader::with_capacity(1 << 20, f))
        .with_context(|| format!("{} is not a readable TIFF", path.display()))?
        .with_limits(Limits::unlimited()))
}

fn ifd_info(dec: &mut Decoder<BufReader<File>>, index: usize) -> Result<IfdInfo> {
    let (width, height) = dec.dimensions()?;
    let color = dec
        .colortype()
        .map(|c| format!("{c:?}"))
        .unwrap_or_else(|e| format!("unsupported ({e})"));
    let compression = dec.find_tag_unsigned::<u16>(Tag::Compression)?.unwrap_or(1);
    let photometric = dec
        .find_tag_unsigned::<u16>(Tag::PhotometricInterpretation)?
        .unwrap_or(1);
    let planar = dec
        .find_tag_unsigned::<u16>(Tag::PlanarConfiguration)?
        .unwrap_or(1)
        == 2;
    let sub_ifds = dec
        .find_tag(Tag::SubIfd)?
        .map(|v| v.into_u64_vec().map(|v| v.len()).unwrap_or(1))
        .unwrap_or(0);
    Ok(IfdInfo {
        index,
        width,
        height,
        color,
        compression,
        tiled: dec.get_chunk_type() == ChunkType::Tile,
        chunk: dec.chunk_dimensions(),
        planar,
        photometric,
        sub_ifds,
    })
}

/// Parse `PhysicalSizeX/Y` (+ units) of the first OME `<Pixels>` element.
pub fn ome_physical_size_um(xml: &str) -> Option<(f64, f64)> {
    let start = xml.find("<Pixels ")?;
    let end = xml[start..].find('>')? + start;
    let el = &xml[start..end];
    let attr = |name: &str| -> Option<&str> {
        let key = format!("{name}=\"");
        let i = el.find(&key)? + key.len();
        let j = el[i..].find('"')? + i;
        Some(&el[i..j])
    };
    let to_um = |v: f64, unit: Option<&str>| -> Option<f64> {
        let f = match unit.unwrap_or("µm") {
            "µm" | "um" | "micron" | "\u{3bc}m" => 1.0,
            "nm" => 1e-3,
            "mm" => 1e3,
            "cm" => 1e4,
            "m" => 1e6,
            _ => return None,
        };
        Some(v * f)
    };
    let x = to_um(
        attr("PhysicalSizeX")?.parse().ok()?,
        attr("PhysicalSizeXUnit"),
    )?;
    let y = to_um(
        attr("PhysicalSizeY")?.parse().ok()?,
        attr("PhysicalSizeYUnit"),
    )?;
    (x > 0.0 && y > 0.0).then_some((x, y))
}

pub fn probe(path: &Path) -> Result<TiffProbe> {
    let mut dec = open_decoder(path)?;
    let mut ifds = vec![ifd_info(&mut dec, 0)?];
    let desc = dec.get_tag_ascii_string(Tag::ImageDescription).ok();
    while dec.more_images() && ifds.len() < 4096 {
        dec.next_image()?;
        let i = ifds.len();
        ifds.push(ifd_info(&mut dec, i)?);
    }
    let is_ome = desc.as_deref().is_some_and(|d| d.contains("<OME"));
    Ok(TiffProbe {
        ome_physical_size_um: desc
            .as_deref()
            .filter(|_| is_ome)
            .and_then(ome_physical_size_um),
        is_ome,
        ifds,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Layout {
    Chunky { spp: u16, bits: u8, ycbcr: bool },
    Planar { bits: u8 },
    Gray { bits: u8 },
}

/// Reads level 0 in horizontal bands (one chunk-row per band), always producing
/// 8-bit pixels with `channels()` samples (3 for colour, 1 for grayscale).
/// Largest decoded band (one chunk row) accepted. A stripped TIFF with a single
/// huge strip would otherwise be decoded whole.
pub const MAX_BAND_BYTES: u64 = 1 << 31;

/// A source of full-resolution row bands for the pyramid builder.
pub trait BandSource {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    /// 1 (gray) or 3 (RGB), interleaved.
    fn channels(&self) -> usize;
    fn band_count(&self) -> u32;
    /// `(first row, rows, pixels)` of band `b`.
    fn read_band(&mut self, b: u32) -> Result<(u32, u32, Vec<u8>)>;
}

impl BandSource for BandReader {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn channels(&self) -> usize {
        BandReader::channels(self)
    }
    fn band_count(&self) -> u32 {
        BandReader::band_count(self)
    }
    fn read_band(&mut self, b: u32) -> Result<(u32, u32, Vec<u8>)> {
        BandReader::read_band(self, b)
    }
}

pub struct BandReader {
    decs: Vec<Decoder<BufReader<File>>>,
    ifds: Vec<usize>,
    layout: Layout,
    pub width: u32,
    pub height: u32,
    chunk_w: u32,
    chunk_h: u32,
    across: u32,
    down: u32,
    tiled: bool,
    /// For 16-bit grayscale: `[lo, hi]` mapped linearly to 0..255.
    window16: Option<(u16, u16)>,
}

impl BandReader {
    /// `ifds`: `None` = auto (largest top-level IFD); `Some([i])` = that IFD;
    /// `Some([r, g, b])` = three single-channel IFDs combined into RGB.
    pub fn open(path: &Path, ifds: Option<&[usize]>) -> Result<Self> {
        let p = probe(path)?;
        let chosen: Vec<usize> = match ifds {
            Some(v) if v.len() == 1 || v.len() == 3 => v.to_vec(),
            Some(_) => bail!("give one IFD index, or three (R,G,B) single-channel IFDs"),
            None => vec![p
                .ifds
                .iter()
                .max_by_key(|i| (i.width as u64 * i.height as u64, std::cmp::Reverse(i.index)))
                .map(|i| i.index)
                .ok_or_else(|| anyhow!("TIFF has no images"))?],
        };
        let first = &p.ifds[chosen
            .first()
            .copied()
            .filter(|&i| i < p.ifds.len())
            .ok_or_else(|| anyhow!("IFD out of range"))?];
        let mut decs = vec![];
        for &i in &chosen {
            let info = p
                .ifds
                .get(i)
                .ok_or_else(|| anyhow!("IFD {i} does not exist ({} IFDs)", p.ifds.len()))?;
            if info.compression == 33003 || info.compression == 33005 || info.compression == 34712 {
                bail!("IFD {i} uses JPEG 2000 compression, which is not supported; convert the image first");
            }
            if (info.width, info.height, info.tiled, info.chunk, &info.color)
                != (
                    first.width,
                    first.height,
                    first.tiled,
                    first.chunk,
                    &first.color,
                )
            {
                bail!("IFD {i} does not match IFD {} in size, tiling or pixel type; refusing to combine them", first.index);
            }
            let mut d = open_decoder(path)?;
            d.seek_to_image(i)?;
            decs.push(d);
        }
        let d0 = &mut decs[0];
        let (width, height) = d0.dimensions()?;
        let (chunk_w, chunk_h) = d0.chunk_dimensions();
        let tiled = d0.get_chunk_type() == ChunkType::Tile;
        let (across, down) = if tiled {
            (width.div_ceil(chunk_w), height.div_ceil(chunk_h))
        } else {
            (1, height.div_ceil(chunk_h))
        };
        let band_bytes = chunk_h.min(height) as u64 * width as u64 * 8;
        if band_bytes > MAX_BAND_BYTES {
            bail!(
                "{}: one chunk row is {} rows x {} px (~{} MB decoded); re-save the image as a tiled TIFF (e.g. `vips tiffsave in.tif out.tif --tile --pyramid --compression jpeg`)",
                path.display(),
                chunk_h.min(height),
                width,
                band_bytes >> 20
            );
        }
        let color = d0.colortype().context("unsupported TIFF colour type")?;
        let planar = d0
            .find_tag_unsigned::<u16>(Tag::PlanarConfiguration)?
            .unwrap_or(1)
            == PlanarConfiguration::Planar.to_u16();
        let photometric = d0
            .find_tag_unsigned::<u16>(Tag::PhotometricInterpretation)?
            .unwrap_or(1);
        let compression = d0.find_tag_unsigned::<u16>(Tag::Compression)?.unwrap_or(1);
        let layout = if decs.len() == 3 {
            match color {
                ColorType::Gray(b @ (8 | 16)) => Layout::Gray { bits: b },
                c => bail!("separate-IFD RGB needs 8/16-bit single-channel IFDs, got {c:?}"),
            }
        } else {
            match color {
                ColorType::RGB(b @ (8 | 16)) | ColorType::RGBA(b @ (8 | 16)) if planar => Layout::Planar { bits: b },
                ColorType::RGB(b @ (8 | 16)) => Layout::Chunky { spp: 3, bits: b, ycbcr: false },
                ColorType::RGBA(b @ (8 | 16)) => Layout::Chunky { spp: 4, bits: b, ycbcr: false },
                ColorType::YCbCr(8) => Layout::Chunky { spp: 3, bits: 8, ycbcr: true },
                ColorType::Gray(b @ (8 | 16)) => Layout::Gray { bits: b },
                c => bail!(
                    "unsupported TIFF pixel type {c:?} (photometric {photometric}, compression {compression})"
                ),
            }
        };
        let _ = CompressionMethod::from_u16(compression);
        let _ = PhotometricInterpretation::from_u16(photometric);
        let mut r = BandReader {
            decs,
            ifds: chosen,
            layout,
            width,
            height,
            chunk_w,
            chunk_h,
            across,
            down,
            tiled,
            window16: None,
        };
        // Any 16-bit layout is windowed from sampled data (12-bit-in-16 data would
        // otherwise render nearly black with a plain >> 8).
        if matches!(
            layout,
            Layout::Gray { bits: 16 }
                | Layout::Chunky { bits: 16, .. }
                | Layout::Planar { bits: 16 }
        ) {
            r.window16 = Some(r.estimate_window16()?);
        }
        Ok(r)
    }

    pub fn channels(&self) -> usize {
        match (self.ifds.len(), self.layout) {
            (1, Layout::Gray { .. }) => 1,
            _ => 3,
        }
    }

    pub fn band_count(&self) -> u32 {
        self.down
    }

    pub fn band_height(&self) -> u32 {
        self.chunk_h
    }

    pub fn ifd_indices(&self) -> &[usize] {
        &self.ifds
    }

    pub fn window16(&self) -> Option<(u16, u16)> {
        self.window16
    }

    /// Sample up to 64 chunks and take the 0.1 / 99.9 percentiles.
    fn estimate_window16(&mut self) -> Result<(u16, u16)> {
        let total = self.across * self.down;
        let step = (total / 64).max(1);
        let mut vals: Vec<u16> = vec![];
        let mut idx = 0;
        while idx < total {
            if let DecodingResult::U16(v) = self.decs[0].read_chunk(idx)? {
                vals.extend(v.iter().step_by(7));
            }
            idx += step;
        }
        if vals.is_empty() {
            return Ok((0, u16::MAX));
        }
        vals.sort_unstable();
        let q = |p: f64| vals[((vals.len() - 1) as f64 * p) as usize];
        let (lo, hi) = (q(0.001), q(0.999));
        let lo = lo.min(u16::MAX - 1);
        Ok((lo, hi.max(lo + 1)))
    }

    /// Read band `b` (chunk row). Returns `(y0, rows, pixels)` where `pixels` is
    /// `rows * width * channels()` bytes.
    pub fn read_band(&mut self, b: u32) -> Result<(u32, u32, Vec<u8>)> {
        if b >= self.down {
            bail!("band {b} out of range");
        }
        let y0 = b * self.chunk_h;
        let rows = self.chunk_h.min(self.height - y0);
        let ch = self.channels();
        let w = self.width as usize;
        let mut out = vec![0u8; rows as usize * w * ch];
        for cx in 0..self.across {
            let x0 = cx * self.chunk_w;
            let cw = if self.tiled {
                self.chunk_w.min(self.width - x0)
            } else {
                self.width
            };
            let idx = b * self.across + cx;
            match (self.ifds.len(), self.layout) {
                (3, _) => {
                    for c in 0..3 {
                        let data = to_u8(self.decs[c].read_chunk(idx)?, self.window16)?;
                        let stride = row_stride(data.len(), rows, cw, self.chunk_w, 1)?;
                        for r in 0..rows as usize {
                            for x in 0..cw as usize {
                                out[(r * w + x0 as usize + x) * 3 + c] = data[r * stride + x];
                            }
                        }
                    }
                }
                (_, Layout::Planar { .. }) => {
                    let per_plane = self.across * self.down;
                    for c in 0..3u32 {
                        let data =
                            to_u8(self.decs[0].read_chunk(idx + c * per_plane)?, self.window16)?;
                        let stride = row_stride(data.len(), rows, cw, self.chunk_w, 1)?;
                        for r in 0..rows as usize {
                            for x in 0..cw as usize {
                                out[(r * w + x0 as usize + x) * 3 + c as usize] =
                                    data[r * stride + x];
                            }
                        }
                    }
                }
                (_, Layout::Chunky { spp, ycbcr, .. }) => {
                    let data = to_u8(self.decs[0].read_chunk(idx)?, self.window16)?;
                    let spp = spp as usize;
                    let stride = row_stride(data.len(), rows, cw, self.chunk_w, spp)?;
                    for r in 0..rows as usize {
                        for x in 0..cw as usize {
                            let s = &data[r * stride + x * spp..r * stride + x * spp + 3];
                            let px = if ycbcr {
                                ycbcr_to_rgb(s[0], s[1], s[2])
                            } else {
                                [s[0], s[1], s[2]]
                            };
                            out[(r * w + x0 as usize + x) * 3..][..3].copy_from_slice(&px);
                        }
                    }
                }
                (_, Layout::Gray { .. }) => {
                    let data = to_u8(self.decs[0].read_chunk(idx)?, self.window16)?;
                    let stride = row_stride(data.len(), rows, cw, self.chunk_w, 1)?;
                    for r in 0..rows as usize {
                        out[r * w + x0 as usize..][..cw as usize]
                            .copy_from_slice(&data[r * stride..r * stride + cw as usize]);
                    }
                }
            }
        }
        Ok((y0, rows, out))
    }
}

/// Bytes per decoded chunk row: either cropped to the image (`cw`) or padded (`chunk_w`).
fn row_stride(len: usize, rows: u32, cw: u32, chunk_w: u32, spp: usize) -> Result<usize> {
    for w in [cw, chunk_w] {
        let stride = w as usize * spp;
        if len >= stride * rows as usize && len.is_multiple_of(stride) {
            return Ok(stride);
        }
    }
    bail!("unexpected decoded chunk size {len} for {rows} rows of width {cw}/{chunk_w} x {spp}")
}

fn to_u8(d: DecodingResult, window: Option<(u16, u16)>) -> Result<Vec<u8>> {
    Ok(match d {
        DecodingResult::U8(v) => v,
        DecodingResult::U16(v) => match window {
            Some((lo, hi)) => {
                let span = (hi - lo) as f32;
                v.iter()
                    .map(|&x| ((x.saturating_sub(lo)) as f32 / span * 255.0).min(255.0) as u8)
                    .collect()
            }
            None => v.iter().map(|&x| (x >> 8) as u8).collect(),
        },
        other => bail!(
            "unsupported sample format {:?}",
            std::mem::discriminant(&other)
        ),
    })
}

/// JFIF / BT.601 full-range YCbCr -> RGB.
pub fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (y, cb, cr) = (y as f32, cb as f32 - 128.0, cr as f32 - 128.0);
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    [
        c(y + 1.402 * cr),
        c(y - 0.344_136 * cb - 0.714_136 * cr),
        c(y + 1.772 * cb),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ome_physical_size_parsing() {
        let xml = r#"<OME><Image><Pixels DimensionOrder="XYCZT" PhysicalSizeX="0.2737" PhysicalSizeXUnit="µm" PhysicalSizeY="0.2737" SizeX="10"></Pixels></Image></OME>"#;
        assert_eq!(ome_physical_size_um(xml), Some((0.2737, 0.2737)));
        let nm = r#"<Pixels PhysicalSizeX="500" PhysicalSizeXUnit="nm" PhysicalSizeY="500" PhysicalSizeYUnit="nm">"#;
        assert_eq!(ome_physical_size_um(nm), Some((0.5, 0.5)));
        assert_eq!(ome_physical_size_um("<Pixels SizeX=\"3\">"), None);
    }

    #[test]
    fn ycbcr_neutral_and_primaries() {
        assert_eq!(ycbcr_to_rgb(128, 128, 128), [128, 128, 128]);
        assert_eq!(ycbcr_to_rgb(255, 128, 128), [255, 255, 255]);
        let red = ycbcr_to_rgb(76, 85, 255);
        assert!(red[0] > 250 && red[1] < 5 && red[2] < 5, "{red:?}");
    }
}
