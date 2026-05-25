use std::path::Path;

use crate::shared_types::Error;

/// Abstracts over how storage is allocated and released for a goopy instance.
///
/// `path` is the full working directory for the instance (e.g. `/data/goopies/tasty-lucky-clover`).
pub trait StorageAllocator {
    fn allocate(&self, path: &Path) -> Result<(), Error>;
    fn release(&self, path: &Path) -> Result<(), Error>;
}

// ---------------------------------------------------------------------------
// ZfsAllocator
// ---------------------------------------------------------------------------

/// Production allocator that creates/destroys ZFS datasets with a quota.
///
/// `allocate` runs: `zfs create -o quota={quota_mb}M {pool}/{slug}`
/// `release`  runs: `zfs destroy {pool}/{slug}`
///
/// The slug is derived from the last path component of `path`.
pub struct ZfsAllocator {
    pub pool: String,
    pub quota_mb: u64,
}

impl StorageAllocator for ZfsAllocator {
    fn allocate(&self, path: &Path) -> Result<(), Error> {
        let slug = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let dataset = format!("{}/{}", self.pool, slug);
        let quota_arg = format!("quota={}M", self.quota_mb);

        let status = std::process::Command::new("zfs")
            .args(["create", "-o", &quota_arg, &dataset])
            .status()
            .map_err(|e| Error::Other(format!("failed to run zfs create: {}", e)))?;

        if !status.success() {
            return Err(Error::Other(format!(
                "zfs create failed for dataset '{}' (exit status: {})",
                dataset, status
            )));
        }

        Ok(())
    }

    fn release(&self, path: &Path) -> Result<(), Error> {
        let slug = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let dataset = format!("{}/{}", self.pool, slug);

        let status = std::process::Command::new("zfs")
            .args(["destroy", &dataset])
            .status()
            .map_err(|e| Error::Other(format!("failed to run zfs destroy: {}", e)))?;

        if !status.success() {
            return Err(Error::Other(format!(
                "zfs destroy failed for dataset '{}' (exit status: {})",
                dataset, status
            )));
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PlainDirAllocator
// ---------------------------------------------------------------------------

/// Dev-mode allocator that simply creates/removes a directory on the local filesystem.
pub struct PlainDirAllocator;

impl StorageAllocator for PlainDirAllocator {
    fn allocate(&self, path: &Path) -> Result<(), Error> {
        std::fs::create_dir_all(path).map_err(Error::Io)
    }

    fn release(&self, path: &Path) -> Result<(), Error> {
        std::fs::remove_dir_all(path).map_err(Error::Io)
    }
}
