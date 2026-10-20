//! Gene-signature scoring for xenium-web.
//!
//! * `ucell` — the UCell algorithm, numerically identical to the R package;
//! * `section` — panel, gene coverage, cell- and bin-level scoring of a section;
//! * `engine` — scoring jobs (cancellable, with progress) that fill the
//!   disk-backed `cache`;
//! * `signatures` — the user's signature definitions;
//! * `summary` — per-category (cell type / niche) statistics;
//! * `export` — Parquet output.

pub mod cache;
pub mod engine;
pub mod export;
pub mod progress;
pub mod section;
pub mod signatures;
pub mod summary;
pub mod ucell;
