//! The spatial tile grid in Xenium µm, shared by the indexer, server and browser.
//!
//! Tile `(tx, ty)` covers `[tx*s, (tx+1)*s) x [ty*s, (ty+1)*s)` µm with the grid
//! anchored at the XU origin. Tile coordinates are signed so negative coordinates
//! are representable; keys pack them with a +32768 bias.

use serde::{Deserialize, Serialize};

pub const DEFAULT_TILE_SIZE_UM: f64 = 256.0;
const BIAS: i32 = 32768;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TileGrid {
    pub tile_size_um: f64,
}

impl Default for TileGrid {
    fn default() -> Self {
        Self {
            tile_size_um: DEFAULT_TILE_SIZE_UM,
        }
    }
}

impl TileGrid {
    pub fn new(tile_size_um: f64) -> Self {
        assert!(tile_size_um.is_finite() && tile_size_um > 0.0);
        Self { tile_size_um }
    }

    /// Tile containing a point. Returns `None` for non-finite or out-of-range input.
    pub fn tile_of(&self, x: f64, y: f64) -> Option<(i32, i32)> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let tx = (x / self.tile_size_um).floor();
        let ty = (y / self.tile_size_um).floor();
        let lim = (BIAS - 1) as f64;
        if tx.abs() > lim || ty.abs() > lim {
            return None;
        }
        Some((tx as i32, ty as i32))
    }

    pub fn tile_rect(&self, tx: i32, ty: i32) -> [f64; 4] {
        let s = self.tile_size_um;
        [
            tx as f64 * s,
            ty as f64 * s,
            (tx + 1) as f64 * s,
            (ty + 1) as f64 * s,
        ]
    }

    /// Inclusive tile ranges intersecting `[min_x, min_y, max_x, max_y]`.
    pub fn tiles_in_rect(&self, r: [f64; 4]) -> Option<(i32, i32, i32, i32)> {
        let (x0, y0) = self.tile_of(r[0], r[1])?;
        let (x1, y1) = self.tile_of(r[2], r[3])?;
        Some((x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)))
    }
}

/// Pack a tile coordinate into a row-major-sortable `u32` key. Coordinates must be
/// within `-32768..=32767` (guaranteed by [`TileGrid::tile_of`]; HTTP inputs are
/// range-checked before use). Out-of-range input saturates instead of aliasing.
pub fn tile_key(tx: i32, ty: i32) -> u32 {
    let c = |v: i32| (v.clamp(-BIAS, BIAS - 1) + BIAS) as u32;
    (c(ty) << 16) | c(tx)
}

pub fn tile_from_key(key: u32) -> (i32, i32) {
    (((key & 0xFFFF) as i32) - BIAS, ((key >> 16) as i32) - BIAS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_assignment_and_keys() {
        let g = TileGrid::new(256.0);
        assert_eq!(g.tile_of(0.0, 0.0), Some((0, 0)));
        assert_eq!(g.tile_of(255.999, 256.0), Some((0, 1)));
        assert_eq!(g.tile_of(-0.001, 10.0), Some((-1, 0)));
        assert_eq!(g.tile_of(f64::NAN, 0.0), None);
        for &(x, y) in &[(0, 0), (-1, 5), (300, -2), (-32767, 32767)] {
            assert_eq!(tile_from_key(tile_key(x, y)), (x, y));
        }
        // Row-major order: (5, 0) < (0, 1)
        assert!(tile_key(5, 0) < tile_key(0, 1));
        assert!(tile_key(-1, 0) < tile_key(0, 0));
    }

    #[test]
    fn rect_tiles() {
        let g = TileGrid::new(100.0);
        assert_eq!(
            g.tiles_in_rect([50.0, 50.0, 250.0, 120.0]),
            Some((0, 0, 2, 1))
        );
        assert_eq!(g.tile_rect(1, 2), [100.0, 200.0, 200.0, 300.0]);
    }
}
