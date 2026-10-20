//! Cheap, reproducible source fingerprints: size, mtime and SHA-256 over the first
//! and last MiB. Detects replaced/truncated inputs without hashing many GB.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;
use xenium_core::manifest::SourceFingerprint;

const SPAN: u64 = 1 << 20;

pub fn fingerprint(role: &str, path: &Path) -> Result<SourceFingerprint> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let size = meta.len();
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    h.update(size.to_le_bytes());
    let mut buf = vec![0u8; SPAN.min(size) as usize];
    f.read_exact(&mut buf)?;
    h.update(&buf);
    if size > SPAN {
        let start = size.saturating_sub(SPAN).max(SPAN);
        f.seek(SeekFrom::Start(start))?;
        let mut tail = Vec::new();
        f.take(SPAN).read_to_end(&mut tail)?;
        h.update(&tail);
    }
    let digest = h.finalize();
    Ok(SourceFingerprint {
        role: role.to_string(),
        file_name: path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size_bytes: size,
        modified_unix: modified,
        sha256_head_tail: digest.iter().map(|b| format!("{b:02x}")).collect(),
    })
}
