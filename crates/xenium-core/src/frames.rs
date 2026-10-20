//! Coordinate frames.
//!
//! The canonical *world* frame of an index is **XU: Xenium micrometres** (x right,
//! y down, origin at the top-left of the Xenium morphology image). Every molecular
//! layer (cells, nuclei, transcripts, grids) is stored in XU and never transformed
//! at ingest, so re-registering an image never requires re-indexing molecular data.
//!
//! Images are stored in their own native pixel frame (`*Px`) together with an
//! explicit, directional transform `matrix_xu_from_px`. Names always read
//! `<to>_from_<from>`; a transform is never applied implicitly.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A point in Xenium micrometres (XU).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct XeniumUm {
    pub x: f64,
    pub y: f64,
}

/// A point in level-0 pixels of a served image (e.g. native H&E, HN/HV).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ImagePx {
    pub x: f64,
    pub y: f64,
}

/// A point in Xenium morphology level-0 pixels (XM).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MorphologyPx {
    pub x: f64,
    pub y: f64,
}

impl XeniumUm {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl ImagePx {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl MorphologyPx {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// XM -> XU using the Xenium `pixel_size` (µm per morphology pixel).
    pub fn to_um(self, pixel_size_um: f64) -> XeniumUm {
        XeniumUm::new(self.x * pixel_size_um, self.y * pixel_size_um)
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum TransformError {
    #[error("matrix must be 3x3 with last row [0, 0, 1] (affine only), got {0:?}")]
    NotAffine(Vec<Vec<f64>>),
    #[error("matrix contains a non-finite value")]
    NonFinite,
    #[error("matrix is singular or numerically degenerate (|det| = {0:e})")]
    Singular(f64),
}

/// A 2-D affine transform `p' = A p + t`, stored row-major as
/// `[[a, b, c], [d, e, f]]` (implicit last row `[0, 0, 1]`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<Vec<f64>>", into = "Vec<Vec<f64>>")]
pub struct Affine2 {
    pub m: [[f64; 3]; 2],
}

impl Affine2 {
    pub const IDENTITY: Affine2 = Affine2 {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
    };

    pub fn new(m: [[f64; 3]; 2]) -> Result<Self, TransformError> {
        let a = Affine2 { m };
        a.validate()?;
        Ok(a)
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Affine2 {
            m: [[sx, 0.0, 0.0], [0.0, sy, 0.0]],
        }
    }

    pub fn translation(tx: f64, ty: f64) -> Self {
        Affine2 {
            m: [[1.0, 0.0, tx], [0.0, 1.0, ty]],
        }
    }

    /// Counter-clockwise rotation in a y-up frame (clockwise on screen with y down).
    pub fn rotation(theta_rad: f64) -> Self {
        let (s, c) = theta_rad.sin_cos();
        Affine2 {
            m: [[c, -s, 0.0], [s, c, 0.0]],
        }
    }

    pub fn determinant(&self) -> f64 {
        self.m[0][0] * self.m[1][1] - self.m[0][1] * self.m[1][0]
    }

    /// True when the transform mirrors (negative determinant).
    pub fn is_reflection(&self) -> bool {
        self.determinant() < 0.0
    }

    pub fn validate(&self) -> Result<(), TransformError> {
        if self.m.iter().flatten().any(|v| !v.is_finite()) {
            return Err(TransformError::NonFinite);
        }
        let det = self.determinant();
        // Relative tolerance: scale-aware so µm<->px matrices (|entries| ~ 0.2..5) pass.
        let norm =
            self.m[0][0].abs() + self.m[0][1].abs() + self.m[1][0].abs() + self.m[1][1].abs();
        if det.abs() <= 1e-12 * norm.max(1e-300) * norm.max(1e-300) || det == 0.0 {
            return Err(TransformError::Singular(det));
        }
        Ok(())
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.m;
        (
            m[0][0] * x + m[0][1] * y + m[0][2],
            m[1][0] * x + m[1][1] * y + m[1][2],
        )
    }

    /// `self ∘ other`: apply `other` first, then `self`.
    pub fn compose(&self, other: &Affine2) -> Affine2 {
        let a = &self.m;
        let b = &other.m;
        Affine2 {
            m: [
                [
                    a[0][0] * b[0][0] + a[0][1] * b[1][0],
                    a[0][0] * b[0][1] + a[0][1] * b[1][1],
                    a[0][0] * b[0][2] + a[0][1] * b[1][2] + a[0][2],
                ],
                [
                    a[1][0] * b[0][0] + a[1][1] * b[1][0],
                    a[1][0] * b[0][1] + a[1][1] * b[1][1],
                    a[1][0] * b[0][2] + a[1][1] * b[1][2] + a[1][2],
                ],
            ],
        }
    }

    pub fn inverse(&self) -> Result<Affine2, TransformError> {
        self.validate()?;
        let [[a, b, c], [d, e, f]] = self.m;
        let det = a * e - b * d;
        let ia = e / det;
        let ib = -b / det;
        let id = -d / det;
        let ie = a / det;
        Affine2::new([[ia, ib, -(ia * c + ib * f)], [id, ie, -(id * c + ie * f)]])
    }

    /// Mean isotropic scale factor, sqrt(|det|).
    pub fn mean_scale(&self) -> f64 {
        self.determinant().abs().sqrt()
    }

    /// Apply to the four corners of an axis-aligned rectangle and return the
    /// axis-aligned bounding box of the result `[min_x, min_y, max_x, max_y]`.
    pub fn transform_rect_bounds(&self, r: [f64; 4]) -> [f64; 4] {
        let pts = [
            self.apply(r[0], r[1]),
            self.apply(r[2], r[1]),
            self.apply(r[2], r[3]),
            self.apply(r[0], r[3]),
        ];
        let mut out = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for (x, y) in pts {
            out[0] = out[0].min(x);
            out[1] = out[1].min(y);
            out[2] = out[2].max(x);
            out[3] = out[3].max(y);
        }
        out
    }

    pub fn to_rows3(&self) -> Vec<Vec<f64>> {
        vec![self.m[0].to_vec(), self.m[1].to_vec(), vec![0.0, 0.0, 1.0]]
    }
}

impl TryFrom<Vec<Vec<f64>>> for Affine2 {
    type Error = TransformError;

    /// Accepts a 3x3 matrix whose last row must be `[0, 0, 1]` (within 1e-9), or a 2x3 matrix.
    fn try_from(rows: Vec<Vec<f64>>) -> Result<Self, Self::Error> {
        let ok_shape = (rows.len() == 3 || rows.len() == 2) && rows.iter().all(|r| r.len() == 3);
        if !ok_shape {
            return Err(TransformError::NotAffine(rows));
        }
        if rows.len() == 3 {
            let last = &rows[2];
            if (last[0]).abs() > 1e-9 || (last[1]).abs() > 1e-9 || (last[2] - 1.0).abs() > 1e-9 {
                return Err(TransformError::NotAffine(rows));
            }
        }
        Affine2::new([
            [rows[0][0], rows[0][1], rows[0][2]],
            [rows[1][0], rows[1][1], rows[1][2]],
        ])
    }
}

impl From<Affine2> for Vec<Vec<f64>> {
    fn from(a: Affine2) -> Self {
        a.to_rows3()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn identity_translation_scale() {
        assert!(close(Affine2::IDENTITY.apply(3.0, 4.0), (3.0, 4.0)));
        assert!(close(
            Affine2::translation(10.0, -5.0).apply(1.0, 2.0),
            (11.0, -3.0)
        ));
        assert!(close(Affine2::scale(2.0, 0.5).apply(3.0, 4.0), (6.0, 2.0)));
    }

    #[test]
    fn rotation_and_inverse_round_trip() {
        let t = Affine2::translation(120.0, -40.0)
            .compose(&Affine2::rotation(0.3))
            .compose(&Affine2::scale(0.2125, 0.2125));
        let inv = t.inverse().unwrap();
        for &(x, y) in &[(0.0, 0.0), (1000.0, 2500.5), (-3.25, 7.75)] {
            let (u, v) = t.apply(x, y);
            assert!(close(inv.apply(u, v), (x, y)));
        }
        assert!(close(t.compose(&inv).apply(42.0, 17.0), (42.0, 17.0)));
    }

    #[test]
    fn reflection_detected_and_invertible() {
        let flip = Affine2::new([[-1.0, 0.0, 500.0], [0.0, 1.0, 0.0]]).unwrap();
        assert!(flip.is_reflection());
        assert!(close(flip.apply(100.0, 3.0), (400.0, 3.0)));
        assert!(close(
            flip.inverse().unwrap().apply(400.0, 3.0),
            (100.0, 3.0)
        ));
    }

    #[test]
    fn compose_order_is_right_to_left() {
        let s = Affine2::scale(2.0, 2.0);
        let t = Affine2::translation(1.0, 0.0);
        // s ∘ t: translate first, then scale.
        assert!(close(s.compose(&t).apply(0.0, 0.0), (2.0, 0.0)));
        assert!(close(t.compose(&s).apply(0.0, 0.0), (1.0, 0.0)));
    }

    #[test]
    fn rejects_singular_and_projective() {
        assert!(matches!(
            Affine2::new([[1.0, 2.0, 0.0], [2.0, 4.0, 0.0]]),
            Err(TransformError::Singular(_))
        ));
        let projective = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.001, 0.0, 1.0],
        ];
        assert!(matches!(
            Affine2::try_from(projective),
            Err(TransformError::NotAffine(_))
        ));
        assert!(matches!(
            Affine2::new([[f64::NAN, 0.0, 0.0], [0.0, 1.0, 0.0]]),
            Err(TransformError::NonFinite)
        ));
    }

    #[test]
    fn serde_round_trip_uses_3x3_rows() {
        let a = Affine2::new([[0.5, 0.1, 3.0], [-0.1, 0.5, 4.0]]).unwrap();
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, "[[0.5,0.1,3.0],[-0.1,0.5,4.0],[0.0,0.0,1.0]]");
        let back: Affine2 = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn rect_bounds_under_rotation() {
        let r = Affine2::rotation(std::f64::consts::FRAC_PI_2);
        let b = r.transform_rect_bounds([0.0, 0.0, 2.0, 1.0]);
        assert!((b[0] + 1.0).abs() < 1e-9 && b[1].abs() < 1e-9);
        assert!(b[2].abs() < 1e-9 && (b[3] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn morphology_px_to_um() {
        let p = MorphologyPx::new(100.0, 200.0).to_um(0.2125);
        assert!((p.x - 21.25).abs() < 1e-12 && (p.y - 42.5).abs() < 1e-12);
    }
}
