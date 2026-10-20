//! Summary statistics and field-column construction.

use std::collections::BTreeMap;
use xenium_core::codec::FieldColumn;
use xenium_core::manifest::{CategoryInfo, FieldInfo, FieldKind, NumericStats};

pub fn numeric_stats(values: &[f32]) -> NumericStats {
    let mut v: Vec<f64> = values
        .iter()
        .filter(|x| x.is_finite())
        .map(|&x| x as f64)
        .collect();
    let n_missing = (values.len() - v.len()) as u64;
    if v.is_empty() {
        return NumericStats {
            n_missing,
            ..Default::default()
        };
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    NumericStats {
        min: Some(v[0]),
        max: Some(v[v.len() - 1]),
        mean: Some(v.iter().sum::<f64>() / v.len() as f64),
        p01: Some(q(0.01)),
        p99: Some(q(0.99)),
        n_valid: v.len() as u64,
        n_missing,
    }
}

/// Build a categorical column from per-cell optional labels (index = key - 1).
/// Categories are sorted by name so codes are deterministic.
pub fn categorical(values: &[Option<String>]) -> (FieldColumn, Vec<CategoryInfo>) {
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for v in values.iter().flatten() {
        *counts.entry(v.as_str()).or_default() += 1;
    }
    let names: Vec<&str> = counts.keys().copied().collect();
    let code_of: std::collections::HashMap<&str, u32> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (*n, i as u32 + 1))
        .collect();
    let codes = values
        .iter()
        .map(|v| v.as_deref().map(|s| code_of[s]).unwrap_or(0))
        .collect();
    let cats = names
        .iter()
        .map(|n| CategoryInfo {
            name: n.to_string(),
            count: counts[n],
        })
        .collect();
    (
        FieldColumn::Categorical {
            codes,
            n_categories: names.len() as u32,
        },
        cats,
    )
}

/// Make a URL-safe field id from an arbitrary column name.
pub fn sanitize_id(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("..") {
        out = out.replace("..", "_.");
    }
    if out.is_empty() || out.starts_with('.') {
        out.insert(0, 'f');
    }
    out.truncate(120);
    out
}

pub fn field_info(
    id: &str,
    label: &str,
    group: &str,
    source: &str,
    col: &FieldColumn,
    cats: Option<Vec<CategoryInfo>>,
) -> FieldInfo {
    let (kind, stats) = match col {
        FieldColumn::Numeric(v) => (FieldKind::Numeric, Some(numeric_stats(v))),
        FieldColumn::Categorical { .. } => (FieldKind::Categorical, None),
    };
    FieldInfo {
        id: id.to_string(),
        label: label.to_string(),
        kind,
        group: group.to_string(),
        source_file: source.to_string(),
        stats,
        categories: cats,
        provenance: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_and_categories() {
        let s = numeric_stats(&[1.0, f32::NAN, 3.0, 2.0]);
        assert_eq!(
            (s.min, s.max, s.n_valid, s.n_missing),
            (Some(1.0), Some(3.0), 3, 1)
        );
        let (col, cats) =
            categorical(&[Some("b".into()), None, Some("a".into()), Some("b".into())]);
        assert_eq!(
            cats.iter()
                .map(|c| (c.name.as_str(), c.count))
                .collect::<Vec<_>>(),
            vec![("a", 1), ("b", 2)]
        );
        assert_eq!(
            col,
            FieldColumn::Categorical {
                codes: vec![2, 0, 1, 2],
                n_categories: 2
            }
        );
        assert_eq!(sanitize_id("fine cell/type"), "fine_cell_type");
        assert_eq!(sanitize_id("../x"), "_._x");
        assert!(xenium_core::manifest::is_safe_id(&sanitize_id("a..b")));
    }
}
