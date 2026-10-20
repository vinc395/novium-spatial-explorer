//! Streaming reader for `transcripts.parquet`. Batches are handed to a callback
//! and dropped; the reader never materialises the whole table.

use crate::columns::{as_f64, as_u8, as_utf8, column, is_integer, open_projected, parquet_schema};
use anyhow::{bail, Result};
use arrow::array::ArrayRef;
use arrow::compute::cast;
use arrow::datatypes::DataType;
use serde::Serialize;
use std::path::Path;

/// Columns actually available in this file (they vary across XOA versions).
#[derive(Debug, Clone, Serialize, Default)]
pub struct TranscriptColumns {
    pub feature_column: String,
    pub has_z: bool,
    pub has_qv: bool,
    pub has_cell_id: bool,
    pub cell_id_integer: bool,
    pub has_overlaps_nucleus: bool,
    pub has_codeword_category: bool,
    pub has_is_gene: bool,
    pub total_rows: u64,
}

/// One batch, with every column already cast to a fixed type:
/// text -> Utf8, coordinates/qv -> Float64, overlaps_nucleus -> UInt8, is_gene -> Boolean.
pub struct TranscriptBatch {
    pub len: usize,
    pub feature: ArrayRef,
    pub x: ArrayRef,
    pub y: ArrayRef,
    pub z: Option<ArrayRef>,
    pub qv: Option<ArrayRef>,
    pub cell_id: Option<ArrayRef>,
    pub overlaps_nucleus: Option<ArrayRef>,
    pub codeword_category: Option<ArrayRef>,
    pub is_gene: Option<ArrayRef>,
}

/// Sentinels used by XOA for transcripts not assigned to a cell.
pub fn is_unassigned_cell_id(id: &str, integer_ids: bool) -> bool {
    id.is_empty() || id == "UNASSIGNED" || (integer_ids && id == "-1")
}

pub fn detect_columns(path: &Path) -> Result<TranscriptColumns> {
    let schema = parquet_schema(path)?;
    let find = |n: &str| schema.iter().find(|(c, _)| c == n).map(|(_, t)| t.clone());
    let feature_column = if find("feature_name").is_some() {
        "feature_name"
    } else if find("gene").is_some() {
        "gene"
    } else {
        bail!("{}: no feature_name column", path.display())
    };
    for req in ["x_location", "y_location"] {
        if find(req).is_none() {
            bail!("{}: required column {req:?} not found", path.display());
        }
    }
    Ok(TranscriptColumns {
        feature_column: feature_column.to_string(),
        has_z: find("z_location").is_some(),
        has_qv: find("qv").is_some(),
        has_cell_id: find("cell_id").is_some(),
        cell_id_integer: find("cell_id").is_some_and(|t| is_integer(&t)),
        has_overlaps_nucleus: find("overlaps_nucleus").is_some(),
        has_codeword_category: find("codeword_category").is_some(),
        has_is_gene: find("is_gene").is_some(),
        total_rows: 0,
    })
}

pub fn stream_transcripts(
    path: &Path,
    mut f: impl FnMut(&TranscriptColumns, &TranscriptBatch) -> Result<()>,
) -> Result<TranscriptColumns> {
    let mut cols = detect_columns(path)?;
    let wanted = [
        cols.feature_column.as_str(),
        "x_location",
        "y_location",
        "z_location",
        "qv",
        "cell_id",
        "overlaps_nucleus",
        "codeword_category",
        "is_gene",
    ];
    let (reader, rows, _) = open_projected(path, &wanted)?;
    cols.total_rows = rows;
    for batch in reader {
        let batch = batch?;
        let opt = |name: &str, on: bool| -> Result<Option<&ArrayRef>> {
            if on {
                Ok(Some(column(&batch, name)?))
            } else {
                Ok(None)
            }
        };
        let tb = TranscriptBatch {
            len: batch.num_rows(),
            feature: as_utf8(column(&batch, &cols.feature_column)?)?,
            x: as_f64(column(&batch, "x_location")?)?,
            y: as_f64(column(&batch, "y_location")?)?,
            z: opt("z_location", cols.has_z)?.map(as_f64).transpose()?,
            qv: opt("qv", cols.has_qv)?.map(as_f64).transpose()?,
            cell_id: opt("cell_id", cols.has_cell_id)?.map(as_utf8).transpose()?,
            overlaps_nucleus: opt("overlaps_nucleus", cols.has_overlaps_nucleus)?
                .map(as_u8)
                .transpose()?,
            codeword_category: opt("codeword_category", cols.has_codeword_category)?
                .map(as_utf8)
                .transpose()?,
            is_gene: opt("is_gene", cols.has_is_gene)?
                .map(|a| cast(a, &DataType::Boolean))
                .transpose()?,
        };
        f(&cols, &tb)?;
    }
    Ok(cols)
}
