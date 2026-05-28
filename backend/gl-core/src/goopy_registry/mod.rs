pub mod sqlite_registry;

use crate::goopy::Goopy;
use crate::shared_types::*;

pub trait GoopyRegistry {
    fn save(&self, gp: &Goopy) -> Result<(), Error>;
    fn load(&self, slug: &str) -> Result<Option<Goopy>, Error>;
    fn delete(&self, slug: &str) -> Result<(), Error>;
    fn list(&self) -> Result<Vec<Goopy>, Error>;
    fn update_status(&self, slug: &str, new_status: Status) -> Result<(), Error>;

    /// Find the lowest unused port in `[range_start, range_end)`, mark it as
    /// allocated, and return it.  Returns `Error::PortExhausted` if the entire
    /// range is exhausted.
    fn acquire_port(&self, range_start: u32, range_end: u32) -> Result<u32, Error>;

    /// Release a previously-acquired port so it can be reused.
    fn release_port(&self, port: u32) -> Result<(), Error>;
}
