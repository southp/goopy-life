pub mod dry_run_allocator;
pub mod plain_dir_allocator;
pub mod zfs_allocator;

pub use dry_run_allocator::DryRunStorageAllocator;
pub use plain_dir_allocator::PlainDirAllocator;
pub use zfs_allocator::ZfsAllocator;

use crate::shared_types::Error;
use std::path::Path;

/// Abstracts over how storage is allocated and released for a goopy instance.
///
/// `path` is the full working directory for the instance (e.g. `/data/goopies/tasty-lucky-clover`).
pub trait StorageAllocator: Send + Sync {
    fn allocate(&self, path: &Path) -> Result<(), Error>;
    fn release(&self, path: &Path) -> Result<(), Error>;
}
