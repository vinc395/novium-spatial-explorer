//! `experiment.xenium` (JSON) parsing. Only technical fields are retained; run,
//! region, slide and instrument identifiers are deliberately not copied.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;
use xenium_core::manifest::XeniumInfo;

pub fn read_experiment(path: &Path) -> Result<XeniumInfo> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_experiment(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn parse_experiment(text: &str) -> Result<XeniumInfo> {
    let v: Value = serde_json::from_str(text)?;
    let num = |k: &str| v.get(k).and_then(Value::as_f64);
    let int = |k: &str| v.get(k).and_then(Value::as_u64);
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let pixel_size_um = num("pixel_size").filter(|p| p.is_finite() && *p > 0.0);
    let targets = match (
        int("panel_num_targets_predesigned"),
        int("panel_num_targets_custom"),
    ) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    };
    Ok(XeniumInfo {
        pixel_size_um,
        analysis_sw_version: s("analysis_sw_version"),
        panel_name: s("panel_name"),
        panel_num_targets: targets,
        num_cells_reported: int("num_cells"),
        transcripts_reported: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_fields_and_ignores_identifiers() {
        let info = parse_experiment(
            r#"{"pixel_size":0.2125,"analysis_sw_version":"xenium-3.1.0.4","panel_name":"P",
                "panel_num_targets_predesigned":377,"panel_num_targets_custom":100,
                "num_cells":123,"run_name":"SECRET","region_name":"SECRET2"}"#,
        )
        .unwrap();
        assert_eq!(info.pixel_size_um, Some(0.2125));
        assert_eq!(info.panel_num_targets, Some(477));
        assert_eq!(info.num_cells_reported, Some(123));
        assert!(!format!("{info:?}").contains("SECRET"));
    }

    #[test]
    fn rejects_bad_pixel_size_and_bad_json() {
        assert_eq!(
            parse_experiment(r#"{"pixel_size":-1}"#)
                .unwrap()
                .pixel_size_um,
            None
        );
        assert!(parse_experiment("not json").is_err());
    }
}
