//! Atomic publishing of a completed index version.
//!
//! ```text
//! <root>/<section_id>                -> .versions/<section_id>/<version>   (symlink)
//! <root>/.versions/<section_id>/<version>/manifest.json ...
//! <root>/.versions/<section_id>/.tmp-<version>/         (staging while building)
//! ```
//! A new version becomes visible only through one `rename(2)` of a symlink, so a
//! reader sees either the old complete index or the new complete index.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::path::{Path, PathBuf};

/// Exclusive per-section ingest lock (`<root>/.versions/.<section>.lock`), released on drop.
pub struct SectionLock {
    file: File,
}

impl SectionLock {
    /// Flush every pending write on the index filesystem with one `syncfs`. The lock
    /// file was opened before the ingest wrote anything, so a writeback error since then
    /// is reported here.
    #[cfg(target_os = "linux")]
    fn sync_filesystem(&self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: syncfs only reads the descriptor, which `self.file` keeps open.
        if unsafe { libc::syncfs(self.file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

pub fn lock_section(root: &Path, section_id: &str) -> Result<SectionLock> {
    let dir = root.join(".versions");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(".{section_id}.lock"));
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(SectionLock { file }),
        Err(std::fs::TryLockError::WouldBlock) => {
            bail!("another ingest of section {section_id:?} is running")
        }
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("lock {}", path.display()))
        }
    }
}

/// fsync every file and directory below `dir` (and `dir` itself).
pub fn fsync_tree(dir: &Path) -> Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            fsync_tree(&p)?;
        } else {
            File::open(&p)?.sync_all()?;
        }
    }
    File::open(dir)?.sync_all()?;
    Ok(())
}

pub fn versions_dir(root: &Path, section_id: &str) -> PathBuf {
    root.join(".versions").join(section_id)
}

pub fn new_version_name() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "v{}-{:09}-{}",
        now.as_secs(),
        now.subsec_nanos(),
        std::process::id()
    )
}

/// Create a fresh staging directory for a new version. Call with the section lock
/// held: any other `.tmp-*` directory is then a leftover from a crashed run and is removed.
pub fn create_staging(root: &Path, section_id: &str, version: &str) -> Result<PathBuf> {
    let link = root.join(section_id);
    if link.exists() && !link.is_symlink() {
        bail!(
            "{} exists and is not a symlink managed by xenium-web; refusing to overwrite it",
            link.display()
        );
    }
    let vdir = versions_dir(root, section_id);
    if let Ok(rd) = std::fs::read_dir(&vdir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(".tmp-") {
                tracing::warn!("removing stale staging directory {}", e.path().display());
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let staging = vdir.join(format!(".tmp-{version}"));
    std::fs::create_dir_all(&staging).with_context(|| format!("create {}", staging.display()))?;
    Ok(staging)
}

/// Move staging -> `<version>` and atomically repoint `<root>/<section_id>`.
/// Older versions beyond `keep` are deleted afterwards.
pub fn publish(
    root: &Path,
    section_id: &str,
    staging: &Path,
    version: &str,
    keep: usize,
    lock: &SectionLock,
) -> Result<PathBuf> {
    let vdir = versions_dir(root, section_id);
    let final_dir = vdir.join(version);
    // Make the new version durable before it becomes reachable. On Linux one syncfs
    // does it: an fsync per file also flushes the disk cache each time, which takes
    // about 15 minutes for the ~30,000 tiles of a section with morphology on a USB disk.
    #[cfg(target_os = "linux")]
    lock.sync_filesystem()
        .with_context(|| format!("sync the filesystem of {}", root.display()))?;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = lock;
        fsync_tree(staging)?;
    }
    std::fs::rename(staging, &final_dir)
        .with_context(|| format!("rename to {}", final_dir.display()))?;
    File::open(&vdir)?.sync_all()?;
    let rel = Path::new(".versions").join(section_id).join(version);
    let tmp_link = root.join(format!(".{section_id}.link-{version}"));
    let _ = std::fs::remove_file(&tmp_link);
    std::os::unix::fs::symlink(&rel, &tmp_link)?;
    std::fs::rename(&tmp_link, root.join(section_id)).context("swap section symlink")?;
    File::open(root)?.sync_all()?;
    // Never delete whatever the link points at now (defensive against concurrent publishers).
    let live = std::fs::read_link(root.join(section_id))
        .ok()
        .and_then(|t| t.file_name().map(|n| n.to_os_string()));
    // Retention: keep the newest `keep` complete versions (the live one is newest).
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&vdir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('v'))
                && p.is_dir()
        })
        .collect();
    versions.sort();
    let keep = keep.max(1);
    if versions.len() > keep {
        for old in &versions[..versions.len() - keep] {
            if old != &final_dir && old.file_name().map(|n| n.to_os_string()) != live {
                if let Err(e) = std::fs::remove_dir_all(old) {
                    tracing::warn!("could not remove old index version {}: {e}", old.display());
                }
            }
        }
    }
    Ok(final_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_exclusive_and_stale_staging_is_swept() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        let l = lock_section(r, "S").unwrap();
        assert!(lock_section(r, "S").is_err());
        assert!(lock_section(r, "T").is_ok());
        let stale = create_staging(r, "S", "v0").unwrap();
        let fresh = create_staging(r, "S", "v1").unwrap();
        assert!(!stale.exists() && fresh.exists());
        drop(l);
        assert!(lock_section(r, "S").is_ok());
    }

    #[test]
    fn publish_swaps_and_retains() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        let mut published = vec![];
        let lock = lock_section(r, "S1").unwrap();
        for i in 0..3 {
            let v = format!("v{i}");
            let s = create_staging(r, "S1", &v).unwrap();
            std::fs::write(s.join("manifest.json"), format!("{i}")).unwrap();
            published.push(publish(r, "S1", &s, &v, 2, &lock).unwrap());
            assert_eq!(
                std::fs::read_to_string(r.join("S1/manifest.json")).unwrap(),
                format!("{i}")
            );
        }
        assert!(!published[0].exists());
        assert!(published[1].exists() && published[2].exists());
        // A real directory in the way is never clobbered.
        std::fs::create_dir_all(r.join("S2")).unwrap();
        assert!(create_staging(r, "S2", "v0").is_err());
    }
}
