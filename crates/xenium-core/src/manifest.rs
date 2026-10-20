//! The versioned per-section index manifest (`manifest.json`).
//!
//! The manifest is the single registry the server uses to resolve section IDs,
//! fields, genes, images and grids. Nothing in an HTTP request is ever used as a
//! filesystem path directly; it is looked up here first.

use crate::features::FeatureCategory;
use crate::grid::TileGrid;
use crate::registration::ResolvedRegistration;
use serde::{Deserialize, Serialize};

/// Written by this build. 2 added `images[].tile_layout` and `cells.field_layout`
/// (packed tiles and fields), which version-1 readers would ignore and then miss
/// every tile and field.
pub const SCHEMA_VERSION: u32 = 2;
/// Oldest schema this build still reads (version 1 = one file per tile and field).
pub const MIN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionManifest {
    pub schema_version: u32,
    pub section_id: String,
    pub display_name: String,
    /// `xenium-web <crate version>`.
    pub created_by: String,
    pub created_at_unix: u64,
    /// Always `"xenium_um"`: every vector layer is stored in Xenium micrometres.
    pub world_frame: String,
    /// `[min_x, min_y, max_x, max_y]` in µm over all indexed layers.
    pub bounds_um: [f64; 4],
    pub grid: TileGrid,
    pub xenium: XeniumInfo,
    pub sources: Vec<SourceFingerprint>,
    pub cells: Option<CellsInfo>,
    pub transcripts: Option<TranscriptsInfo>,
    pub images: Vec<ImageInfo>,
    pub grids: Vec<GridInfo>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct XeniumInfo {
    /// µm per morphology pixel from `experiment.xenium` (`pixel_size`).
    pub pixel_size_um: Option<f64>,
    pub analysis_sw_version: Option<String>,
    pub panel_name: Option<String>,
    pub panel_num_targets: Option<u64>,
    pub num_cells_reported: Option<u64>,
    pub transcripts_reported: Option<u64>,
}

/// Cheap reproducible fingerprint of a source file (basename only; no absolute paths).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceFingerprint {
    pub role: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub modified_unix: u64,
    /// SHA-256 over the first and last MiB plus the size.
    pub sha256_head_tail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellsInfo {
    pub count: u64,
    pub boundaries: Option<ShapeLayerInfo>,
    pub nuclei: Option<ShapeLayerInfo>,
    pub fields: Vec<FieldInfo>,
    /// How the field columns are stored; absent (one file per field) in schema 1.
    #[serde(default, skip_serializing_if = "Layout::is_files")]
    pub field_layout: Layout,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeLayerInfo {
    pub polygons: u64,
    pub vertices: u64,
    /// Polygons are assigned to the chunk containing their owning cell's centroid.
    /// A viewport query must expand its rectangle by this margin (max distance from a
    /// centroid to any vertex of its polygons) to include every intersecting polygon.
    pub query_margin_um: f64,
    /// `[tx, ty, n_polygons]` for every non-empty chunk.
    pub chunks: Vec<[i64; 3]>,
    pub skipped_polygons: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Numeric,
    Categorical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryInfo {
    pub name: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NumericStats {
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub mean: Option<f64>,
    pub p01: Option<f64>,
    pub p99: Option<f64>,
    pub n_valid: u64,
    pub n_missing: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldInfo {
    /// Stable identifier used in URLs; `[A-Za-z0-9_.:-]+` only.
    pub id: String,
    pub label: String,
    pub kind: FieldKind,
    /// e.g. `cells.parquet`, `annotation`, `lr_score`.
    pub group: String,
    pub source_file: String,
    pub stats: Option<NumericStats>,
    pub categories: Option<Vec<CategoryInfo>>,
    /// Free-form provenance (method, normalization, analysis_run_id, pair, role...).
    pub provenance: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneInfo {
    pub id: u32,
    pub name: String,
    pub category: FeatureCategory,
    pub count: u64,
    pub count_qv20: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptsInfo {
    pub rows_read: u64,
    pub indexed: u64,
    pub skipped_invalid: u64,
    pub unassigned: u64,
    pub unknown_cell_ids: u64,
    /// `qv` is stored as `floor(qv * qv_scale)`.
    pub qv_scale: f64,
    pub default_qv_min: f64,
    pub z_available: bool,
    pub genes: Vec<GeneInfo>,
    /// Density tiles exist for levels `0..=density_max_level` (bin = 2^level µm).
    pub density_max_level: u32,
    pub density_tile_bins: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageKind {
    Rgb,
    Gray,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageInfo {
    /// `[A-Za-z0-9_-]+`; used in URLs.
    pub id: String,
    pub label: String,
    pub kind: ImageKind,
    pub width: u32,
    pub height: u32,
    pub tile_size: u32,
    /// Level 0 is full resolution; level `l` is downsampled by `2^l`.
    pub levels: u32,
    pub format: String,
    pub registration: ResolvedRegistration,
    /// Axis-aligned bounds of the transformed image in µm.
    pub bounds_um: [f64; 4],
    pub source_file: String,
    pub notes: Vec<String>,
    /// Set for fluorescence morphology channels (absent for H&E). Optional and
    /// additive: indexes without it read unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelInfo>,
    /// How the tiles are stored on disk; absent (one file per tile) in schema 1.
    #[serde(default, skip_serializing_if = "Layout::is_files")]
    pub tile_layout: Layout,
}

/// How the files of a set of keyed blobs (an image's JPEG tiles, the cell fields) are
/// stored. URLs and decoded data are the same for both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// One file per blob: `images/<id>/<level>/<x>_<y>.jpg`, `cells/fields/<id>.bin`.
    #[default]
    Files,
    /// One pack (XIX1 index) for the set: `images/<id>/tiles.{pack,idx}` keyed by
    /// `level << 26 | y << 13 | x`, `cells/fields.{pack,idx}` keyed by the field's
    /// position in `cells.fields`. A section then has a few dozen files instead of
    /// tens of thousands, which spares the file system's metadata.
    Packed,
}

impl Layout {
    pub fn is_files(&self) -> bool {
        *self == Layout::Files
    }
}

/// A fluorescence channel stored as an 8-bit pyramid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelInfo {
    /// Channel name from the OME metadata, e.g. "DAPI".
    pub name: String,
    /// Raw intensity window mapped linearly to 0..255 at ingest.
    pub window: [u32; 2],
    /// Suggested display colour (sRGB).
    pub color: [u8; 3],
}

/// Display colour for a morphology channel name (Xenium multimodal stains first).
pub fn default_channel_color(name: &str, index: usize) -> [u8; 3] {
    let n = name.to_lowercase();
    if n.contains("dapi") {
        [70, 130, 255]
    } else if n.contains("atp1a1")
        || n.contains("cadherin")
        || n.contains("cd45")
        || n.contains("boundary")
    {
        [60, 220, 90]
    } else if n.contains("18s") || n.contains("rna") {
        [255, 180, 0]
    } else if n.contains("sma") || n.contains("vimentin") {
        [255, 60, 170]
    } else {
        [
            [0, 220, 220],
            [230, 230, 60],
            [255, 120, 40],
            [180, 120, 255],
        ][index % 4]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridInfo {
    pub id: String,
    pub label: String,
    /// µm position of the *corner* of grid cell (0, 0).
    pub origin_um: [f64; 2],
    pub spacing_um: f64,
    pub width: u32,
    pub height: u32,
    pub fields: Vec<FieldInfo>,
    pub source_file: String,
    /// Rendering rule; always `nearest` (each value is drawn as its grid square).
    pub resampling: String,
}

pub fn is_safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
        && !s.starts_with('.')
        && !s.contains("..")
}

impl SectionManifest {
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&self.schema_version) {
            return Err(format!(
                "unsupported schema_version {} (this build reads {MIN_SCHEMA_VERSION}-{SCHEMA_VERSION})",
                self.schema_version
            ));
        }
        if self.schema_version < 2
            && (self.images.iter().any(|i| !i.tile_layout.is_files())
                || self
                    .cells
                    .as_ref()
                    .is_some_and(|c| !c.field_layout.is_files()))
        {
            return Err("packed tiles or fields need schema_version 2".into());
        }
        if !is_safe_id(&self.section_id) {
            return Err(format!("unsafe section_id {:?}", self.section_id));
        }
        if self.world_frame != "xenium_um" {
            return Err(format!("unsupported world_frame {:?}", self.world_frame));
        }
        let ids = self
            .cells
            .iter()
            .flat_map(|c| c.fields.iter().map(|f| f.id.as_str()))
            .chain(self.images.iter().map(|i| i.id.as_str()))
            .chain(self.grids.iter().map(|g| g.id.as_str()))
            .chain(
                self.grids
                    .iter()
                    .flat_map(|g| g.fields.iter().map(|f| f.id.as_str())),
            );
        for id in ids {
            if !is_safe_id(id) {
                return Err(format!("unsafe id {id:?}"));
            }
        }
        if let Some(t) = &self.transcripts {
            for (i, g) in t.genes.iter().enumerate() {
                if g.id as usize != i {
                    return Err("gene ids must be dense 0..n".into());
                }
            }
        }
        Ok(())
    }

    pub fn field(&self, id: &str) -> Option<&FieldInfo> {
        self.cells.as_ref()?.fields.iter().find(|f| f.id == id)
    }

    pub fn image(&self, id: &str) -> Option<&ImageInfo> {
        self.images.iter().find(|i| i.id == id)
    }

    pub fn grid(&self, id: &str) -> Option<&GridInfo> {
        self.grids.iter().find(|g| g.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_ids() {
        assert!(is_safe_id("SECTION_001"));
        assert!(is_safe_id("lr:CXCL8_CXCR2:receiver"));
        assert!(!is_safe_id("../etc"));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id(".hidden"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id("a..b"));
        assert!(!is_safe_id("sp ace"));
    }
}
