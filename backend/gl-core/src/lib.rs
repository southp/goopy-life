mod shared_types;
mod goopy;
mod goopy_manager;
mod slug_generator;
pub mod config;
pub mod goopy_registry;
pub mod goopy_provisioner;
pub mod storage_allocator;

pub use config::Config;
pub use goopy::*;
pub use shared_types::*;
pub use goopy_manager::*;
pub use slug_generator::generate_slug;
pub use storage_allocator::{StorageAllocator, ZfsAllocator, PlainDirAllocator};
