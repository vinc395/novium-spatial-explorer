//! Generic analysis tables (CSV / TSV, optionally gzipped, or Parquet), used for
//! per-cell annotations, precomputed LR scores and segmentation-free grids.
//!
//! Text cells equal to `""`, `NA`, `NaN`, `nan`, `null`, `NULL` or `None` are missing.
//! A column is numeric when every non-missing value parses as a finite or NaN f64.

use crate::columns::{as_f64, as_utf8, f64_at, str_at};
use anyhow::{anyhow, bail, Context, Result};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum ColumnValues {
    Numeric(Vec<f64>),
    Text(Vec<Option<String>>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableColumn {
    pub name: String,
    pub values: ColumnValues,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    pub n_rows: usize,
    pub columns: Vec<TableColumn>,
}

pub fn is_missing(s: &str) -> bool {
    matches!(
        s.trim(),
        "" | "NA" | "NaN" | "nan" | "null" | "NULL" | "None" | "#N/A" | "N/A" | "n/a"
    )
}

/// For a text column, the fraction of non-missing values that parse as numbers and
/// one example that does not (used to warn about almost-numeric columns).
pub fn numeric_fraction(vals: &[Option<String>]) -> (f64, Option<&str>) {
    let present: Vec<&str> = vals.iter().flatten().map(String::as_str).collect();
    if present.is_empty() {
        return (0.0, None);
    }
    let bad = present.iter().find(|s| s.parse::<f64>().is_err()).copied();
    let ok = present.iter().filter(|s| s.parse::<f64>().is_ok()).count();
    (ok as f64 / present.len() as f64, bad)
}

impl Table {
    pub fn column(&self, name: &str) -> Option<&TableColumn> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn text(&self, name: &str) -> Result<Vec<Option<String>>> {
        let c = self
            .column(name)
            .ok_or_else(|| anyhow!("column {name:?} not found"))?;
        Ok(match &c.values {
            ColumnValues::Text(v) => v.clone(),
            ColumnValues::Numeric(v) => v
                .iter()
                .map(|x| (!x.is_nan()).then(|| format_number(*x)))
                .collect(),
        })
    }

    pub fn numeric(&self, name: &str) -> Result<&[f64]> {
        let c = self
            .column(name)
            .ok_or_else(|| anyhow!("column {name:?} not found"))?;
        match &c.values {
            ColumnValues::Numeric(v) => Ok(v),
            ColumnValues::Text(_) => bail!("column {name:?} is not numeric"),
        }
    }
}

/// Integer-valued numbers print without a trailing `.0` so numeric IDs round-trip.
fn format_number(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

pub fn read_table(path: &Path) -> Result<Table> {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name.ends_with(".parquet") {
        return read_parquet_table(path);
    }
    let gz = name.ends_with(".gz");
    let base = name.trim_end_matches(".gz");
    let delim = if base.ends_with(".tsv") || base.ends_with(".txt") || base.ends_with(".tab") {
        b'\t'
    } else if base.ends_with(".csv") {
        b','
    } else {
        bail!(
            "{}: unsupported table extension (use .csv, .tsv, .txt, optionally .gz, or .parquet)",
            path.display()
        );
    };
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader: Box<dyn Read> = if gz {
        Box::new(flate2::read::MultiGzDecoder::new(BufReader::new(file)))
    } else {
        Box::new(BufReader::new(file))
    };
    read_delimited(reader, delim).with_context(|| format!("parse {}", path.display()))
}

pub fn read_delimited(reader: impl Read, delim: u8) -> Result<Table> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delim)
        .flexible(false)
        .from_reader(reader);
    let headers: Vec<String> = rdr
        .headers()?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    if headers.is_empty() {
        bail!("table has no header row");
    }
    let mut seen = std::collections::HashSet::new();
    for h in &headers {
        if !seen.insert(h) {
            bail!("duplicate column name {h:?}");
        }
    }
    let mut raw: Vec<Vec<Option<String>>> = vec![vec![]; headers.len()];
    let mut n = 0;
    for rec in rdr.records() {
        let rec = rec?;
        for (i, v) in rec.iter().enumerate() {
            raw[i].push((!is_missing(v)).then(|| v.trim().to_string()));
        }
        n += 1;
    }
    let columns = headers
        .into_iter()
        .zip(raw)
        .map(|(name, vals)| TableColumn {
            name,
            values: infer(vals),
        })
        .collect();
    Ok(Table { n_rows: n, columns })
}

fn infer(vals: Vec<Option<String>>) -> ColumnValues {
    let any = vals.iter().any(Option::is_some);
    // Accepts integers, decimals, exponents and +/-inf ("inf", "Infinity").
    let parsed: Option<Vec<f64>> = vals
        .iter()
        .map(|v| match v {
            None => Some(f64::NAN),
            Some(s) => s.parse::<f64>().ok(),
        })
        .collect();
    match parsed {
        Some(p) if any => ColumnValues::Numeric(p),
        _ => ColumnValues::Text(vals),
    }
}

fn read_parquet_table(path: &Path) -> Result<Table> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let schema = builder.schema().clone();
    let reader = builder
        .with_batch_size(crate::columns::BATCH_ROWS)
        .build()?;
    let mut cols: Vec<TableColumn> = schema
        .fields()
        .iter()
        .map(|f| TableColumn {
            name: f.name().clone(),
            values: if f.data_type().is_numeric() {
                ColumnValues::Numeric(vec![])
            } else {
                ColumnValues::Text(vec![])
            },
        })
        .collect();
    let mut n = 0;
    for batch in reader {
        let batch = batch?;
        for (i, c) in cols.iter_mut().enumerate() {
            let arr = batch.column(i);
            match &mut c.values {
                ColumnValues::Numeric(v) => {
                    let a = as_f64(arr)?;
                    v.extend((0..batch.num_rows()).map(|r| f64_at(&a, r)));
                }
                ColumnValues::Text(v) => {
                    let a = as_utf8(arr)?;
                    v.extend(
                        (0..batch.num_rows())
                            .map(|r| str_at(&a, r).filter(|s| !is_missing(s)).map(str::to_string)),
                    );
                }
            }
        }
        n += batch.num_rows();
    }
    Ok(Table {
        n_rows: n,
        columns: cols,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_types_and_missing() {
        let csv = "cell_id,type,score,conf\naaa-1,T cell,1.5,NA\nbbb-1,,2,0.9\nccc-1,B cell,NaN,\n";
        let t = read_delimited(csv.as_bytes(), b',').unwrap();
        assert_eq!(t.n_rows, 3);
        assert_eq!(
            t.text("type").unwrap(),
            vec![Some("T cell".into()), None, Some("B cell".into())]
        );
        let s = t.numeric("score").unwrap();
        assert_eq!(s[0], 1.5);
        assert!(s[2].is_nan());
        assert!(t.numeric("conf").unwrap()[0].is_nan());
        let t = read_delimited("x\n1\ninf\n#N/A\n".as_bytes(), b',').unwrap();
        let x = t.numeric("x").unwrap();
        assert!(x[1].is_infinite() && x[2].is_nan());
        assert!(t.numeric("type").is_err());
        // Numeric IDs round-trip without ".0"
        let t = read_delimited("cell_id,x\n12,1\n".as_bytes(), b',').unwrap();
        assert_eq!(t.text("cell_id").unwrap(), vec![Some("12".into())]);
    }

    #[test]
    fn rejects_ragged_and_duplicate_headers() {
        assert!(read_delimited("a,b\n1\n".as_bytes(), b',').is_err());
        assert!(read_delimited("a,a\n1,2\n".as_bytes(), b',').is_err());
    }
}
