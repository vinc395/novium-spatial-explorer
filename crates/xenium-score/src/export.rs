//! Parquet export of stored UCell results. Columns follow R's naming
//! (`<signature>_UCell`); the file metadata key `xenium_web.ucell` holds every
//! result's metadata (signature genes, parameters, coverage, algorithm, index version).

use crate::cache::{Level, ResultMeta, Values};
use anyhow::{bail, Result};
use arrow::array::{ArrayRef, Float32Array, Float64Array, StringArray, UInt32Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use std::collections::HashSet;
use std::sync::Arc;
use xenium_index::SectionStore;

pub const METADATA_KEY: &str = "xenium_web.ucell";

/// Unique `<name>_UCell` column names (a repeated name gets its result key appended).
fn column_names(results: &[(ResultMeta, Values)]) -> Vec<String> {
    let mut seen = HashSet::new();
    results
        .iter()
        .map(|(m, _)| {
            let base = format!("{}_UCell", m.signature.name);
            if seen.insert(base.clone()) {
                base
            } else {
                format!("{base}_{}", m.key)
            }
        })
        .collect()
}

fn write(
    schema: Arc<Schema>,
    cols: Vec<ArrayRef>,
    results: &[(ResultMeta, Values)],
) -> Result<Vec<u8>> {
    let batch = RecordBatch::try_new(schema.clone(), cols)?;
    let metas: Vec<&ResultMeta> = results.iter().map(|r| &r.0).collect();
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3)?))
        .set_key_value_metadata(Some(vec![KeyValue::new(
            METADATA_KEY.to_string(),
            serde_json::to_string(&metas)?,
        )]))
        .build();
    let mut buf = vec![];
    let mut w = ArrowWriter::try_new(&mut buf, schema, Some(props))?;
    w.write(&batch)?;
    w.close()?;
    Ok(buf)
}

/// One row per cell: section_id, cell_id, cell_key, x_um, y_um, scores...
pub fn cells_parquet(store: &SectionStore, results: &[(ResultMeta, Values)]) -> Result<Vec<u8>> {
    if results.is_empty() || results.iter().any(|(m, _)| m.level != Level::Cell) {
        bail!("cell export needs one or more cell-level results");
    }
    let n = store.n_cells();
    let dict = store.cell_dictionary();
    let keys: Vec<u32> = (1..=n as u32).collect();
    let mut fields = vec![
        Field::new("section_id", DataType::Utf8, false),
        Field::new("cell_id", DataType::Utf8, false),
        Field::new("cell_key", DataType::UInt32, false),
        Field::new("x_um", DataType::Float32, false),
        Field::new("y_um", DataType::Float32, false),
    ];
    let mut cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(vec![
            store.manifest.section_id.as_str();
            n
        ])),
        Arc::new(StringArray::from(
            keys.iter()
                .map(|&k| dict.id_of(k).unwrap_or(""))
                .collect::<Vec<_>>(),
        )),
        Arc::new(UInt32Array::from(keys.clone())),
        Arc::new(Float32Array::from(
            keys.iter()
                .map(|&k| store.centroid(k).unwrap()[0])
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float32Array::from(
            keys.iter()
                .map(|&k| store.centroid(k).unwrap()[1])
                .collect::<Vec<_>>(),
        )),
    ];
    for (name, (m, v)) in column_names(results).into_iter().zip(results) {
        if v.scores.len() != n || m.n_units != n {
            bail!("result {} does not match the section's cells", m.key);
        }
        fields.push(Field::new(name, DataType::Float64, true));
        cols.push(Arc::new(Float64Array::from(
            v.scores
                .iter()
                .map(|x| x.is_finite().then_some(*x))
                .collect::<Vec<_>>(),
        )));
    }
    write(Arc::new(Schema::new(fields)), cols, results)
}

/// One row per bin with at least one transcript: bin column/row, corner (µm), size,
/// transcripts, scores... All results must share one bin grid.
pub fn bins_parquet(section_id: &str, results: &[(ResultMeta, Values)]) -> Result<Vec<u8>> {
    let Some(grid) = results.first().and_then(|r| r.0.grid) else {
        bail!("bin export needs one or more bin-level results");
    };
    if results
        .iter()
        .any(|(m, _)| m.grid != Some(grid) || !matches!(m.level, Level::Bin { .. }))
    {
        bail!("bin results to export together must share one bin grid");
    }
    let ntx = results[0].1.n_transcripts.clone().unwrap_or_default();
    let rows: Vec<usize> = (0..grid.len())
        .filter(|&i| ntx.get(i).copied().unwrap_or(0) > 0)
        .collect();
    let w = grid.width as usize;
    let mut fields = vec![
        Field::new("section_id", DataType::Utf8, false),
        Field::new("bin_col", DataType::UInt32, false),
        Field::new("bin_row", DataType::UInt32, false),
        Field::new("x0_um", DataType::Float64, false),
        Field::new("y0_um", DataType::Float64, false),
        Field::new("bin_um", DataType::Float64, false),
        Field::new("n_transcripts", DataType::UInt32, false),
    ];
    let mut cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(vec![section_id; rows.len()])),
        Arc::new(UInt32Array::from(
            rows.iter().map(|&i| (i % w) as u32).collect::<Vec<_>>(),
        )),
        Arc::new(UInt32Array::from(
            rows.iter().map(|&i| (i / w) as u32).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|&i| grid.origin_um[0] + (i % w) as f64 * grid.bin_um)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|&i| grid.origin_um[1] + (i / w) as f64 * grid.bin_um)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(vec![grid.bin_um; rows.len()])),
        Arc::new(UInt32Array::from(
            rows.iter().map(|&i| ntx[i]).collect::<Vec<_>>(),
        )),
    ];
    for (name, (_, v)) in column_names(results).into_iter().zip(results) {
        fields.push(Field::new(name, DataType::Float64, true));
        cols.push(Arc::new(Float64Array::from(
            rows.iter()
                .map(|&i| v.scores[i].is_finite().then_some(v.scores[i]))
                .collect::<Vec<_>>(),
        )));
    }
    write(Arc::new(Schema::new(fields)), cols, results)
}
