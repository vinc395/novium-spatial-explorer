//! Image registration inputs, resolved to one explicit `matrix_xu_from_px`.
//!
//! Every supported input is a *named* adapter with a documented direction and unit
//! convention. Nothing is guessed: ambiguous or unsupported inputs return an error.

use crate::frames::{Affine2, TransformError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistrationError {
    #[error(transparent)]
    Transform(#[from] TransformError),
    #[error("registration JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("alignment CSV is invalid: {0}")]
    Csv(String),
    #[error("pixel size must be finite and > 0, got {0}")]
    BadPixelSize(f64),
    #[error("registration mode '{0}' is recognised but not implemented; refusing to fall back to an affine approximation")]
    Unsupported(String),
}

/// Direction of a user-supplied affine matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffineDirection {
    /// Matrix maps image level-0 pixels -> Xenium µm.
    XuFromPx,
    /// Matrix maps Xenium µm -> image level-0 pixels (it will be inverted explicitly).
    PxFromXu,
}

/// Output unit of a 10x Xenium Explorer `*_imagealignment.csv` matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentOutputUnits {
    /// The matrix maps image px -> Xenium morphology level-0 px (10x / spatialdata-io convention).
    XeniumMorphologyPx,
    /// The matrix maps image px -> Xenium µm.
    Um,
}

/// A registration specification as stored in a `registration.json` file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RegistrationSpec {
    /// The image is already in the Xenium frame (same origin, no rotation); only a
    /// pixel size is needed. Example: the Xenium morphology image itself, or an H&E
    /// that was warped onto the morphology pixel grid.
    AlreadyAligned { pixel_size_um: f64 },
    /// An explicit affine matrix with a declared direction.
    Affine {
        direction: AffineDirection,
        matrix: Vec<Vec<f64>>,
    },
    /// A 10x Xenium Explorer alignment matrix (image px -> morphology px by default).
    XeniumExplorerAlignment {
        matrix: Vec<Vec<f64>>,
        output_units: AlignmentOutputUnits,
    },
    /// Affine + local displacement field. Recognised so that it fails loudly.
    DisplacementField {},
}

/// The result used by the rest of the system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedRegistration {
    pub mode: String,
    /// Image level-0 pixels -> Xenium µm.
    pub matrix_xu_from_px: Affine2,
    /// Approximate isotropic µm per image pixel (sqrt |det|), for display only.
    pub approx_pixel_size_um: f64,
    pub is_reflection: bool,
    pub provenance: String,
}

impl RegistrationSpec {
    pub fn from_json(text: &str) -> Result<Self, RegistrationError> {
        Ok(serde_json::from_str(text)?)
    }

    /// Resolve to `matrix_xu_from_px`. `xenium_pixel_size_um` is the morphology pixel
    /// size from `experiment.xenium` and is only used by the 10x alignment adapter.
    pub fn resolve(
        &self,
        xenium_pixel_size_um: Option<f64>,
        provenance: impl Into<String>,
    ) -> Result<ResolvedRegistration, RegistrationError> {
        let (mode, m) = match self {
            RegistrationSpec::AlreadyAligned { pixel_size_um } => {
                check_pixel_size(*pixel_size_um)?;
                (
                    "already_aligned",
                    Affine2::scale(*pixel_size_um, *pixel_size_um),
                )
            }
            RegistrationSpec::Affine { direction, matrix } => {
                let a = Affine2::try_from(matrix.clone())?;
                match direction {
                    AffineDirection::XuFromPx => ("affine_xu_from_px", a),
                    AffineDirection::PxFromXu => ("affine_px_from_xu(inverted)", a.inverse()?),
                }
            }
            RegistrationSpec::XeniumExplorerAlignment {
                matrix,
                output_units,
            } => {
                let a = Affine2::try_from(matrix.clone())?;
                match output_units {
                    AlignmentOutputUnits::Um => ("xenium_explorer_alignment(um)", a),
                    AlignmentOutputUnits::XeniumMorphologyPx => {
                        let ps = xenium_pixel_size_um.ok_or_else(|| {
                            RegistrationError::Csv(
                                "morphology-pixel alignment needs the Xenium pixel_size from experiment.xenium".into(),
                            )
                        })?;
                        check_pixel_size(ps)?;
                        // XU <- XM (scale by pixel size) <- image px (alignment matrix)
                        (
                            "xenium_explorer_alignment(morphology_px)",
                            Affine2::scale(ps, ps).compose(&a),
                        )
                    }
                }
            }
            RegistrationSpec::DisplacementField {} => {
                return Err(RegistrationError::Unsupported("displacement_field".into()))
            }
        };
        m.validate()?;
        Ok(ResolvedRegistration {
            mode: mode.to_string(),
            approx_pixel_size_um: m.mean_scale(),
            is_reflection: m.is_reflection(),
            matrix_xu_from_px: m,
            provenance: provenance.into(),
        })
    }
}

fn check_pixel_size(ps: f64) -> Result<(), RegistrationError> {
    if !(ps.is_finite() && ps > 0.0) {
        return Err(RegistrationError::BadPixelSize(ps));
    }
    Ok(())
}

/// Parse a 10x Xenium Explorer `*_imagealignment.csv`: three lines of three numbers.
/// Accepts commas or whitespace, optional trailing newline; rejects headers/extra rows.
pub fn parse_alignment_csv(text: &str) -> Result<Vec<Vec<f64>>, RegistrationError> {
    let rows: Vec<Vec<f64>> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.trim_matches('"')
                        .parse::<f64>()
                        .map_err(|_| RegistrationError::Csv(format!("not a number: {s:?}")))
                })
                .collect::<Result<Vec<f64>, _>>()
        })
        .collect::<Result<_, _>>()?;
    if rows.len() != 3 || rows.iter().any(|r| r.len() != 3) {
        return Err(RegistrationError::Csv(format!(
            "expected a 3x3 matrix, got {} rows",
            rows.len()
        )));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_aligned_is_pure_scale() {
        let r = RegistrationSpec::from_json(r#"{"mode":"already_aligned","pixel_size_um":0.2125}"#)
            .unwrap()
            .resolve(None, "test")
            .unwrap();
        assert_eq!(r.matrix_xu_from_px.apply(100.0, 40.0), (21.25, 8.5));
        assert!(!r.is_reflection);
    }

    #[test]
    fn px_from_xu_is_inverted_once() {
        let spec = RegistrationSpec::Affine {
            direction: AffineDirection::PxFromXu,
            matrix: vec![
                vec![2.0, 0.0, 100.0],
                vec![0.0, 2.0, 50.0],
                vec![0.0, 0.0, 1.0],
            ],
        };
        let r = spec.resolve(None, "test").unwrap();
        // px = 2*um + t  =>  um = (px - t)/2
        let (x, y) = r.matrix_xu_from_px.apply(300.0, 250.0);
        assert!((x - 100.0).abs() < 1e-12 && (y - 100.0).abs() < 1e-12);
        assert!((r.approx_pixel_size_um - 0.5).abs() < 1e-12);
    }

    #[test]
    fn alignment_csv_morphology_px_is_scaled_by_xenium_pixel_size() {
        let rows = parse_alignment_csv("1,0,10\n0,1,20\n0,0,1\n").unwrap();
        let spec = RegistrationSpec::XeniumExplorerAlignment {
            matrix: rows,
            output_units: AlignmentOutputUnits::XeniumMorphologyPx,
        };
        let r = spec.resolve(Some(0.2125), "csv").unwrap();
        let (x, y) = r.matrix_xu_from_px.apply(0.0, 0.0);
        assert!((x - 2.125).abs() < 1e-12 && (y - 4.25).abs() < 1e-12);
        // Without the Xenium pixel size the adapter must refuse rather than guess.
        assert!(spec.resolve(None, "csv").is_err());
    }

    #[test]
    fn alignment_csv_parser_rejects_bad_shapes() {
        assert!(parse_alignment_csv("1,0,0\n0,1,0\n").is_err());
        assert!(parse_alignment_csv("a,b,c\n0,1,0\n0,0,1").is_err());
        assert!(parse_alignment_csv("1 0 0\n0 1 0\n0 0 1").is_ok());
        assert!(parse_alignment_csv("1.5e-1,0,0\r\n0,1,0\r\n0,0,1\r\n").is_ok());
    }

    #[test]
    fn displacement_field_fails_loudly() {
        let spec = RegistrationSpec::from_json(r#"{"mode":"displacement_field"}"#).unwrap();
        assert!(matches!(
            spec.resolve(None, "x"),
            Err(RegistrationError::Unsupported(_))
        ));
    }

    #[test]
    fn mirrored_registration_flagged() {
        let spec = RegistrationSpec::Affine {
            direction: AffineDirection::XuFromPx,
            matrix: vec![vec![-0.5, 0.0, 1000.0], vec![0.0, 0.5, 0.0]],
        };
        assert!(spec.resolve(None, "x").unwrap().is_reflection);
    }
}
