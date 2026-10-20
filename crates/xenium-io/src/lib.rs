//! Native (non-WASM) readers for Xenium output folders, per-cell analysis tables
//! and TIFF images. Readers stream: nothing here materialises a whole transcript
//! table in memory.
//!
//! Original implementation. Odon (GPL-3.0) and Fovea (MIT) were consulted as
//! references only; no source code was copied from either (see
//! `docs/THIRD_PARTY_ATTRIBUTION.md`).

pub mod boundaries;
pub mod cells;
pub mod columns;
pub mod discover;
pub mod experiment;
pub mod fingerprint;
pub mod morphology;
pub mod ome_tiles;
pub mod synth;
pub mod tables;
pub mod tiff_image;
pub mod transcripts;

pub use discover::XeniumFiles;
