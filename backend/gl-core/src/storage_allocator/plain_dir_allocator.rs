use std::path::Path;

use tracing::{info, instrument};

use crate::shared_types::Error;
use crate::storage_allocator::StorageAllocator;

/// Dev-mode allocator that simply creates/removes a directory on the local filesystem.
pub struct PlainDirAllocator;

impl StorageAllocator for PlainDirAllocator {
    #[instrument(skip(self))]
    fn allocate(&self, path: &Path) -> Result<(), Error> {
        info!(path = %path.display(), "creating directory");
        std::fs::create_dir_all(path).map_err(Error::Io)?;
        info!(path = %path.display(), "directory created");
        Ok(())
    }

    #[instrument(skip(self))]
    fn release(&self, path: &Path) -> Result<(), Error> {
        info!(path = %path.display(), "removing directory");
        match std::fs::remove_dir_all(path) {
            Ok(()) => {
                info!(path = %path.display(), "directory removed");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!(path = %path.display(), "directory already absent, nothing to do");
                Ok(())
            }
            Err(e) => Err(Error::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn allocate_creates_directory() {
        let base = tempdir().unwrap();
        let target = base.path().join("goopy-alloc");

        PlainDirAllocator.allocate(&target).expect("allocate should succeed");
        assert!(target.is_dir(), "directory should exist after allocate");
    }

    #[test]
    fn allocate_is_idempotent() {
        let base = tempdir().unwrap();
        let target = base.path().join("goopy-alloc-idem");

        PlainDirAllocator.allocate(&target).expect("first allocate");
        PlainDirAllocator.allocate(&target).expect("second allocate should not fail");
        assert!(target.is_dir());
    }

    #[test]
    fn release_removes_directory() {
        let base = tempdir().unwrap();
        let target = base.path().join("goopy-release");
        PlainDirAllocator.allocate(&target).expect("setup: allocate should succeed");

        PlainDirAllocator.release(&target).expect("release should succeed");
        assert!(!target.exists(), "directory should be gone after release");
    }

    #[test]
    fn release_is_idempotent() {
        let base = tempdir().unwrap();
        let target = base.path().join("goopy-release-idem");
        // Directory does not exist — second release must also succeed.

        PlainDirAllocator.release(&target).expect("release on missing path should be Ok");
    }
}
