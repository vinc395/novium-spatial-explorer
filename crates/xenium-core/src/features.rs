//! Classification of Xenium transcript features (genes versus control probes).
//!
//! Preference order: an explicit `is_gene` column, then `codeword_category`
//! (XOA >= 2.x), then documented feature-name prefixes for older outputs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FeatureCategory {
    Gene,
    NegativeControlProbe,
    NegativeControlCodeword,
    GenomicControl,
    UnassignedCodeword,
    DeprecatedCodeword,
    /// Non-gene feature whose specific control type is not known.
    OtherControl,
}

impl FeatureCategory {
    pub fn is_gene(self) -> bool {
        self == FeatureCategory::Gene
    }

    pub fn from_codeword_category(cat: &str) -> Option<Self> {
        Some(match cat {
            "predesigned_gene" | "custom_gene" | "gene" => FeatureCategory::Gene,
            "negative_control_probe" => FeatureCategory::NegativeControlProbe,
            "negative_control_codeword" => FeatureCategory::NegativeControlCodeword,
            "genomic_control_probe" | "genomic_control" => FeatureCategory::GenomicControl,
            "unassigned_codeword" => FeatureCategory::UnassignedCodeword,
            "deprecated_codeword" => FeatureCategory::DeprecatedCodeword,
            _ => return None,
        })
    }

    pub fn from_name(name: &str) -> Self {
        const RULES: &[(&str, FeatureCategory)] = &[
            ("NegControlProbe_", FeatureCategory::NegativeControlProbe),
            (
                "NegControlCodeword_",
                FeatureCategory::NegativeControlCodeword,
            ),
            ("antisense_", FeatureCategory::NegativeControlProbe),
            ("BLANK_", FeatureCategory::UnassignedCodeword),
            ("UnassignedCodeword_", FeatureCategory::UnassignedCodeword),
            ("DeprecatedCodeword_", FeatureCategory::DeprecatedCodeword),
            ("GenomicControl_", FeatureCategory::GenomicControl),
            ("Intergenic_", FeatureCategory::GenomicControl),
        ];
        RULES
            .iter()
            .find(|(p, _)| name.starts_with(p))
            .map(|(_, c)| *c)
            .unwrap_or(FeatureCategory::Gene)
    }

    /// Combine the available evidence. A non-gene `is_gene=false` with no other
    /// evidence becomes `OtherControl`, never `Gene`.
    pub fn classify(name: &str, codeword_category: Option<&str>, is_gene: Option<bool>) -> Self {
        let by_cat = codeword_category.and_then(Self::from_codeword_category);
        match (is_gene, by_cat) {
            (Some(true), _) => FeatureCategory::Gene,
            (Some(false), Some(c)) if !c.is_gene() => c,
            (Some(false), _) => match Self::from_name(name) {
                FeatureCategory::Gene => FeatureCategory::OtherControl,
                c => c,
            },
            (None, Some(c)) => c,
            (None, None) => Self::from_name(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FeatureCategory::*;

    #[test]
    fn name_prefixes() {
        assert_eq!(FeatureCategory::from_name("CXCL8"), Gene);
        assert_eq!(
            FeatureCategory::from_name("NegControlProbe_00042"),
            NegativeControlProbe
        );
        assert_eq!(FeatureCategory::from_name("BLANK_0006"), UnassignedCodeword);
        assert_eq!(
            FeatureCategory::from_name("UnassignedCodeword_0001"),
            UnassignedCodeword
        );
    }

    #[test]
    fn explicit_columns_take_precedence() {
        assert_eq!(FeatureCategory::classify("BLANK_1", None, Some(true)), Gene);
        assert_eq!(
            FeatureCategory::classify("WEIRD", None, Some(false)),
            OtherControl
        );
        assert_eq!(
            FeatureCategory::classify("X", Some("negative_control_codeword"), None),
            NegativeControlCodeword
        );
        assert_eq!(
            FeatureCategory::classify("X", Some("predesigned_gene"), None),
            Gene
        );
        assert_eq!(
            FeatureCategory::classify("NegControlProbe_1", Some("unknown_new_cat"), None),
            NegativeControlProbe
        );
    }
}
