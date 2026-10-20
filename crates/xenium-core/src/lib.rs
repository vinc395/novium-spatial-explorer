//! Shared, IO-free building blocks for xenium-web.
//!
//! * [`frames`]: typed coordinate frames and validated 2-D affine transforms.
//! * [`registration`]: named image-registration inputs resolved to one explicit transform.
//! * [`cellid`]: stable, reversible string `cell_id` <-> `CellKey` dictionaries.
//! * [`features`]: Xenium feature (gene / control) classification.
//! * [`grid`]: the spatial tile grid shared by the indexer, server and browser.
//! * [`codec`]: versioned little-endian binary formats streamed to the browser.
//! * [`manifest`]: the versioned per-section index manifest.

pub mod cellid;
pub mod codec;
pub mod features;
pub mod frames;
pub mod grid;
pub mod manifest;
pub mod registration;

pub use cellid::{CellDictionary, CellKey};
pub use frames::{Affine2, XeniumUm};
