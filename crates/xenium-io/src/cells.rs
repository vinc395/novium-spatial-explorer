//! `cells.parquet`: cell IDs, centroids (µm) and per-cell summary columns.

use crate::columns::{
    as_f64, as_utf8, column, f64_at, is_integer, open_projected, parquet_schema, str_at,
};
use anyhow::{bail, Result};
use arrow::datatypes::DataType;
use std::collections::HashSet;
use std::path::Path;

/// Text columns with more distinct values than this are treated as identifiers and skipped.
pub const MAX_CATEGORIES: usize = 2000;

#[derive(Debug, Default)]
pub struct CellsTable {
    /// Original IDs in file order.
    pub ids: Vec<String>,
    /// `[x, y]` µm in file order.
    pub centroids: Vec<[f64; 2]>,
    pub numeric: Vec<(String, Vec<f64>)>,
    pub categorical: Vec<(String, Vec<Option<String>>)>,
    pub id_was_integer: bool,
    /// Rows dropped because the ID was missing or the centroid was not finite.
    pub dropped_rows: u64,
    pub skipped_columns: Vec<String>,
}

pub fn read_cells(path: &Path) -> Result<CellsTable> {
    let schema = parquet_schema(path)?;
    let has = |n: &str| schema.iter().any(|(c, _)| c == n);
    for req in ["cell_id", "x_centroid", "y_centroid"] {
        if !has(req) {
            bail!("{}: required column {req:?} not found", path.display());
        }
    }
    let mut t = CellsTable {
        id_was_integer: schema
            .iter()
            .any(|(c, dt)| c == "cell_id" && is_integer(dt)),
        ..Default::default()
    };
    let mut numeric_names = vec![];
    let mut text_names = vec![];
    for (name, dt) in &schema {
        if matches!(name.as_str(), "cell_id" | "x_centroid" | "y_centroid") {
            continue;
        }
        if dt.is_numeric() {
            numeric_names.push(name.clone());
        } else if matches!(
            dt,
            DataType::Utf8
                | DataType::LargeUtf8
                | DataType::Utf8View
                | DataType::Dictionary(_, _)
                | DataType::Boolean
        ) {
            text_names.push(name.clone());
        } else {
            t.skipped_columns.push(format!("{name} ({dt})"));
        }
    }
    let mut wanted: Vec<&str> = vec!["cell_id", "x_centroid", "y_centroid"];
    wanted.extend(numeric_names.iter().map(String::as_str));
    wanted.extend(text_names.iter().map(String::as_str));
    let (reader, rows, _) = open_projected(path, &wanted)?;
    let mut numeric: Vec<Vec<f64>> = numeric_names
        .iter()
        .map(|_| Vec::with_capacity(rows as usize))
        .collect();
    let mut text: Vec<Vec<Option<String>>> = text_names
        .iter()
        .map(|_| Vec::with_capacity(rows as usize))
        .collect();
    for batch in reader {
        let batch = batch?;
        let ids = as_utf8(column(&batch, "cell_id")?)?;
        let xs = as_f64(column(&batch, "x_centroid")?)?;
        let ys = as_f64(column(&batch, "y_centroid")?)?;
        let ncols: Vec<_> = numeric_names
            .iter()
            .map(|n| as_f64(column(&batch, n)?))
            .collect::<Result<_>>()?;
        let tcols: Vec<_> = text_names
            .iter()
            .map(|n| as_utf8(column(&batch, n)?))
            .collect::<Result<_>>()?;
        for i in 0..batch.num_rows() {
            let (x, y) = (f64_at(&xs, i), f64_at(&ys, i));
            let id = str_at(&ids, i).filter(|s| !s.is_empty());
            let Some(id) = id.filter(|_| x.is_finite() && y.is_finite()) else {
                t.dropped_rows += 1;
                continue;
            };
            t.ids.push(id.to_string());
            t.centroids.push([x, y]);
            for (c, col) in ncols.iter().enumerate() {
                numeric[c].push(f64_at(col, i));
            }
            for (c, col) in tcols.iter().enumerate() {
                text[c].push(str_at(col, i).map(str::to_string));
            }
        }
    }
    t.numeric = numeric_names.into_iter().zip(numeric).collect();
    for (name, vals) in text_names.into_iter().zip(text) {
        let distinct: HashSet<&str> = vals.iter().flatten().map(String::as_str).collect();
        if distinct.len() <= MAX_CATEGORIES {
            t.categorical.push((name, vals));
        } else {
            t.skipped_columns.push(format!(
                "{name} ({} distinct values; treated as an identifier)",
                distinct.len()
            ));
        }
    }
    Ok(t)
}
