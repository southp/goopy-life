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
    /// allocated (recording which goopy instance owns it), and return it.
    /// Returns `Error::PortExhausted` if the entire range is exhausted.
    fn acquire_port(&self, slug: &str, range_start: u32, range_end: u32) -> Result<u32, Error>;

    /// Release a previously-acquired port so it can be reused.
    fn release_port(&self, port: u32) -> Result<(), Error>;

    /// Count all rows in the registry regardless of status.
    ///
    /// Used to enforce `max_provisioned` (disk-bound cap).
    /// Includes `Failed` instances, which still hold a port/directory until
    /// the sweep task reaps them.
    fn count_provisioned(&self) -> Result<u32, Error>;

    /// Count instances that are consuming RAM: `Spawning`, `Done`, and
    /// `Despawning`.
    ///
    /// Used to enforce `max_active` (RAM-bound cap). `Despawning` is counted
    /// because `despawn` flips the status synchronously and then tears the
    /// instance down on a background thread — the process stays resident for
    /// the whole teardown. That over-counts briefly, which is the safe
    /// direction for a RAM cap.
    ///
    /// `Failed` is not counted: its process is gone. A future `Suspended`
    /// status (#96, scale-to-zero) will be handled the same way — by simply not
    /// being added to the `IN` list.
    fn count_active(&self) -> Result<u32, Error>;
}
