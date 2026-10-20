//! Import of external per-cell analysis tables (annotations, precomputed LR
//! scores) and segmentation-free grids (e.g. niche or programme maps).
//! Values are imported as given; nothing is recomputed or inferred.

use crate::stats::{categorical, field_info, sanitize_id};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use xenium_core::codec::FieldColumn;
use xenium_core::manifest::{FieldInfo, GridInfo};
use xenium_core::CellDictionary;
use xenium_io::tables::{numeric_fraction, ColumnValues, Table, TableColumn};

/// A requested column: exact name or `*` glob, optionally forcing its type
/// (`name:cat` for integer-coded labels, `name:num` for numbers stored as text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    pub pattern: String,
    pub force: Option<ForceKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceKind {
    Categorical,
    Numeric,
}

impl ColumnSpec {
    /// Parse `a,b:cat,N0*_mass:num` (whitespace ignored).
    pub fn parse_list(s: &str) -> Result<Vec<ColumnSpec>> {
        s.split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| {
                let (pattern, force) = match t.rsplit_once(':') {
                    Some((p, "cat")) => (p, Some(ForceKind::Categorical)),
                    Some((p, "num")) => (p, Some(ForceKind::Numeric)),
                    Some((_, other)) => {
                        bail!("column spec {t:?}: unknown type {other:?} (use :cat or :num)")
                    }
                    None => (t, None),
                };
                if pattern.is_empty() {
                    bail!("empty column name in {s:?}");
                }
                Ok(ColumnSpec {
                    pattern: pattern.to_string(),
                    force,
                })
            })
            .collect()
    }

    pub fn matches(&self, name: &str) -> bool {
        glob_match(&self.pattern, name)
    }
}

/// `*` matches any run of characters; everything else is literal.
fn glob_match(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.len() >= p.len() && rest.ends_with(p);
        } else {
            match rest.find(p) {
                Some(j) => rest = &rest[j + p.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Columns to import (in table order) with their forced kind. `None` = all columns.
fn select_columns<'a>(
    table: &'a Table,
    select: Option<&[ColumnSpec]>,
    skip: &[&str],
) -> Vec<(&'a TableColumn, Option<ForceKind>)> {
    // With a selection, columns follow the order of the specs (table order within a
    // glob), so the first listed column becomes the default in the viewer.
    let mut out: Vec<(usize, &TableColumn, Option<ForceKind>)> = table
        .columns
        .iter()
        .filter(|c| !skip.contains(&c.name.as_str()))
        .filter_map(|c| match select {
            None => Some((0, c, None)),
            Some(specs) => specs
                .iter()
                .position(|sp| sp.matches(&c.name))
                .map(|i| (i, c, specs[i].force)),
        })
        .collect();
    out.sort_by_key(|e| e.0);
    out.into_iter().map(|(_, c, f)| (c, f)).collect()
}

/// Apply a forced kind: numbers -> category labels, or text -> numbers (all must parse).
fn coerce(col: &TableColumn, force: Option<ForceKind>, source: &str) -> Result<ColumnValues> {
    Ok(match (force, &col.values) {
        (Some(ForceKind::Categorical), ColumnValues::Numeric(v)) => ColumnValues::Text(
            v.iter()
                .map(|x| {
                    x.is_finite().then(|| {
                        if x.fract() == 0.0 && x.abs() < 1e15 {
                            format!("{}", *x as i64)
                        } else {
                            format!("{x}")
                        }
                    })
                })
                .collect(),
        ),
        (Some(ForceKind::Numeric), ColumnValues::Text(v)) => ColumnValues::Numeric(
            v.iter()
                .map(|s| match s {
                    None => Ok(f64::NAN),
                    Some(s) => match s.as_str() {
                        "True" | "true" | "TRUE" => Ok(1.0),
                        "False" | "false" | "FALSE" => Ok(0.0),
                        _ => s.parse::<f64>().map_err(|_| {
                            anyhow!("{source}: column {:?} value {s:?} is not numeric", col.name)
                        }),
                    },
                })
                .collect::<Result<_>>()?,
        ),
        (_, v) => v.clone(),
    })
}

/// Text columns with more distinct values than this are skipped with a warning.
pub const MAX_CATEGORIES: usize = 2000;

/// Returns a warning (and `true` = skip) for text columns that look like IDs or
/// almost-numeric columns with a few stray values.
fn check_text_column(
    source: &str,
    name: &str,
    vals: &[Option<String>],
    forced: bool,
    warnings: &mut Vec<String>,
) -> bool {
    let (frac, bad) = numeric_fraction(vals);
    // Only warn when a few stray values turned a numeric column into text; a type
    // the user forced (`:cat`) is intentional.
    if !forced && (0.9..1.0).contains(&frac) {
        warnings.push(format!(
            "{source}: column {name:?} is {:.0}% numeric but contains values such as {:?}; imported as categories — fix the table if it should be numeric",
            frac * 100.0,
            bad.unwrap_or("")
        ));
    }
    let distinct: BTreeSet<&str> = vals.iter().flatten().map(String::as_str).collect();
    if distinct.len() > MAX_CATEGORIES {
        warnings.push(format!(
            "{source}: column {name:?} skipped: {} distinct text values (> {MAX_CATEGORIES}) looks like an identifier, not a category",
            distinct.len()
        ));
        return true;
    }
    false
}

/// Per (pair, role): values by cell, "seen" flags (duplicate detection), provenance values.
type LrGroup<'a> = (Vec<f32>, Vec<bool>, BTreeMap<&'a str, BTreeSet<String>>);

pub struct ImportedField {
    pub info: FieldInfo,
    pub column: FieldColumn,
}

pub struct ImportReport {
    pub fields: Vec<ImportedField>,
    pub warnings: Vec<String>,
}

fn keys_for(
    table: &Table,
    id_col: &str,
    dict: &CellDictionary,
    warnings: &mut Vec<String>,
    source: &str,
) -> Result<Vec<Option<u32>>> {
    let ids = table
        .text(id_col)
        .map_err(|_| anyhow!("{source}: id column {id_col:?} not found"))?;
    let mut unknown = 0usize;
    let keys = ids
        .iter()
        .map(|id| {
            let k = id.as_deref().and_then(|s| dict.key_of(s));
            if k.is_none() {
                unknown += 1;
            }
            k
        })
        .collect();
    if unknown > 0 {
        warnings.push(format!(
            "{source}: {unknown} rows have a cell_id not present in cells.parquet; ignored"
        ));
    }
    Ok(keys)
}

/// Wide table: one row per cell, any number of numeric/categorical columns.
/// With `select`, only matching columns are imported; `done` holds source column
/// names already imported from an earlier table (those are skipped here).
pub fn import_annotations(
    table: &Table,
    id_col: &str,
    dict: &CellDictionary,
    source: &str,
    taken: &BTreeSet<String>,
    select: Option<&[ColumnSpec]>,
    done: &mut BTreeSet<String>,
) -> Result<ImportReport> {
    let mut warnings = vec![];
    let keys = keys_for(table, id_col, dict, &mut warnings, source)?;
    let mut seen = vec![false; dict.len()];
    let mut dups = 0;
    for k in keys.iter().flatten() {
        if std::mem::replace(&mut seen[*k as usize - 1], true) {
            dups += 1;
        }
    }
    if dups > 0 {
        bail!("{source}: {dups} cells appear more than once; a wide annotation table needs one row per cell");
    }
    let n = dict.len();
    let mut fields = vec![];
    for (col, force) in select_columns(table, select, &[id_col]) {
        if !done.insert(col.name.clone()) {
            // With an explicit selection, "first table wins" is the documented rule.
            if select.is_none() {
                warnings.push(format!(
                    "{source}: column {:?} already imported from an earlier table; skipped",
                    col.name
                ));
            }
            continue;
        }
        let values = coerce(col, force, source)?;
        let mut id = sanitize_id(&col.name);
        if taken.contains(&id) {
            id = format!("annotation.{id}");
        }
        let (column, cats) = match &values {
            ColumnValues::Numeric(v) => {
                let mut out = vec![f32::NAN; n];
                for (row, k) in keys.iter().enumerate() {
                    if let Some(k) = k {
                        out[*k as usize - 1] = v[row] as f32;
                    }
                }
                (FieldColumn::Numeric(out), None)
            }
            ColumnValues::Text(v) => {
                if check_text_column(source, &col.name, v, force.is_some(), &mut warnings) {
                    continue;
                }
                let mut out: Vec<Option<String>> = vec![None; n];
                for (row, k) in keys.iter().enumerate() {
                    if let Some(k) = k {
                        out[*k as usize - 1] = v[row].clone();
                    }
                }
                let (c, cats) = categorical(&out);
                (c, Some(cats))
            }
        };
        let info = field_info(&id, &col.name, "annotation", source, &column, cats);
        fields.push(ImportedField { info, column });
    }
    Ok(ImportReport { fields, warnings })
}

/// Long LR table: `cell_id, pair_id, score` (+ optional `role`, `method`,
/// `normalization`, `analysis_run_id`). One numeric field per `(pair, role)`.
pub fn import_lr_scores(
    table: &Table,
    dict: &CellDictionary,
    source: &str,
) -> Result<ImportReport> {
    let mut warnings = vec![];
    for req in ["cell_id", "pair_id", "score"] {
        if table.column(req).is_none() {
            bail!("{source}: required column {req:?} not found");
        }
    }
    let keys = keys_for(table, "cell_id", dict, &mut warnings, source)?;
    let pairs = table.text("pair_id")?;
    let roles = if table.column("role").is_some() {
        table.text("role")?
    } else {
        vec![Some("aggregate".to_string()); table.n_rows]
    };
    let scores = table.numeric("score")?;
    let prov_cols: Vec<(&str, Vec<Option<String>>)> =
        ["method", "normalization", "analysis_run_id"]
            .iter()
            .filter(|c| table.column(c).is_some())
            .map(|c| Ok((*c, table.text(c)?)))
            .collect::<Result<_>>()?;
    let n = dict.len();
    let mut groups: BTreeMap<(String, String), LrGroup> = BTreeMap::new();
    let mut dups = 0usize;
    for row in 0..table.n_rows {
        let (Some(k), Some(pair)) = (keys[row], pairs[row].as_ref()) else {
            continue;
        };
        let role = roles[row].clone().unwrap_or_else(|| "aggregate".into());
        let g = groups
            .entry((pair.clone(), role))
            .or_insert_with(|| (vec![f32::NAN; n], vec![false; n], BTreeMap::new()));
        let i = k as usize - 1;
        if std::mem::replace(&mut g.1[i], true) {
            dups += 1;
        }
        g.0[i] = scores[row] as f32;
        for (c, v) in &prov_cols {
            if let Some(v) = &v[row] {
                g.2.entry(c).or_default().insert(v.clone());
            }
        }
    }
    if dups > 0 {
        bail!("{source}: {dups} duplicate (cell_id, pair_id, role) rows; refusing to pick one");
    }
    let mut fields = vec![];
    for ((pair, role), (vals, _, prov)) in groups {
        let id = sanitize_id(&format!("lr:{pair}:{role}"));
        let column = FieldColumn::Numeric(vals);
        let mut info = field_info(
            &id,
            &format!("{pair} ({role})"),
            "lr_score",
            source,
            &column,
            None,
        );
        let mut p = Map::new();
        p.insert("pair_id".into(), json!(pair));
        p.insert("role".into(), json!(role));
        for (c, vals) in prov {
            let v: Vec<String> = vals.into_iter().collect();
            p.insert(
                c.to_string(),
                if v.len() == 1 { json!(v[0]) } else { json!(v) },
            );
        }
        info.provenance = p;
        fields.push(ImportedField { info, column });
    }
    Ok(ImportReport { fields, warnings })
}

pub struct ImportedGrid {
    pub info: GridInfo,
    pub rasters: Vec<FieldColumn>,
    pub warnings: Vec<String>,
}

pub const MAX_GRID_CELLS: usize = 16_000_000;

/// Regular grid table: `x_um, y_um` are grid-cell *centres* in Xenium µm.
/// What to import from one grid table.
pub struct GridSpec<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub x_col: &'a str,
    pub y_col: &'a str,
    pub spacing: Option<f64>,
    pub select: Option<&'a [ColumnSpec]>,
}

pub fn import_grid(table: &Table, spec: &GridSpec, source: &str) -> Result<ImportedGrid> {
    let GridSpec {
        id,
        label,
        x_col,
        y_col,
        spacing,
        select,
    } = *spec;
    let xs = table.numeric(x_col).map_err(|e| anyhow!("{source}: {e}"))?;
    let ys = table.numeric(y_col).map_err(|e| anyhow!("{source}: {e}"))?;
    if xs.iter().chain(ys).any(|v| !v.is_finite()) {
        bail!("{source}: grid coordinates must all be finite");
    }
    if xs.is_empty() {
        bail!("{source}: grid table is empty");
    }
    let infer = |v: &[f64]| -> Option<f64> {
        let mut u: Vec<f64> = v.to_vec();
        u.sort_by(f64::total_cmp);
        u.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        u.windows(2)
            .map(|w| w[1] - w[0])
            .filter(|d| *d > 1e-9)
            .min_by(f64::total_cmp)
    };
    let s = match spacing {
        Some(s) => s,
        None => {
            let (sx, sy) = (infer(xs), infer(ys));
            match (sx, sy) {
                (Some(a), Some(b)) if (a - b).abs() <= 1e-6 * a.max(b) => a,
                (Some(a), None) | (None, Some(a)) => a,
                _ => bail!("{source}: cannot infer one square grid spacing (x {sx:?}, y {sy:?}); pass it explicitly"),
            }
        }
    };
    if !(s.is_finite() && s > 0.0) {
        bail!("{source}: invalid grid spacing {s}");
    }
    let minx = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let miny = ys.iter().copied().fold(f64::INFINITY, f64::min);
    let maxx = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let maxy = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let origin = [minx - s / 2.0, miny - s / 2.0];
    let w = ((maxx - minx) / s).round() as usize + 1;
    let h = ((maxy - miny) / s).round() as usize + 1;
    if w * h > MAX_GRID_CELLS {
        bail!("{source}: grid of {w}x{h} exceeds the {MAX_GRID_CELLS}-cell limit");
    }
    let mut idx = Vec::with_capacity(xs.len());
    let mut seen = vec![false; w * h];
    for (x, y) in xs.iter().zip(ys) {
        let fi = (x - minx) / s;
        let fj = (y - miny) / s;
        if (fi - fi.round()).abs() > 0.01 || (fj - fj.round()).abs() > 0.01 {
            bail!("{source}: point ({x}, {y}) is not on a regular grid of spacing {s}");
        }
        let i = fj.round() as usize * w + fi.round() as usize;
        if std::mem::replace(&mut seen[i], true) {
            bail!("{source}: duplicate grid position ({x}, {y})");
        }
        idx.push(i);
    }
    let mut fields = vec![];
    let mut rasters = vec![];
    let mut warnings = vec![];
    if let Some(specs) = select {
        for sp in specs.iter().filter(|sp| !sp.pattern.contains('*')) {
            if !table.columns.iter().any(|c| c.name == sp.pattern) {
                bail!("{source}: requested grid column {:?} not found", sp.pattern);
            }
        }
    }
    for (col, force) in select_columns(table, select, &[x_col, y_col]) {
        let values = coerce(col, force, source)?;
        if let ColumnValues::Text(v) = &values {
            if check_text_column(source, &col.name, v, force.is_some(), &mut warnings) {
                continue;
            }
        }
        let fid = sanitize_id(&col.name);
        let (column, cats) = match &values {
            ColumnValues::Numeric(v) => {
                let mut out = vec![f32::NAN; w * h];
                for (r, &i) in idx.iter().enumerate() {
                    out[i] = v[r] as f32;
                }
                (FieldColumn::Numeric(out), None)
            }
            ColumnValues::Text(v) => {
                let mut out: Vec<Option<String>> = vec![None; w * h];
                for (r, &i) in idx.iter().enumerate() {
                    out[i] = v[r].clone();
                }
                let (c, cats) = categorical(&out);
                (c, Some(cats))
            }
        };
        let mut info = field_info(&fid, &col.name, "grid", source, &column, cats);
        info.provenance
            .insert("grid_id".into(), Value::String(id.to_string()));
        fields.push(info);
        rasters.push(column);
    }
    if xs.len() < w * h {
        warnings.push(format!(
            "{source}: {} of {} grid positions have no row (stored as missing)",
            w * h - xs.len(),
            w * h
        ));
    }
    Ok(ImportedGrid {
        info: GridInfo {
            id: id.to_string(),
            label: label.to_string(),
            origin_um: origin,
            spacing_um: s,
            width: w as u32,
            height: h as u32,
            fields,
            source_file: source.to_string(),
            resampling: "nearest".into(),
        },
        rasters,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xenium_io::tables::read_delimited;

    fn gs<'a>(
        x: &'a str,
        y: &'a str,
        spacing: Option<f64>,
        select: Option<&'a [ColumnSpec]>,
    ) -> GridSpec<'a> {
        GridSpec {
            id: "g",
            label: "G",
            x_col: x,
            y_col: y,
            spacing,
            select,
        }
    }

    fn dict() -> CellDictionary {
        CellDictionary::from_ids(vec!["a-1".into(), "b-1".into(), "c-1".into()]).unwrap()
    }

    #[test]
    fn annotations_join_by_original_id() {
        let t = read_delimited(
            "cell_id,type,score\nc-1,X,1\na-1,Y,\nzz,Y,2\n".as_bytes(),
            b',',
        )
        .unwrap();
        let r = import_annotations(
            &t,
            "cell_id",
            &dict(),
            "ann.csv",
            &BTreeSet::new(),
            None,
            &mut BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(r.warnings.len(), 1);
        let ty = &r.fields[0];
        assert_eq!(
            ty.column,
            FieldColumn::Categorical {
                codes: vec![2, 0, 1],
                n_categories: 2
            }
        );
        match &r.fields[1].column {
            FieldColumn::Numeric(v) => assert!(v[0].is_nan() && v[1].is_nan() && v[2] == 1.0),
            _ => panic!(),
        }
        let dup = read_delimited("cell_id,t\na-1,X\na-1,Y\n".as_bytes(), b',').unwrap();
        assert!(import_annotations(
            &dup,
            "cell_id",
            &dict(),
            "d",
            &BTreeSet::new(),
            None,
            &mut BTreeSet::new()
        )
        .is_err());
    }

    #[test]
    fn lr_scores_per_pair_role_with_provenance() {
        let t = read_delimited(
            "cell_id,pair_id,role,score,method\na-1,P,sender,1.5,m1\nb-1,P,sender,NaN,m1\na-1,P,receiver,2,m1\n".as_bytes(),
            b',',
        )
        .unwrap();
        let r = import_lr_scores(&t, &dict(), "lr.csv").unwrap();
        let ids: Vec<&str> = r.fields.iter().map(|f| f.info.id.as_str()).collect();
        assert_eq!(ids, vec!["lr:P:receiver", "lr:P:sender"]);
        assert_eq!(r.fields[1].info.provenance["method"], json!("m1"));
        match &r.fields[1].column {
            FieldColumn::Numeric(v) => assert!(v[0] == 1.5 && v[1].is_nan() && v[2].is_nan()),
            _ => panic!(),
        }
        let dup =
            read_delimited("cell_id,pair_id,score\na-1,P,1\na-1,P,2\n".as_bytes(), b',').unwrap();
        assert!(import_lr_scores(&dup, &dict(), "d").is_err());
    }

    #[test]
    fn grid_layout_and_validation() {
        let t = read_delimited(
            "x,y,label,p\n10,10,A,0.5\n30,10,B,1\n10,30,A,NA\n".as_bytes(),
            b',',
        )
        .unwrap();
        let g = import_grid(&t, &gs("x", "y", None, None), "grid.csv").unwrap();
        assert_eq!(
            (g.info.width, g.info.height, g.info.spacing_um),
            (2, 2, 20.0)
        );
        assert_eq!(g.info.origin_um, [0.0, 0.0]);
        assert_eq!(
            g.rasters[0],
            FieldColumn::Categorical {
                codes: vec![1, 2, 1, 0],
                n_categories: 2
            }
        );
        assert_eq!(g.warnings.len(), 1);
        let off = read_delimited("x,y,v\n10,10,1\n25,10,2\n30,10,3\n".as_bytes(), b',').unwrap();
        assert!(import_grid(&off, &gs("x", "y", Some(20.0), None), "o").is_err());
    }

    #[test]
    fn column_specs_select_glob_and_force() {
        let specs = ColumnSpec::parse_list("type, N0*_mass, cluster:cat ,flag:num").unwrap();
        assert_eq!(specs.len(), 4);
        assert!(specs[1].matches("N001_mass") && !specs[1].matches("N001_dist"));
        assert!(
            glob_match("*_um", "dist_N001_um")
                && glob_match("a*b*c", "aXbYc")
                && !glob_match("a*b", "ab_c")
        );
        assert!(ColumnSpec::parse_list("x:weird").is_err());
        let t = read_delimited(
            "cell_id,type,N001_mass,N002_mass,cluster,flag,secret\na-1,X,0.1,0.9,3,True,p1\nb-1,Y,0.5,0.5,7,False,p2\n".as_bytes(),
            b',',
        )
        .unwrap();
        let mut done = BTreeSet::new();
        let r = import_annotations(
            &t,
            "cell_id",
            &dict(),
            "a",
            &BTreeSet::new(),
            Some(&specs),
            &mut done,
        )
        .unwrap();
        let ids: Vec<&str> = r.fields.iter().map(|f| f.info.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["type", "N001_mass", "N002_mass", "cluster", "flag"]
        );
        assert_eq!(
            r.fields[3].info.kind,
            xenium_core::manifest::FieldKind::Categorical
        );
        assert_eq!(
            r.fields[4].info.kind,
            xenium_core::manifest::FieldKind::Numeric
        );
        // A second table offering the same column is skipped, not duplicated.
        let r2 = import_annotations(
            &t,
            "cell_id",
            &dict(),
            "b",
            &BTreeSet::new(),
            Some(&specs[..1]),
            &mut done,
        )
        .unwrap();
        assert!(r2.fields.is_empty());
        let g = read_delimited(
            "x_um,y_um,niche,patient\n10,10,1,p\n30,10,2,p\n".as_bytes(),
            b',',
        )
        .unwrap();
        let gsel = ColumnSpec::parse_list("niche:cat").unwrap();
        let nope = ColumnSpec::parse_list("nope").unwrap();
        let gs_ = gs;
        let ig = import_grid(&g, &gs_("x_um", "y_um", None, Some(&gsel)), "g.csv").unwrap();
        assert_eq!(ig.info.fields.len(), 1);
        assert_eq!(
            ig.info.fields[0].kind,
            xenium_core::manifest::FieldKind::Categorical
        );
        assert!(import_grid(&g, &gs_("x_um", "y_um", None, Some(&nope)), "g.csv").is_err());
    }
}
