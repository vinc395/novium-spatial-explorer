//! Detect which Xenium output files are present in a folder.

use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Default)]
pub struct XeniumFiles {
    pub root: PathBuf,
    pub experiment: Option<PathBuf>,
    pub cells: Option<PathBuf>,
    pub cell_boundaries: Option<PathBuf>,
    pub nucleus_boundaries: Option<PathBuf>,
    pub transcripts: Option<PathBuf>,
    /// Morphology images found (not indexed by the current version).
    pub morphology_images: Vec<PathBuf>,
    /// Recognised but unsupported representations (reported, never silently used).
    pub unsupported: Vec<String>,
}

fn existing(root: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|n| root.join(n)).find(|p| p.is_file())
}

impl XeniumFiles {
    pub fn discover(root: &Path) -> Result<Self> {
        if !root.is_dir() {
            bail!("{} is not a directory", root.display());
        }
        let mut f = XeniumFiles {
            root: root.to_path_buf(),
            experiment: existing(root, &["experiment.xenium"]),
            cells: existing(root, &["cells.parquet"]),
            cell_boundaries: existing(root, &["cell_boundaries.parquet"]),
            nucleus_boundaries: existing(root, &["nucleus_boundaries.parquet"]),
            transcripts: existing(root, &["transcripts.parquet"]),
            ..Default::default()
        };
        for name in [
            "morphology.ome.tif",
            "morphology_mip.ome.tif",
            "morphology_focus.ome.tif",
        ] {
            if root.join(name).is_file() {
                f.morphology_images.push(root.join(name));
            }
        }
        if let Ok(rd) = std::fs::read_dir(root.join("morphology_focus")) {
            let mut v: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "tif"))
                .collect();
            v.sort();
            f.morphology_images.extend(v);
        }
        for (name, what) in [
            ("cells.csv.gz", "cells.csv.gz (use cells.parquet)"),
            (
                "transcripts.csv.gz",
                "transcripts.csv.gz (use transcripts.parquet)",
            ),
            (
                "cells.zarr.zip",
                "cells.zarr.zip (Parquet tables are used instead)",
            ),
            (
                "transcripts.zarr.zip",
                "transcripts.zarr.zip (Parquet tables are used instead)",
            ),
        ] {
            if root.join(name).is_file() {
                f.unsupported.push(what.to_string());
            }
        }
        Ok(f)
    }

    pub fn require_cells(&self) -> Result<&Path> {
        match &self.cells {
            Some(p) => Ok(p),
            None => bail!(
                "{}: cells.parquet not found (CSV-only outputs from very old XOA versions are not supported)",
                self.root.display()
            ),
        }
    }
}
