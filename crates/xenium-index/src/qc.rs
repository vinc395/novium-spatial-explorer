//! Registration QC without looking at images: for a sample of Xenium nuclei,
//! measure the nuclear stain at the nucleus centre shifted by a grid of offsets:
//! H&E darkness (255 - mean RGB; nuclei stain dark with haematoxylin) or, for a
//! fluorescence channel such as DAPI, brightness. If the image is placed right, the
//! signal peaks at offset (0, 0). Run per spatial block, this also exposes local
//! deformation. Only aggregate numbers are reported.

use crate::store::{SectionStore, StoreError};
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::collections::HashMap;
use xenium_core::codec::decode_polygons;
use xenium_core::manifest::ImageInfo;
use xenium_core::Affine2;

#[derive(Debug, Clone, Serialize)]
pub struct OffsetPeak {
    /// Offset (µm, Xenium frame) of the signal peak (half-maximum centroid). The image
    /// content sits at the registered position + this offset. The `darkness_*` values
    /// hold the signal named in `RegistrationQc::signal` (brightness for a channel).
    pub best_dx_um: f64,
    pub best_dy_um: f64,
    /// Mean darkness at the registered position and the maximum over all offsets.
    pub darkness_at_zero: f64,
    pub darkness_at_best: f64,
    /// Mean darkness over offsets >= `ring_um` from zero (background reference).
    pub darkness_background: f64,
    /// (zero - background) / background: > 0 means nuclei are darker than the surroundings.
    pub contrast_at_zero: f64,
    pub n_nuclei: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockPeak {
    pub block: [usize; 2],
    pub center_um: [f64; 2],
    #[serde(flatten)]
    pub peak: OffsetPeak,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistrationQc {
    pub section_id: String,
    pub image_id: String,
    /// "darkness" (H&E) or "brightness" (fluorescence channel, e.g. DAPI).
    pub signal: String,
    pub registration_mode: String,
    pub sampled_level: u32,
    pub sampled_pixel_um: f64,
    pub search_radius_um: f64,
    pub step_um: f64,
    pub global: OffsetPeak,
    pub blocks: Vec<BlockPeak>,
    pub verdict: String,
}

/// `(width, height, interleaved pixels)` of one decoded JPEG tile.
type DecodedTile = (usize, usize, Vec<u8>);

struct Sampler<'a> {
    store: &'a SectionStore,
    img: &'a ImageInfo,
    px_from_xu: Affine2,
    level: u32,
    /// Fluorescence channel: nuclei are bright, so the signal is the intensity itself.
    bright: bool,
    tiles: HashMap<(u32, u32), Option<DecodedTile>>,
}

impl Sampler<'_> {
    /// Nuclear signal (0..255) at a point in µm, or None outside the image.
    fn signal(&mut self, x: f64, y: f64) -> Result<Option<f64>> {
        let (px, py) = self.px_from_xu.apply(x, y);
        let scale = (1u64 << self.level) as f64;
        let (lx, ly) = (px / scale, py / scale);
        if lx < 0.0 || ly < 0.0 || px >= self.img.width as f64 || py >= self.img.height as f64 {
            return Ok(None);
        }
        let ts = self.img.tile_size as f64;
        let key = ((lx / ts) as u32, (ly / ts) as u32);
        if !self.tiles.contains_key(&key) {
            let tile = match self
                .store
                .image_tile(&self.img.id, self.level, key.0, key.1)
            {
                Ok(bytes) => {
                    let mut d =
                        zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(&bytes));
                    let px = d.decode().map_err(|e| {
                        anyhow!(
                            "decode {} tile {}/{}_{}: {e:?}",
                            self.img.id,
                            self.level,
                            key.0,
                            key.1
                        )
                    })?;
                    let (w, h) = d
                        .dimensions()
                        .ok_or_else(|| anyhow!("no JPEG dimensions"))?;
                    Some((w, h, px))
                }
                Err(e) if matches!(e.downcast_ref(), Some(StoreError::NotFound(_))) => None,
                Err(e) => return Err(e),
            };
            self.tiles.insert(key, tile);
        }
        let Some((w, h, px)) = &self.tiles[&key] else {
            return Ok(None);
        };
        let ix = (lx - key.0 as f64 * ts) as usize;
        let iy = (ly - key.1 as f64 * ts) as usize;
        if ix >= *w || iy >= *h {
            return Ok(None);
        }
        let ch = px.len() / (w * h);
        let o = (iy * w + ix) * ch;
        let mean = if ch >= 3 {
            (px[o] as f64 + px[o + 1] as f64 + px[o + 2] as f64) / 3.0
        } else {
            px[o] as f64
        };
        Ok(Some(if self.bright { mean } else { 255.0 - mean }))
    }
}

fn peak(
    sampler: &mut Sampler,
    pts: &[[f64; 2]],
    radius: f64,
    step: f64,
    ring: f64,
) -> Result<Option<OffsetPeak>> {
    if pts.len() < 20 {
        return Ok(None);
    }
    let n = (radius / step).round() as i32;
    let mut grid: Vec<(f64, f64, f64)> = vec![];
    let (mut zero, mut bg_sum, mut bg_n) = (f64::NAN, 0.0, 0usize);
    for iy in -n..=n {
        for ix in -n..=n {
            let (dx, dy) = (ix as f64 * step, iy as f64 * step);
            let (mut s, mut c) = (0.0, 0usize);
            for p in pts {
                if let Some(v) = sampler.signal(p[0] + dx, p[1] + dy)? {
                    s += v;
                    c += 1;
                }
            }
            if c == 0 {
                continue;
            }
            let m = s / c as f64;
            if ix == 0 && iy == 0 {
                zero = m;
            }
            if (dx * dx + dy * dy).sqrt() >= ring {
                bg_sum += m;
                bg_n += 1;
            }
            grid.push((dx, dy, m));
        }
    }
    let best_d = grid.iter().map(|g| g.2).fold(f64::NEG_INFINITY, f64::max);
    let bg = if bg_n > 0 {
        bg_sum / bg_n as f64
    } else {
        f64::NAN
    };
    // Nuclei give a flat-topped response, so the peak is the weighted centroid of the
    // offsets within 15% of the peak height (an argmax on a plateau is arbitrary).
    let min_d = grid.iter().map(|g| g.2).fold(f64::INFINITY, f64::min);
    let thr = best_d - 0.15 * (best_d - min_d);
    let (mut wx, mut wy, mut ws) = (0.0, 0.0, 0.0);
    for &(dx, dy, m) in &grid {
        let w = m - thr;
        if w > 0.0 {
            wx += w * dx;
            wy += w * dy;
            ws += w;
        }
    }
    let best = if ws > 0.0 {
        (wx / ws, wy / ws)
    } else {
        (0.0, 0.0)
    };
    Ok(Some(OffsetPeak {
        best_dx_um: (best.0 * 10.0).round() / 10.0,
        best_dy_um: (best.1 * 10.0).round() / 10.0,
        darkness_at_zero: zero,
        darkness_at_best: best_d,
        darkness_background: bg,
        contrast_at_zero: (zero - bg) / bg,
        n_nuclei: pts.len(),
    }))
}

pub fn registration_qc(
    store: &SectionStore,
    image_id: Option<&str>,
    max_nuclei: usize,
    radius: f64,
    step: f64,
    blocks: usize,
) -> Result<RegistrationQc> {
    let m = &store.manifest;
    let img = match image_id {
        Some(id) => m.image(id).ok_or_else(|| anyhow!("no image {id:?}"))?,
        None => m
            .images
            .iter()
            .find(|i| i.channel.is_none())
            .ok_or_else(|| anyhow!("index has no H&E image (registration QC needs one)"))?,
    };
    let nuclei = m
        .cells
        .as_ref()
        .and_then(|c| c.nuclei.as_ref())
        .ok_or_else(|| anyhow!("index has no nucleus boundaries"))?;
    // Nucleus centres (vertex mean of each ring).
    let mut centres: Vec<[f64; 2]> = vec![];
    for ch in &nuclei.chunks {
        let Some(raw) = store.polygon_chunk(true, ch[0] as i32, ch[1] as i32)? else {
            continue;
        };
        let (_, c) = decode_polygons(&raw)?;
        for i in 0..c.len() {
            let (a, b) = (c.starts[i] as usize, c.starts[i + 1] as usize);
            let n = (b - a) as f64;
            let (sx, sy) = c.xy[2 * a..2 * b]
                .chunks_exact(2)
                .fold((0.0, 0.0), |s, v| (s.0 + v[0] as f64, s.1 + v[1] as f64));
            centres.push([sx / n, sy / n]);
        }
    }
    if centres.is_empty() {
        bail!("no nuclei found");
    }
    let stride = centres.len().div_ceil(max_nuclei.max(1));
    let sample: Vec<[f64; 2]> = centres.iter().step_by(stride).copied().collect();
    // Sample at roughly 0.5-1 µm per pixel.
    let px_um = img.registration.approx_pixel_size_um;
    let level = ((0.8 / px_um).log2().round().max(0.0) as u32).min(img.levels - 1);
    let mut sampler = Sampler {
        store,
        img,
        px_from_xu: img.registration.matrix_xu_from_px.inverse()?,
        level,
        bright: img.channel.is_some(),
        tiles: HashMap::new(),
    };
    let (signal, stronger) = if sampler.bright {
        ("brightness", "brighter")
    } else {
        ("darkness", "darker")
    };
    let ring = (radius * 0.6).max(10.0);
    let global = peak(&mut sampler, &sample, radius, step, ring)?
        .ok_or_else(|| anyhow!("too few nuclei inside the image"))?;
    let b = m.bounds_um;
    let mut out_blocks = vec![];
    for by in 0..blocks {
        for bx in 0..blocks {
            let x0 = b[0] + (b[2] - b[0]) * bx as f64 / blocks as f64;
            let x1 = b[0] + (b[2] - b[0]) * (bx + 1) as f64 / blocks as f64;
            let y0 = b[1] + (b[3] - b[1]) * by as f64 / blocks as f64;
            let y1 = b[1] + (b[3] - b[1]) * (by + 1) as f64 / blocks as f64;
            let pts: Vec<[f64; 2]> = sample
                .iter()
                .filter(|p| p[0] >= x0 && p[0] < x1 && p[1] >= y0 && p[1] < y1)
                .copied()
                .collect();
            if let Some(p) = peak(&mut sampler, &pts, radius, step, ring)? {
                out_blocks.push(BlockPeak {
                    block: [bx, by],
                    center_um: [(x0 + x1) / 2.0, (y0 + y1) / 2.0],
                    peak: p,
                });
            }
        }
    }
    let off = |p: &OffsetPeak| (p.best_dx_um.powi(2) + p.best_dy_um.powi(2)).sqrt();
    let worst = out_blocks.iter().map(|b| off(&b.peak)).fold(0.0, f64::max);
    let verdict = if global.contrast_at_zero <= 0.02 {
        format!("FAIL: nuclei are not {stronger} than their surroundings at the registered position — check the transform")
    } else if off(&global) <= 2.0_f64.max(step) && worst <= 4.0_f64.max(2.0 * step) {
        format!(
            "OK: {signal} peaks within {:.1} µm of the registered position (worst block {:.1} µm)",
            off(&global),
            worst
        )
    } else {
        format!(
            "CHECK: best global offset ({:.1}, {:.1}) µm, worst block offset {:.1} µm — possible misregistration or local deformation",
            global.best_dx_um, global.best_dy_um, worst
        )
    };
    Ok(RegistrationQc {
        section_id: m.section_id.clone(),
        image_id: img.id.clone(),
        signal: signal.into(),
        registration_mode: img.registration.mode.clone(),
        sampled_level: level,
        sampled_pixel_um: px_um * (1u64 << level) as f64,
        search_radius_um: radius,
        step_um: step,
        global,
        blocks: out_blocks,
        verdict,
    })
}
