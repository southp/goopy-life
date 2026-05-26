use std::path::Path;

use tracing::{error, info, instrument};

use crate::shared_types::Error;
use crate::storage_allocator::StorageAllocator;

/// Production allocator that creates/destroys ZFS datasets with a quota.
///
/// `allocate` runs: `zfs create -o quota={quota_mb}M -o mountpoint=<path> {pool}/{slug}`
/// `release`  runs: `zfs destroy {pool}/{slug}`
///
/// The slug is derived from the last path component of `path`.
pub struct ZfsAllocator {
    pool: String,
    quota_mb: u64,
}

impl ZfsAllocator {
    pub fn new(pool: String, quota_mb: u64) -> Self {
        Self { pool, quota_mb }
    }
}

impl StorageAllocator for ZfsAllocator {
    #[instrument(skip(self), fields(pool = %self.pool, quota_mb = self.quota_mb))]
    fn allocate(&self, path: &Path) -> Result<(), Error> {
        let slug = path
            .file_name()
            .ok_or_else(|| Error::Other(format!("path has no final component: {}", path.display())))?
            .to_string_lossy()
            .into_owned();
        let dataset = format!("{}/{}", self.pool, slug);
        let quota_arg = format!("quota={}M", self.quota_mb);
        let mountpoint_arg = format!("mountpoint={}", path.display());

        info!(%dataset, path = %path.display(), "creating ZFS dataset");

        let output = std::process::Command::new("zfs")
            .args(["create", "-o", &quota_arg, "-o", &mountpoint_arg, &dataset])
            .output()
            .map_err(|e| Error::Other(format!("failed to run zfs create: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!(%dataset, %stderr, "zfs create failed");
            return Err(Error::Other(format!(
                "zfs create failed for dataset '{}' (exit status: {}): {}",
                dataset, output.status, stderr.trim()
            )));
        }

        info!(%dataset, "ZFS dataset created");
        Ok(())
    }

    #[instrument(skip(self), fields(pool = %self.pool))]
    fn release(&self, path: &Path) -> Result<(), Error> {
        let slug = path
            .file_name()
            .ok_or_else(|| Error::Other(format!("path has no final component: {}", path.display())))?
            .to_string_lossy()
            .into_owned();
        let dataset = format!("{}/{}", self.pool, slug);

        info!(%dataset, "destroying ZFS dataset");

        let output = std::process::Command::new("zfs")
            .args(["destroy", &dataset])
            .output()
            .map_err(|e| Error::Other(format!("failed to run zfs destroy: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!(%dataset, %stderr, "zfs destroy failed");
            return Err(Error::Other(format!(
                "zfs destroy failed for dataset '{}' (exit status: {}): {}",
                dataset, output.status, stderr.trim()
            )));
        }

        info!(%dataset, "ZFS dataset destroyed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// These tests require a live ZFS pool named "testpool".
    /// Run with: cargo test -- --include-ignored

    #[test]
    #[ignore]
    fn allocate_creates_dataset() {
        let pool = "testpool";
        let allocator = ZfsAllocator::new(pool.to_string(), 100);
        let path = PathBuf::from("/testpool/goopy-zfs-test-alloc");

        allocator.allocate(&path).expect("allocate should succeed");

        // Verify dataset exists
        let output = std::process::Command::new("zfs")
            .args(["list", &format!("{}/goopy-zfs-test-alloc", pool)])
            .output()
            .expect("zfs list should run");
        assert!(output.status.success(), "dataset should exist after allocate");

        // Cleanup
        std::process::Command::new("zfs")
            .args(["destroy", &format!("{}/goopy-zfs-test-alloc", pool)])
            .status()
            .ok();
    }

    #[test]
    #[ignore]
    fn release_destroys_dataset() {
        let pool = "testpool";
        let allocator = ZfsAllocator::new(pool.to_string(), 100);
        let path = PathBuf::from("/testpool/goopy-zfs-test-release");
        let dataset = format!("{}/goopy-zfs-test-release", pool);

        // Setup
        std::process::Command::new("zfs")
            .args(["create", &dataset])
            .status()
            .expect("setup: zfs create should succeed");

        allocator.release(&path).expect("release should succeed");

        let output = std::process::Command::new("zfs")
            .args(["list", &dataset])
            .output()
            .expect("zfs list should run");
        assert!(!output.status.success(), "dataset should be gone after release");
    }
}
