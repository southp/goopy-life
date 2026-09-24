pub mod sqlite_registry;

use crate::goopy::Goopy;
use crate::instance_event::InstanceEvent;
use crate::shared_types::*;

use chrono::{DateTime, Utc};

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

    // -- the instance event log (#118) ------------------------------------
    //
    // Append-only, and deliberately not keyed to a `goopies` row: it is what
    // is left once the row has been reaped. See [`crate::instance_event`].

    /// Append `event` to the log.
    ///
    /// For an event that stands alone. When the event describes something that
    /// also changes the instance's row, prefer [`fail_with_event`] or
    /// [`delete_with_event`], which write both together.
    ///
    /// [`fail_with_event`]: GoopyRegistry::fail_with_event
    /// [`delete_with_event`]: GoopyRegistry::delete_with_event
    fn record_event(&self, event: &InstanceEvent) -> Result<(), Error>;

    /// Mark `slug` [`Status::Failed`] and append `event`, in one transaction.
    ///
    /// The two halves of recording a failure: the row says *this is broken*,
    /// the event says *why*. Splitting them across two writes would let a crash
    /// in between produce the exact state #118 exists to abolish — a `Failed`
    /// row with no reason attached.
    ///
    /// The event is skipped when it repeats the slug's newest one — same
    /// phase, outcome, code and detail. The sweep retries every `Failed` row,
    /// so an instance that cannot be torn down would otherwise add the same
    /// reason on every run and bury everything else in the log. The status is
    /// still set, and `Failed` alone already says the instance is stuck. If
    /// retention has since dropped the earlier event, the next failure writes
    /// a fresh one.
    fn fail_with_event(&self, slug: &str, event: &InstanceEvent) -> Result<(), Error>;

    /// Delete `slug` and append `event`, in one transaction.
    ///
    /// Atomicity matters in both directions here. A committed delete with a
    /// lost event silently destroys the instance's only record; a committed
    /// event with a rolled-back delete claims a reap that did not happen, which
    /// is the lie #117 was filed about, written somewhere more durable than a
    /// log line.
    fn delete_with_event(&self, slug: &str, event: &InstanceEvent) -> Result<(), Error>;

    /// Drop events that occurred strictly before `cutoff`, returning how many
    /// rows went.
    ///
    /// Append-only means unbounded, so something has to trim it. The sweep
    /// does, because it is already the periodic maintenance task.
    fn prune_events_before(&self, cutoff: DateTime<Utc>) -> Result<u32, Error>;

    /// Read up to `limit` events, newest first, for one `slug` or for every
    /// instance.
    ///
    /// Newest first because the question is almost always "what has been
    /// failing lately". Note that the results carry `detail`, which is
    /// operator-only — see [`InstanceEvent`].
    fn events(&self, slug: Option<&str>, limit: u32) -> Result<Vec<InstanceEvent>, Error>;
}
