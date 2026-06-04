use std::path::Path;

use crate::shared_types::Error;
use crate::storage_allocator::StorageAllocator;

/// No-op allocator used with `--dry-run`. Prints what would happen without touching disk.
pub struct DryRunStorageAllocator;

impl StorageAllocator for DryRunStorageAllocator {
    fn allocate(&self, path: &Path) -> Result<(), Error> {
        println!("[dry-run] would allocate storage: {}", path.display());
        Ok(())
    }

    fn release(&self, path: &Path) -> Result<(), Error> {
        println!("[dry-run] would release storage: {}", path.display());
        Ok(())
    }
}
