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
    /// Used to report `max_provisioned` headroom (disk-bound cap). `Failed`
    /// instances are included: they still occupy a registry slot until the
    /// sweep reaps them, and one left behind by a failed *despawn* also still
    /// holds its port and working directory.
    fn count_provisioned(&self) -> Result<u32, Error>;

    /// Count instances that are consuming RAM: `Spawning`, `Done`, and
    /// `Despawning`.
    ///
    /// Used to report `max_active` headroom (RAM-bound cap). `Despawning` is
    /// counted because `despawn` flips the status synchronously and then tears
    /// the instance down on a background thread — the process stays resident
    /// for the whole teardown. That over-counts briefly, which is the safe
    /// direction for a RAM cap.
    ///
    /// `Failed` is not counted: its process is gone. A future `Suspended`
    /// status (#96, scale-to-zero) will be handled the same way — by simply not
    /// being added to the `IN` list.
    fn count_active(&self) -> Result<u32, Error>;

    /// Insert `gp`, but only if both caps still have room.
    ///
    /// The counts and the insert happen inside one write transaction, so
    /// concurrent spawns cannot all observe the same free slot and overshoot.
    /// Callers must use this rather than counting and then calling [`save`],
    /// which is a check-then-act race.
    ///
    /// Returns [`Error::CapacityFull`] naming the cap that was already met, or
    /// [`Error::AlreadyExists`] if the slug collides.
    ///
    /// [`save`]: GoopyRegistry::save
    fn save_within_caps(
        &self,
        gp: &Goopy,
        max_provisioned: u32,
        max_active: u32,
    ) -> Result<(), Error>;
}
