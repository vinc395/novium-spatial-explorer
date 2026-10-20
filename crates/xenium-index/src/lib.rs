//! Offline indexer (`ingest`) and on-disk reader (`SectionStore`) for one
//! Xenium section. See `docs/DATA_CONTRACT.md` for the derived file layout.

pub mod bundle_ingest;
pub mod ingest;
pub mod pack;
pub mod publish;
pub mod pyramid;
pub mod qc;
pub mod stats;
pub mod store;
pub mod tables_ingest;
pub mod transcripts_ingest;
pub mod validate;

pub use ingest::{ingest, GridInput, ImageInput, IngestConfig, RegistrationInput};
pub use store::SectionStore;
