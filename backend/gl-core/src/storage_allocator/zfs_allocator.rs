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
            .env("LANG", "C")
            .output()
            .map_err(|e| Error::Other(format!("failed to run zfs create: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("already exists") {
                info!(%dataset, "ZFS dataset already present, treating as success");
                return Ok(());
            }
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
            .env("LANG", "C")
            .output()
            .map_err(|e| Error::Other(format!("failed to run zfs destroy: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not exist") {
                info!(%dataset, "ZFS dataset already absent, treating as success");
                return Ok(());
            }
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
    const TESTPOOL: &str = "testpool";

    #[test]
    #[ignore]
    fn allocate_creates_dataset() {
        let allocator = ZfsAllocator::new(TESTPOOL.to_string(), 100);
        let path = PathBuf::from(format!("/{}/goopy-zfs-test-alloc", TESTPOOL));
        let dataset = format!("{}/goopy-zfs-test-alloc", TESTPOOL);

        allocator.allocate(&path).expect("allocate should succeed");

        // Verify dataset exists
        let list_output = std::process::Command::new("zfs")
            .args(["list", &dataset])
            .output()
            .expect("zfs list should run");
        assert!(list_output.status.success(), "dataset should exist after allocate");

        // Verify mountpoint was set correctly
        let mp_output = std::process::Command::new("zfs")
            .args(["get", "-H", "-o", "value", "mountpoint", &dataset])
            .output()
            .expect("zfs get mountpoint should run");
        assert_eq!(
            String::from_utf8_lossy(&mp_output.stdout).trim(),
            path.to_str().unwrap(),
            "mountpoint should match path argument"
        );

        // Cleanup
        std::process::Command::new("zfs")
            .args(["destroy", &dataset])
            .status()
            .ok();
    }

    #[test]
    #[ignore]
    fn release_destroys_dataset() {
        let allocator = ZfsAllocator::new(TESTPOOL.to_string(), 100);
        let path = PathBuf::from(format!("/{}/goopy-zfs-test-release", TESTPOOL));
        let dataset = format!("{}/goopy-zfs-test-release", TESTPOOL);

        // Setup: create via allocate so the dataset mirrors production state
        allocator.allocate(&path).expect("setup: allocate should succeed");

        allocator.release(&path).expect("release should succeed");

        let output = std::process::Command::new("zfs")
            .args(["list", &dataset])
            .output()
            .expect("zfs list should run");
        assert!(!output.status.success(), "dataset should be gone after release");
    }

    #[test]
    #[ignore]
    fn release_is_idempotent() {
        let allocator = ZfsAllocator::new(TESTPOOL.to_string(), 100);
        let path = PathBuf::from(format!("/{}/goopy-zfs-test-idempotent", TESTPOOL));
        let dataset = format!("{}/goopy-zfs-test-idempotent", TESTPOOL);

        allocator.allocate(&path).expect("setup: allocate should succeed");
        allocator.release(&path).expect("first release should succeed");
        allocator.release(&path).expect("second release should succeed (idempotent)");

        // Cleanup guard (no-op if already gone)
        std::process::Command::new("zfs")
            .args(["destroy", &dataset])
            .status()
            .ok();
    }

    #[test]
    #[ignore]
    fn allocate_is_idempotent() {
        let allocator = ZfsAllocator::new(TESTPOOL.to_string(), 100);
        let path = PathBuf::from(format!("/{}/goopy-zfs-test-alloc-idem", TESTPOOL));
        let dataset = format!("{}/goopy-zfs-test-alloc-idem", TESTPOOL);

        allocator.allocate(&path).expect("first allocate should succeed");
        allocator.allocate(&path).expect("second allocate should succeed (idempotent)");

        // Cleanup
        std::process::Command::new("zfs")
            .args(["destroy", &dataset])
            .status()
            .ok();
    }
}
