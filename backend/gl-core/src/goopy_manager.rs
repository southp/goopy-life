use crate::goopy::*;
use crate::goopy_provisioner::*;
use crate::goopy_registry::*;
use crate::instance_event::*;
use crate::shared_types::*;

use chrono::{Duration, Utc};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug)]
pub struct GoopyManagerConfig {
    pub base_dir: PathBuf,
    pub domain: String,
    pub life_in_days: i32,
    pub port_range_start: u32,
    pub port_range_end: u32,
    /// RAM-bound cap on resident (Spawning + Done) instances. See [`Config::max_active`].
    pub max_active: u32,
    /// Disk-bound cap on total provisioned instances. See [`Config::max_provisioned`].
    pub max_provisioned: u32,
    /// How long an instance event is kept before the sweep drops it. See
    /// [`Config::event_retention_days`].
    pub event_retention_days: u32,
}

/// A point-in-time reading of both instance caps and how much of each is used.
///
/// Reported by [`GoopyManager::capacity`] and surfaced by gl-serv so the
/// frontend can show headroom *before* a user clicks spawn, instead of only
/// discovering a full server from a 503. The counts are a snapshot with no
/// lock held: by the time a caller acts on them another spawn may have taken
/// the last slot, so this is advisory only — [`GoopyRegistry::save_within_caps`]
/// remains the authority that actually enforces the caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    /// Instances currently consuming RAM. See [`GoopyRegistry::count_active`].
    pub active: u32,
    /// The RAM-bound cap `active` is measured against.
    pub max_active: u32,
    /// Total instances occupying a registry slot. See
    /// [`GoopyRegistry::count_provisioned`].
    pub provisioned: u32,
    /// The disk-bound cap `provisioned` is measured against.
    pub max_provisioned: u32,
}

impl Capacity {
    /// The cap that will actually refuse the next spawn — the one with less
    /// headroom — as `(used, total)`.
    ///
    /// This is the only pair worth showing a visitor, who cares about one
    /// thing: is there a free slot. Reporting `active` unconditionally makes
    /// the UI contradict itself — "0 / 2 in use" beside "the server is full" —
    /// because a `Failed` row holds a slot without holding RAM, and the sweep
    /// may not reap it for a full `sweep_interval_secs`.
    ///
    /// Ties go to the provisioned cap: it is the one `save_within_caps` checks
    /// first, so it is the one that names the refusal.
    pub fn binding(&self) -> (u32, u32) {
        let provisioned_headroom = self.max_provisioned.saturating_sub(self.provisioned);
        let active_headroom = self.max_active.saturating_sub(self.active);
        if active_headroom < provisioned_headroom {
            (self.active, self.max_active)
        } else {
            (self.provisioned, self.max_provisioned)
        }
    }

    /// Whether either cap is met, i.e. a spawn attempted right now would be
    /// refused with [`Error::CapacityFull`].
    ///
    /// Derived from [`binding`] so "which number is shown" and "is it full"
    /// can never disagree.
    ///
    /// [`binding`]: Capacity::binding
    pub fn is_full(&self) -> bool {
        let (used, total) = self.binding();
        used >= total
    }
}

pub struct GoopyManager<
    Registry: GoopyRegistry + Send + Sync + 'static,
    Provisioner: GoopyProvisioner + Send + Sync + 'static,
> {
    pub base_dir: PathBuf,
    pub domain: String,
    pub goopy_life_in_days: i32,
    pub port_range_start: u32,
    pub port_range_end: u32,
    pub max_active: u32,
    pub max_provisioned: u32,
    pub event_retention_days: u32,

    registry: Arc<Registry>,
    provisioner: Arc<Provisioner>,
}

impl<Registry, Provisioner> GoopyManager<Registry, Provisioner>
where
    Registry: GoopyRegistry + Send + Sync + 'static,
    Provisioner: GoopyProvisioner + Send + Sync + 'static,
{
    pub fn new(config: GoopyManagerConfig, registry: Registry, provisioner: Provisioner) -> Self {
        Self {
            base_dir: config.base_dir,
            domain: config.domain,
            goopy_life_in_days: config.life_in_days,
            port_range_start: config.port_range_start,
            port_range_end: config.port_range_end,
            max_active: config.max_active,
            max_provisioned: config.max_provisioned,
            event_retention_days: config.event_retention_days,
            registry: Arc::new(registry),
            provisioner: Arc::new(provisioner),
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn spawn(&self) -> Result<(String, u32), Error> {
        if self.goopy_life_in_days <= 0 {
            return Err(Error::Invalid);
        }

        const MAX_RETRIES: usize = 10;

        // Port is acquired inside the retry loop so the DB record links the
        // port to the slug from the moment of allocation.
        let mut new_goopy = None;
        for _ in 0..MAX_RETRIES {
            let slug = crate::slug_generator::generate_slug();
            debug_assert!(
                !slug.is_empty(),
                "slug generator must not produce empty slugs"
            );
            let port =
                self.registry
                    .acquire_port(&slug, self.port_range_start, self.port_range_end)?;

            let candidate = Goopy {
                slug: slug.clone(),
                life_in_days: self.goopy_life_in_days,
                created_at: Utc::now(),
                working_dir: self.base_dir.join(&slug),
                port,
                status: Status::Spawning,
                provisioner_kind: self.provisioner.kind(),
                service_version: self.provisioner.service_version().to_string(),
            };

            // Capacity is enforced by the insert itself rather than by a
            // preceding count: a separate check-then-insert lets concurrent
            // spawns all observe the same free slot and overshoot the cap.
            // `CapacityFull` falls through to the catch-all arm below, which
            // releases the port just acquired and returns.
            match self
                .registry
                .save_within_caps(&candidate, self.max_provisioned, self.max_active)
            {
                Ok(()) => {
                    new_goopy = Some(candidate);
                    break;
                }
                Err(Error::AlreadyExists) => {
                    tracing::warn!(slug = %slug, "slug collision, retrying");
                    if let Err(rel_err) = self.registry.release_port(port) {
                        tracing::error!(
                            "spawn: release port {} on slug collision error: {:?}",
                            port,
                            rel_err
                        );
                    }
                    continue;
                }
                Err(e) => {
                    if let Err(rel_err) = self.registry.release_port(port) {
                        tracing::error!("spawn: release port {} error: {:?}", port, rel_err);
                    }
                    return Err(e);
                }
            }
        }

        let Some(new_goopy) = new_goopy else {
            return Err(Error::SlugExhausted);
        };
        let port = new_goopy.port;

        let slug = new_goopy.slug.clone();

        // now, spawn the job
        let registry = Arc::clone(&self.registry);
        let goopy_clone = new_goopy.clone();
        let provisioner = Arc::clone(&self.provisioner);
        let span = tracing::Span::current();

        std::thread::spawn(move || {
            let _guard = span.enter();
            match provisioner.provision(&goopy_clone) {
                Ok(_) => {
                    if let Err(e) = registry.update_status(&goopy_clone.slug, Status::Done) {
                        tracing::error!("spawning: update {} error: {:?}", goopy_clone.slug, e);
                    }
                }
                Err(err) => {
                    tracing::error!(
                        "provisioning for goopy: {} failed: {:?}",
                        goopy_clone.slug,
                        err
                    );

                    if let Err(e) = registry.release_port(port) {
                        tracing::error!("spawn: release port {} error: {:?}", port, e);
                    }
                    // This is the one moment the reason exists in full. The
                    // sweep will reap the `Failed` row and the log will rotate,
                    // so if it is not written down here it is gone (#118).
                    let event = InstanceEvent::failed(&goopy_clone.slug, EventPhase::Spawn, &err);
                    if let Err(e) = registry.fail_with_event(&goopy_clone.slug, &event) {
                        tracing::error!("spawning: update {} error: {:?}", goopy_clone.slug, e);
                    }
                }
            }
        });

        Ok((slug, port))
    }

    /// Despawn an instance without waiting for its teardown.
    ///
    /// Marks the row `Despawning` and hands the actual teardown to a thread, so
    /// an HTTP caller gets an immediate answer instead of waiting on
    /// `systemctl`, nginx and the storage allocator. The cost of that is that
    /// the outcome of the teardown is *not* in the return value: `Ok` means the
    /// despawn was accepted, not that the instance is gone. Callers that need
    /// the real outcome — the sweep, above all (#117) — must use
    /// [`despawn_blocking`] instead.
    ///
    /// [`despawn_blocking`]: GoopyManager::despawn_blocking
    #[tracing::instrument(skip(self))]
    pub fn despawn(&self, slug: String) -> Result<String, Error> {
        let goopy = self.begin_despawn(&slug)?;

        let registry = Arc::clone(&self.registry);
        let provisioner = Arc::clone(&self.provisioner);
        let span = tracing::Span::current();

        std::thread::spawn(move || {
            let _guard = span.enter();
            // The caller was already told `Ok`, so a failure here can only be
            // reported through the log, the row's restored `Failed` status and
            // the event `teardown` records against it.
            let _ = Self::teardown(&registry, &provisioner, &goopy, EventPhase::Despawn);
        });

        Ok(slug)
    }

    /// Despawn an instance and wait for its teardown to finish.
    ///
    /// The blocking counterpart of [`despawn`]: it returns once the instance is
    /// actually gone from the registry, or with the error that stopped it.
    ///
    /// Used by [`sweep`], which runs on a background task where nobody is
    /// waiting on a response, so the extra thread bought nothing — and cost the
    /// sweep any knowledge of whether the teardown worked.
    ///
    /// [`despawn`]: GoopyManager::despawn
    /// [`sweep`]: GoopyManager::sweep
    #[tracing::instrument(skip(self))]
    pub fn despawn_blocking(&self, slug: &str) -> Result<(), Error> {
        self.despawn_blocking_as(slug, EventPhase::Despawn)
    }

    /// [`despawn_blocking`], recording its events under `phase`.
    ///
    /// The teardown is identical whoever asked for it, but the event log's
    /// readers care who did: "what did the sweeper reclaim" and "what did
    /// somebody tear down by hand" are different questions, and once the row is
    /// gone the log is the only thing that can still tell them apart. So
    /// [`sweep`] goes through here with [`EventPhase::Sweep`] while the public
    /// entry point keeps [`EventPhase::Despawn`].
    ///
    /// [`despawn_blocking`]: GoopyManager::despawn_blocking
    /// [`sweep`]: GoopyManager::sweep
    fn despawn_blocking_as(&self, slug: &str, phase: EventPhase) -> Result<(), Error> {
        let goopy = self.begin_despawn(slug)?;
        Self::teardown(&self.registry, &self.provisioner, &goopy, phase)
    }

    /// Check that `slug` may be despawned and claim it by marking it
    /// `Despawning`, returning the row as it stood before the claim.
    fn begin_despawn(&self, slug: &str) -> Result<Goopy, Error> {
        let Some(goopy) = self.get(slug)? else {
            return Err(Error::NotFound);
        };

        if goopy.status == Status::Spawning || goopy.status == Status::Despawning {
            return Err(Error::Invalid);
        }

        // annotate the status
        self.registry.update_status(slug, Status::Despawning)?;

        Ok(goopy)
    }

    /// Release an instance's resources and settle its registry row.
    ///
    /// `Ok(())` means — and only means — that the row is gone from the
    /// registry, which is what lets [`sweep`] count reclaimed capacity rather
    /// than attempted reclamations.
    ///
    /// On failure the row is put back to `Failed` and its port is deliberately
    /// left reserved, so the stuck instance stays visible for investigation and
    /// a later despawn — by hand or by the next sweep — can retry it and
    /// release the port on success.
    ///
    /// What survives is the **row**, not the instance's data: both provisioners
    /// call `storage.release` unconditionally and only warn if it fails, so a
    /// teardown that errors afterwards has already removed the working
    /// directory. So a retry is usually cheaper than the first attempt rather
    /// than a repeat of it, and the `Failed` row is a record that something went
    /// wrong, not a preserved scene to inspect — #118 is what gives the failure
    /// a home that outlives the instance.
    ///
    /// [`sweep`]: GoopyManager::sweep
    fn teardown(
        registry: &Registry,
        provisioner: &Provisioner,
        goopy: &Goopy,
        phase: EventPhase,
    ) -> Result<(), Error> {
        // The `reaped` event and the delete commit together, so the event is
        // written exactly when the row actually leaves the registry — the same
        // signal `Ok(())` stands for, rather than a second, weaker one (#117).
        let torn_down = provisioner.deprovision(goopy).and_then(|()| {
            registry.delete_with_event(&goopy.slug, &InstanceEvent::reaped(&goopy.slug, phase))
        });

        match torn_down {
            Ok(()) => {
                // The row is gone, so the slot really was reclaimed. A port that
                // fails to return to the pool leaks one port — worth logging,
                // but it does not make the removal any less true.
                if let Err(e) = registry.release_port(goopy.port) {
                    tracing::error!(
                        slug = %goopy.slug,
                        port = goopy.port,
                        error = ?e,
                        "despawn: releasing the port failed",
                    );

                    // Recorded rather than only logged because this is the
                    // quietest failure in the codebase: the sweep still counts
                    // the row as swept, correctly, while the port range shrinks
                    // by one with nothing left pointing at it. The row it
                    // belonged to no longer exists, so the event log is the
                    // only place the leak can be attributed to a slug.
                    //
                    // `NotFound` is excluded because it is the *ordinary* case,
                    // not a leak: a spawn that failed released its own port
                    // before going `Failed`, so the sweep that later reaps that
                    // row finds nothing to release. Recording it would fill the
                    // log with leaks that never happened, which is the one
                    // thing a forensic record may not do.
                    if !matches!(e, Error::NotFound) {
                        let event = InstanceEvent::failed(&goopy.slug, phase, &e)
                            .during(format!("releasing port {}", goopy.port));
                        if let Err(record_err) = registry.record_event(&event) {
                            tracing::error!(
                                slug = %goopy.slug,
                                error = ?record_err,
                                "despawn: recording the leaked port failed",
                            );
                        }
                    }
                }
                Ok(())
            }
            Err(err) => {
                tracing::error!(
                    slug = %goopy.slug,
                    error = ?err,
                    "despawn: teardown failed, instance left Failed",
                );

                // Since #117 a failed delete puts the row back to `Failed`
                // rather than leaving it `Despawning`, where the sweep would
                // skip it forever. That made the failure visible; this makes it
                // legible, and keeps it legible after the retry that eventually
                // succeeds reaps the row.
                let event = InstanceEvent::failed(&goopy.slug, phase, &err);
                if let Err(e) = registry.fail_with_event(&goopy.slug, &event) {
                    tracing::error!(
                        slug = %goopy.slug,
                        error = ?e,
                        "despawn: restoring the Failed status failed",
                    );
                }
                Err(err)
            }
        }
    }

    pub fn get(&self, slug: &str) -> Result<Option<Goopy>, Error> {
        self.registry.load(slug)
    }

    pub fn list(&self) -> Result<Vec<Goopy>, Error> {
        self.registry.list()
    }

    /// Read up to `limit` recorded events, newest first, for one `slug` or for
    /// every instance.
    ///
    /// Unlike [`list`], this answers questions about instances that no longer
    /// exist — which is most of the interesting ones (#118).
    ///
    /// The results carry `detail`, which is operator-only: see
    /// [`InstanceEvent`]. Anything that renders these for a visitor must show
    /// `code` and nothing else.
    ///
    /// [`list`]: GoopyManager::list
    pub fn events(&self, slug: Option<&str>, limit: u32) -> Result<Vec<InstanceEvent>, Error> {
        self.registry.events(slug, limit)
    }

    /// Read the current usage of both caps.
    ///
    /// The two counts are read independently, so they are not a consistent
    /// snapshot of each other; that is acceptable because the result is
    /// advisory (see [`Capacity`]) and both counts move in the same direction
    /// during a spawn.
    pub fn capacity(&self) -> Result<Capacity, Error> {
        Ok(Capacity {
            active: self.registry.count_active()?,
            max_active: self.max_active,
            provisioned: self.registry.count_provisioned()?,
            max_provisioned: self.max_provisioned,
        })
    }

    /// Drop instance events older than `event_retention_days`.
    ///
    /// The event log is append-only, so without this it grows for the life of
    /// the host. The sweep enforces it because the sweep is already the
    /// periodic maintenance task; a second timer would be a second thing to
    /// configure, restart and forget.
    ///
    /// Deliberately not part of [`sweep`]'s `(swept, errors)` result. Those
    /// errors are per-instance reap failures, and gl-serv logs a sweep as
    /// failed when any are present — a housekeeping delete that could not run
    /// would then make a run that reclaimed everything read as a bad one. A
    /// prune that fails is logged and the sweep carries on; the only cost of
    /// missing one is a larger table at the next attempt.
    ///
    /// [`sweep`]: GoopyManager::sweep
    fn prune_events(&self, now: chrono::DateTime<Utc>) {
        let cutoff = now - Duration::days(self.event_retention_days as i64);

        match self.registry.prune_events_before(cutoff) {
            Ok(0) => {}
            Ok(pruned) => {
                tracing::info!(
                    pruned,
                    retention_days = self.event_retention_days,
                    "sweep: dropped expired instance events"
                );
            }
            Err(e) => {
                tracing::error!(
                    error = ?e,
                    retention_days = self.event_retention_days,
                    "sweep: pruning instance events failed",
                );
            }
        }
    }

    /// Despawn all expired goopy instances and reap all `Failed` instances.
    ///
    /// **Expired instances** are those where `now > created_at + life_in_days`.
    /// **Failed instances** are reaped regardless of age — a `Failed` status
    /// means provisioning failed and it is safe to delete.  `Suspended`
    /// instances are intentionally left alone; they hold valid data on disk and
    /// are managed separately.
    ///
    /// Instances with `Spawning` or `Despawning` status are skipped to avoid
    /// interfering with in-progress operations.
    ///
    /// Returns `(swept_count, per_instance_errors)`. `swept_count` is the
    /// number of instances **actually removed from the registry**, not the
    /// number of teardowns started: the sweep uses [`despawn_blocking`], so an
    /// instance that cannot be deprovisioned lands in the error list instead of
    /// the count, every run, for as long as it stays stuck. Errors are
    /// collected rather than aborting the sweep early.
    ///
    /// The distinction is the whole point of this method's log line — it is the
    /// only signal an operator has for whether capacity was reclaimed, so it
    /// must not read as a success on a run that freed nothing (#117).
    ///
    /// **Teardowns run one after another.** Knowing an outcome means waiting for
    /// it, so the parallelism the old detached-thread despawn gave us is gone by
    /// construction: reclamation latency now scales with the number of rows
    /// reaped in a pass, each paying for its own `systemctl` calls plus an
    /// `nginx -t` and a reload. That cost is what #109 (batch the reloads) buys
    /// back; until then a sweep over a full registry is the slow case to watch.
    ///
    /// Also enforces retention on the instance event log — see
    /// `prune_events`, which is deliberately outside the returned counts.
    ///
    /// Meant to be called periodically (e.g. via `tokio::time::interval` in
    /// `gl-serv`), from a context where blocking is acceptable.
    ///
    /// [`despawn_blocking`]: GoopyManager::despawn_blocking
    #[tracing::instrument(skip(self))]
    pub fn sweep(&self) -> Result<(u32, Vec<Error>), Error> {
        let now = Utc::now();
        self.prune_events(now);
        let goopies = self.list()?;
        let mut swept = 0u32;
        let mut errors: Vec<Error> = Vec::new();

        for gp in goopies {
            if gp.status == Status::Spawning || gp.status == Status::Despawning {
                continue;
            }

            let should_reap = if gp.status == Status::Failed {
                tracing::info!(
                    slug = %gp.slug,
                    "sweeping Failed instance"
                );
                true
            } else {
                let expires_at = gp.created_at + Duration::days(gp.life_in_days as i64);
                if now > expires_at {
                    tracing::info!(
                        slug = %gp.slug,
                        status = %gp.status,
                        expired_at = %expires_at,
                        "sweeping expired instance"
                    );
                    true
                } else {
                    false
                }
            };

            if should_reap {
                match self.despawn_blocking_as(&gp.slug, EventPhase::Sweep) {
                    Ok(()) => swept += 1,
                    Err(e) => {
                        tracing::error!(
                            slug = %gp.slug,
                            error = %e,
                            "sweep: instance could not be removed",
                        );
                        errors.push(e);
                    }
                }
            }
        }

        // This is the sweep's single log line — `gl-serv` deliberately does not
        // log the same outcome again, so that grepping `sweep complete` returns
        // one record per run. `failed` is logged unconditionally, and a run that
        // failed anything is a warning, so a run that reclaimed nothing cannot
        // be mistaken for a healthy one at a glance.
        if errors.is_empty() {
            tracing::info!(swept, failed = 0, "sweep complete");
        } else {
            tracing::warn!(swept, failed = errors.len(), "sweep complete");
        }
        Ok((swept, errors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goopy::Goopy;
    use crate::goopy_provisioner::GoopyProvisioner;
    use crate::goopy_registry::GoopyRegistry;
    use crate::goopy_registry::sqlite_registry::SqliteRegistry;
    use crate::storage_allocator::{PlainDirAllocator, StorageAllocator};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    struct CollideOnceRegistry {
        save_calls: Mutex<u32>,
        release_calls: Arc<Mutex<u32>>,
    }

    impl GoopyRegistry for CollideOnceRegistry {
        fn save(&self, _gp: &Goopy) -> Result<(), Error> {
            let mut n = self.save_calls.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Err(Error::AlreadyExists)
            } else {
                Ok(())
            }
        }
        fn load(&self, _slug: &str) -> Result<Option<Goopy>, Error> {
            Ok(None)
        }
        fn delete(&self, _slug: &str) -> Result<(), Error> {
            Ok(())
        }
        fn list(&self) -> Result<Vec<Goopy>, Error> {
            Ok(vec![])
        }
        fn update_status(&self, _slug: &str, _new_status: Status) -> Result<(), Error> {
            Ok(())
        }
        fn acquire_port(
            &self,
            _slug: &str,
            range_start: u32,
            _range_end: u32,
        ) -> Result<u32, Error> {
            Ok(range_start)
        }
        fn release_port(&self, _port: u32) -> Result<(), Error> {
            *self.release_calls.lock().unwrap() += 1;
            Ok(())
        }
        fn count_provisioned(&self) -> Result<u32, Error> {
            Ok(0)
        }
        fn count_active(&self) -> Result<u32, Error> {
            Ok(0)
        }
        /// Never capacity-limited; defers to `save` so the collision-then-retry
        /// behaviour this double exists to exercise still applies.
        fn save_within_caps(&self, gp: &Goopy, _: u32, _: u32) -> Result<(), Error> {
            self.save(gp)
        }

        // The event log is irrelevant to the slug-collision retry this double
        // exists for, so it accepts writes and reads back nothing.
        fn record_event(&self, _event: &InstanceEvent) -> Result<(), Error> {
            Ok(())
        }
        fn fail_with_event(&self, _slug: &str, _event: &InstanceEvent) -> Result<(), Error> {
            Ok(())
        }
        fn delete_with_event(&self, _slug: &str, _event: &InstanceEvent) -> Result<(), Error> {
            Ok(())
        }
        fn prune_events_before(&self, _cutoff: chrono::DateTime<Utc>) -> Result<u32, Error> {
            Ok(0)
        }
        fn events(&self, _slug: Option<&str>, _limit: u32) -> Result<Vec<InstanceEvent>, Error> {
            Ok(vec![])
        }
    }

    struct NoopProvisioner;

    impl GoopyProvisioner for NoopProvisioner {
        fn provision(&self, _goopy: &Goopy) -> Result<(), Error> {
            Ok(())
        }
        fn deprovision(&self, _goopy: &Goopy) -> Result<(), Error> {
            Ok(())
        }
        fn kind(&self) -> ProvisionerKind {
            ProvisionerKind::Hello
        }
        fn service_version(&self) -> &str {
            "9.9.9-mock"
        }
    }

    fn make_test_manager(
        registry: SqliteRegistry,
    ) -> GoopyManager<SqliteRegistry, NoopProvisioner> {
        GoopyManager::new(
            GoopyManagerConfig {
                base_dir: PathBuf::from("/tmp"),
                domain: "test.example".into(),
                life_in_days: 7,
                port_range_start: 9000,
                port_range_end: 9100,
                max_active: 100,
                max_provisioned: 100,
                event_retention_days: 30,
            },
            registry,
            NoopProvisioner,
        )
    }

    fn make_goopy(slug: &str, days_ago: i64, port: u32, status: Status) -> Goopy {
        Goopy {
            slug: slug.to_string(),
            life_in_days: 7,
            created_at: Utc::now() - Duration::days(days_ago),
            working_dir: PathBuf::from(format!("/tmp/{slug}")),
            port,
            status,
            provisioner_kind: ProvisionerKind::Hello,
            service_version: "0.1.0".to_string(),
        }
    }

    #[test]
    fn spawn_rejects_non_positive_life_in_days() {
        for bad in [0i32, -1, i32::MIN] {
            let gm = GoopyManager::new(
                GoopyManagerConfig {
                    base_dir: PathBuf::from("/tmp"),
                    domain: "test.example".into(),
                    life_in_days: bad,
                    port_range_start: 9000,
                    port_range_end: 9100,
                    max_active: 100,
                    max_provisioned: 100,
                    event_retention_days: 30,
                },
                SqliteRegistry::new(Path::new(":memory:")).unwrap(),
                NoopProvisioner,
            );
            let err = gm.spawn().unwrap_err();
            assert!(
                matches!(err, Error::Invalid),
                "expected Invalid for life_in_days={bad}"
            );
        }
    }

    #[test]
    fn spawn_retries_on_collision() {
        let release_calls = Arc::new(Mutex::new(0u32));
        let gm = GoopyManager::new(
            GoopyManagerConfig {
                base_dir: PathBuf::from("/tmp/test-goopy"),
                domain: "test.example".into(),
                life_in_days: 7,
                port_range_start: 8080,
                port_range_end: 9080,
                max_active: 100,
                max_provisioned: 100,
                event_retention_days: 30,
            },
            CollideOnceRegistry {
                save_calls: Mutex::new(0),
                release_calls: Arc::clone(&release_calls),
            },
            NoopProvisioner,
        );

        // First save returns AlreadyExists; spawn must retry and succeed on the second attempt.
        let result = gm.spawn();
        assert!(
            result.is_ok(),
            "spawn should succeed after retrying a slug collision"
        );
        // The port acquired for the colliding slug must have been released before retrying.
        assert_eq!(
            *release_calls.lock().unwrap(),
            1,
            "release_port should be called once on slug collision"
        );
    }

    #[test]
    fn sweep_removes_expired_instances() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();

        // Insert an expired goopy: created 10 days ago, lives 7 days
        let expired = make_goopy("expired-slug", 10, 9000, Status::Done);
        registry.save(&expired).unwrap();
        registry.acquire_port("expired-slug", 9000, 9001).unwrap();

        // Insert a non-expired goopy: created now, lives 7 days
        let alive = make_goopy("alive-slug", 0, 9001, Status::Done);
        registry.save(&alive).unwrap();
        registry.acquire_port("alive-slug", 9001, 9002).unwrap();

        let gm = make_test_manager(registry);

        let (swept, errors) = gm.sweep().unwrap();
        assert_eq!(swept, 1);
        assert!(errors.is_empty());

        // Wait for the despawn background thread to finish
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while gm.get("expired-slug").unwrap().is_some() {
            assert!(std::time::Instant::now() < deadline, "despawn timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Expired should be gone
        assert!(gm.get("expired-slug").unwrap().is_none());
        // Alive should remain
        assert!(gm.get("alive-slug").unwrap().is_some());
    }

    #[test]
    fn sweep_skips_in_progress_instances() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();

        // Insert an expired goopy with Spawning status — should be skipped
        let spawning = make_goopy("spawning-slug", 10, 9000, Status::Spawning);
        registry.save(&spawning).unwrap();
        registry.acquire_port("spawning-slug", 9000, 9001).unwrap();

        // Insert an expired goopy with Despawning status — should be skipped
        let despawning = make_goopy("despawning-slug", 10, 9001, Status::Despawning);
        registry.save(&despawning).unwrap();
        registry
            .acquire_port("despawning-slug", 9001, 9002)
            .unwrap();

        let gm = make_test_manager(registry);

        let (swept, errors) = gm.sweep().unwrap();
        assert_eq!(swept, 0);
        assert!(errors.is_empty());

        // Both should still exist
        assert!(gm.get("spawning-slug").unwrap().is_some());
        assert!(gm.get("despawning-slug").unwrap().is_some());
    }

    #[test]
    fn sweep_no_expired_instances() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();

        // Insert a non-expired goopy
        let alive = make_goopy("fresh-slug", 0, 9000, Status::Done);
        registry.save(&alive).unwrap();
        registry.acquire_port("fresh-slug", 9000, 9001).unwrap();

        let gm = make_test_manager(registry);

        let (swept, errors) = gm.sweep().unwrap();
        assert_eq!(swept, 0);
        assert!(errors.is_empty());
        assert!(gm.get("fresh-slug").unwrap().is_some());
    }

    #[test]
    fn sweep_collects_despawn_errors() {
        struct FailingUpdateRegistry(SqliteRegistry);
        impl GoopyRegistry for FailingUpdateRegistry {
            fn save(&self, gp: &Goopy) -> Result<(), Error> {
                self.0.save(gp)
            }
            fn load(&self, slug: &str) -> Result<Option<Goopy>, Error> {
                self.0.load(slug)
            }
            fn delete(&self, slug: &str) -> Result<(), Error> {
                self.0.delete(slug)
            }
            fn list(&self) -> Result<Vec<Goopy>, Error> {
                self.0.list()
            }
            fn update_status(&self, _: &str, _: Status) -> Result<(), Error> {
                Err(Error::Invalid)
            }
            fn acquire_port(&self, slug: &str, s: u32, e: u32) -> Result<u32, Error> {
                self.0.acquire_port(slug, s, e)
            }
            fn release_port(&self, p: u32) -> Result<(), Error> {
                self.0.release_port(p)
            }
            fn count_provisioned(&self) -> Result<u32, Error> {
                self.0.count_provisioned()
            }
            fn count_active(&self) -> Result<u32, Error> {
                self.0.count_active()
            }
            fn save_within_caps(&self, gp: &Goopy, mp: u32, ma: u32) -> Result<(), Error> {
                self.0.save_within_caps(gp, mp, ma)
            }
            /// Fails for the same reason `update_status` does: this double
            /// refuses every status write, whichever door it comes through.
            fn fail_with_event(&self, _: &str, _: &InstanceEvent) -> Result<(), Error> {
                Err(Error::Invalid)
            }
            fn record_event(&self, event: &InstanceEvent) -> Result<(), Error> {
                self.0.record_event(event)
            }
            fn delete_with_event(&self, slug: &str, event: &InstanceEvent) -> Result<(), Error> {
                self.0.delete_with_event(slug, event)
            }
            fn prune_events_before(&self, cutoff: chrono::DateTime<Utc>) -> Result<u32, Error> {
                self.0.prune_events_before(cutoff)
            }
            fn events(&self, slug: Option<&str>, limit: u32) -> Result<Vec<InstanceEvent>, Error> {
                self.0.events(slug, limit)
            }
        }

        let inner = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        let expired = make_goopy("err-slug", 10, 9000, Status::Done);
        inner.save(&expired).unwrap();
        inner.acquire_port("err-slug", 9000, 9001).unwrap();

        let gm = GoopyManager::new(
            GoopyManagerConfig {
                base_dir: PathBuf::from("/tmp"),
                domain: "test.example".into(),
                life_in_days: 7,
                port_range_start: 9000,
                port_range_end: 9100,
                max_active: 100,
                max_provisioned: 100,
                event_retention_days: 30,
            },
            FailingUpdateRegistry(inner),
            NoopProvisioner,
        );

        let (swept, errors) = gm.sweep().unwrap();
        assert_eq!(swept, 0);
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], Error::Invalid));
    }

    // ── spawn / get / list / despawn state machine ────────────────────────

    #[test]
    fn spawn_returns_slug_and_port() {
        let gm = make_test_manager(SqliteRegistry::new(Path::new(":memory:")).unwrap());
        let (slug, port) = gm.spawn().expect("spawn should succeed");
        assert!(!slug.is_empty(), "slug should be non-empty");
        assert!(
            (9000..9100).contains(&port),
            "port should be in configured range"
        );
    }

    #[test]
    fn get_finds_goopy_after_spawn() {
        let gm = make_test_manager(SqliteRegistry::new(Path::new(":memory:")).unwrap());
        let (slug, _) = gm.spawn().unwrap();
        let g = gm.get(&slug).unwrap().expect("should find goopy");
        // NoopProvisioner completes synchronously, so status may be Done already.
        assert!(
            g.status == Status::Spawning || g.status == Status::Done,
            "status should be Spawning or Done, got {:?}",
            g.status
        );
        // The sentinel is deliberately unlike any crate version, so this fails if
        // spawn() ever goes back to stamping env!("CARGO_PKG_VERSION").
        assert_eq!(
            g.service_version, "9.9.9-mock",
            "service_version must come from the provisioner, not the crate version"
        );
    }

    #[test]
    fn get_missing_returns_none() {
        let gm = make_test_manager(SqliteRegistry::new(Path::new(":memory:")).unwrap());
        assert!(gm.get("no-such-slug").unwrap().is_none());
    }

    #[test]
    fn list_returns_spawned_instances() {
        let gm = make_test_manager(SqliteRegistry::new(Path::new(":memory:")).unwrap());
        let (slug1, _) = gm.spawn().unwrap();
        let (slug2, _) = gm.spawn().unwrap();
        let goopies = gm.list().unwrap();
        let slugs: Vec<&str> = goopies.iter().map(|g| g.slug.as_str()).collect();
        assert!(slugs.contains(&slug1.as_str()), "should contain slug1");
        assert!(slugs.contains(&slug2.as_str()), "should contain slug2");
    }

    #[test]
    fn despawn_removes_goopy_after_deprovision() {
        // Use a real registry so status transitions are persisted.
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        let gm = make_test_manager(registry);

        let (slug, _) = gm.spawn().unwrap();

        // Wait for the spawn background thread to finish by polling registry status.
        // NoopProvisioner is instant so this usually completes on the first check.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let g = gm.get(&slug).unwrap().unwrap();
            if g.status == Status::Done || g.status == Status::Failed {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "spawn timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Despawn it
        gm.despawn(slug.clone()).expect("despawn should succeed");

        // Give the despawn thread time to finish
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while gm.get(&slug).unwrap().is_some() {
            assert!(std::time::Instant::now() < deadline, "despawn timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(
            gm.get(&slug).unwrap().is_none(),
            "should be gone after despawn"
        );
    }

    #[test]
    fn despawn_missing_returns_not_found() {
        let gm = make_test_manager(SqliteRegistry::new(Path::new(":memory:")).unwrap());
        let err = gm.despawn("no-such".to_string()).unwrap_err();
        assert!(matches!(err, Error::NotFound));
    }

    /// Provisioner that delegates storage cleanup to `PlainDirAllocator` so that
    /// directory removal is exercised during deprovision.
    struct DirCleaningProvisioner;

    impl GoopyProvisioner for DirCleaningProvisioner {
        fn provision(&self, _goopy: &Goopy) -> Result<(), Error> {
            Ok(())
        }

        fn deprovision(&self, goopy: &Goopy) -> Result<(), Error> {
            // Tolerant of a missing directory (matches PlainDirAllocator semantics).
            PlainDirAllocator.release(&goopy.working_dir)
        }

        fn kind(&self) -> ProvisionerKind {
            ProvisionerKind::Hello
        }
        fn service_version(&self) -> &str {
            "9.9.9-mock"
        }
    }

    /// sweep() must reap `Failed` instances: release their port and remove their
    /// working directory.  The instance should be deleted from the registry and
    /// its port should be returned to the pool so it can be re-acquired.
    #[test]
    fn sweep_reaps_failed_instances() {
        let base_dir = tempfile::tempdir().expect("tempdir");
        let working_dir = base_dir.path().join("failed-slug");

        // Create the working directory to simulate a partial provision.
        std::fs::create_dir_all(&working_dir).unwrap();
        assert!(working_dir.exists(), "working dir must exist before sweep");

        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();

        // Seed a Failed instance directly, bypassing the spawn flow.
        let failed = Goopy {
            slug: "failed-slug".to_string(),
            life_in_days: 7,
            created_at: Utc::now(),
            working_dir: working_dir.clone(),
            port: 9050,
            status: Status::Failed,
            provisioner_kind: ProvisionerKind::Hello,
            service_version: "0.1.0".to_string(),
        };
        registry.save(&failed).unwrap();
        // Register the port so we can verify it gets released.
        registry.acquire_port("failed-slug", 9050, 9051).unwrap();

        let gm = GoopyManager::new(
            GoopyManagerConfig {
                base_dir: base_dir.path().to_path_buf(),
                domain: "test.example".into(),
                life_in_days: 7,
                port_range_start: 9050,
                port_range_end: 9051,
                max_active: 100,
                max_provisioned: 100,
                event_retention_days: 30,
            },
            registry,
            DirCleaningProvisioner,
        );

        let (reaped, errors) = gm.sweep().unwrap();
        assert_eq!(reaped, 1, "one Failed instance should be reaped");
        assert!(errors.is_empty(), "no sweep errors expected");

        // Wait for the despawn background thread to finish.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while gm.get("failed-slug").unwrap().is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "despawn timed out waiting for Failed instance to be removed"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Instance must be gone from the registry.
        assert!(
            gm.get("failed-slug").unwrap().is_none(),
            "Failed instance must be deleted from registry after sweep"
        );

        // Working directory must be removed.
        assert!(
            !working_dir.exists(),
            "working directory must be removed after sweeping a Failed instance"
        );

        // Port 9050 must be released — acquiring it again should succeed.
        let re_acquired = gm
            .registry
            .acquire_port("new-slug", 9050, 9051)
            .expect("port 9050 should be available after the Failed instance is reaped");
        assert_eq!(re_acquired, 9050, "the freed port should be re-acquirable");
    }

    /// sweep() must leave `Done` instances that have not yet expired untouched,
    /// even if other instances are Failed and get reaped in the same pass.
    #[test]
    fn sweep_reaps_failed_but_leaves_healthy_instances() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();

        // A Failed instance — should be reaped.
        let failed = make_goopy("fail-one", 0, 9060, Status::Failed);
        registry.save(&failed).unwrap();
        registry.acquire_port("fail-one", 9060, 9061).unwrap();

        // A healthy Done instance that has not expired — should survive.
        let healthy = make_goopy("alive-one", 0, 9061, Status::Done);
        registry.save(&healthy).unwrap();
        registry.acquire_port("alive-one", 9061, 9062).unwrap();

        let gm = GoopyManager::new(
            GoopyManagerConfig {
                base_dir: PathBuf::from("/tmp"),
                domain: "test.example".into(),
                life_in_days: 7,
                port_range_start: 9060,
                port_range_end: 9062,
                max_active: 100,
                max_provisioned: 100,
                event_retention_days: 30,
            },
            registry,
            NoopProvisioner,
        );

        let (reaped, errors) = gm.sweep().unwrap();
        assert_eq!(reaped, 1, "only the Failed instance should be reaped");
        assert!(errors.is_empty());

        // Wait for despawn background thread.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while gm.get("fail-one").unwrap().is_some() {
            assert!(std::time::Instant::now() < deadline, "despawn timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(
            gm.get("fail-one").unwrap().is_none(),
            "Failed instance should be gone"
        );
        assert!(
            gm.get("alive-one").unwrap().is_some(),
            "healthy Done instance should remain"
        );
    }

    // ── the sweep tells the truth (#117) ──────────────────────────────────

    /// A provisioner whose teardown always fails, reproducing the real fault:
    /// `systemctl stop` on a unit that was never created. The row can never be
    /// removed, so no number of sweeps may ever report it as swept.
    struct UndeprovisionableProvisioner {
        deprovision_calls: Arc<Mutex<u32>>,
    }

    impl GoopyProvisioner for UndeprovisionableProvisioner {
        fn provision(&self, _goopy: &Goopy) -> Result<(), Error> {
            Ok(())
        }
        fn deprovision(&self, _goopy: &Goopy) -> Result<(), Error> {
            *self.deprovision_calls.lock().unwrap() += 1;
            Err(Error::Subprocess("systemctl stop: Unit not found".into()))
        }
        fn kind(&self) -> ProvisionerKind {
            ProvisionerKind::Hello
        }
        fn service_version(&self) -> &str {
            "9.9.9-mock"
        }
    }

    /// Fails the teardown for one slug and succeeds for every other, so a
    /// single sweep can contain both outcomes.
    struct FailsOneSlugProvisioner {
        doomed: String,
    }

    impl GoopyProvisioner for FailsOneSlugProvisioner {
        fn provision(&self, _goopy: &Goopy) -> Result<(), Error> {
            Ok(())
        }
        fn deprovision(&self, goopy: &Goopy) -> Result<(), Error> {
            if goopy.slug == self.doomed {
                Err(Error::Subprocess("systemctl stop: Unit not found".into()))
            } else {
                Ok(())
            }
        }
        fn kind(&self) -> ProvisionerKind {
            ProvisionerKind::Hello
        }
        fn service_version(&self) -> &str {
            "9.9.9-mock"
        }
    }

    /// The one place a test `GoopyManager` is built. `manager_with_provisioner`
    /// and `manager_with_caps` are the two narrow views onto it — every test
    /// varies either the provisioner or the caps, never both.
    fn manager_with<
        R: GoopyRegistry + Send + Sync + 'static,
        P: GoopyProvisioner + Send + Sync + 'static,
    >(
        registry: R,
        provisioner: P,
        max_active: u32,
        max_provisioned: u32,
    ) -> GoopyManager<R, P> {
        GoopyManager::new(
            GoopyManagerConfig {
                base_dir: PathBuf::from("/tmp"),
                domain: "test.example".into(),
                life_in_days: 7,
                port_range_start: 9000,
                port_range_end: 9100,
                max_active,
                max_provisioned,
                event_retention_days: 30,
            },
            registry,
            provisioner,
        )
    }

    /// Caps high enough to stay out of the way; the provisioner is the variable.
    fn manager_with_provisioner<P: GoopyProvisioner + Send + Sync + 'static>(
        registry: SqliteRegistry,
        provisioner: P,
    ) -> GoopyManager<SqliteRegistry, P> {
        manager_with(registry, provisioner, 100, 100)
    }

    /// The defect this issue was filed about: a sweep in which every teardown
    /// fails freed nothing, so it must report nothing — and must surface the
    /// failures rather than swallowing them in a background thread.
    #[test]
    fn sweep_reports_zero_when_every_deprovision_fails() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "stuck-one", 9000, Status::Failed);
        seed_row(&registry, "stuck-two", 9001, Status::Failed);

        let calls = Arc::new(Mutex::new(0u32));
        let gm = manager_with_provisioner(
            registry,
            UndeprovisionableProvisioner {
                deprovision_calls: Arc::clone(&calls),
            },
        );

        let (swept, errors) = gm.sweep().unwrap();

        assert_eq!(swept, 0, "no slot was reclaimed, so none may be reported");
        assert_eq!(errors.len(), 2, "both failures must reach the caller");
        assert_eq!(*calls.lock().unwrap(), 2, "both rows must be attempted");

        // Both rows survive, back in `Failed` so a later sweep retries them.
        for slug in ["stuck-one", "stuck-two"] {
            let row = gm.get(slug).unwrap().expect("stuck row must survive");
            assert_eq!(
                row.status,
                Status::Failed,
                "{slug} must be left Failed, not Despawning, so the sweep can retry it",
            );
        }
    }

    /// The count must follow the registry, not the attempts: one removal and
    /// one stuck row is `swept = 1` with one error, never `swept = 2`.
    #[test]
    fn sweep_counts_only_the_rows_it_actually_removed() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "reapable", 9000, Status::Failed);
        seed_row(&registry, "stuck", 9001, Status::Failed);

        let gm = manager_with_provisioner(
            registry,
            FailsOneSlugProvisioner {
                doomed: "stuck".into(),
            },
        );

        let (swept, errors) = gm.sweep().unwrap();

        assert_eq!(swept, 1, "only the row that left the registry counts");
        assert_eq!(errors.len(), 1);
        assert!(
            gm.get("reapable").unwrap().is_none(),
            "sweep must not return before the removal it counted has happened",
        );
        assert!(gm.get("stuck").unwrap().is_some(), "the stuck row survives");
    }

    /// The failure that hid the bug for two and a half months: the same row,
    /// swept over and over, logging success each time. Every run must now read
    /// as zero reclaimed.
    #[test]
    fn repeated_sweeps_never_report_a_permanently_stuck_row_as_swept() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "wedged", 9000, Status::Failed);

        let calls = Arc::new(Mutex::new(0u32));
        let gm = manager_with_provisioner(
            registry,
            UndeprovisionableProvisioner {
                deprovision_calls: Arc::clone(&calls),
            },
        );

        for run in 1..=3 {
            let (swept, errors) = gm.sweep().unwrap();
            assert_eq!(swept, 0, "run {run} reclaimed nothing");
            assert_eq!(errors.len(), 1, "run {run} must report the failure");
        }

        assert_eq!(
            *calls.lock().unwrap(),
            3,
            "each sweep must retry the wedged row",
        );
        assert!(gm.get("wedged").unwrap().is_some());
    }

    /// The count is only meaningful if the removal has already happened when
    /// `sweep` returns — no polling, no grace period.
    #[test]
    fn sweep_removes_the_row_before_it_returns() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "doomed", 9000, Status::Failed);

        let gm = manager_with_provisioner(registry, NoopProvisioner);

        let (swept, errors) = gm.sweep().unwrap();

        assert_eq!(swept, 1);
        assert!(errors.is_empty());
        assert!(
            gm.get("doomed").unwrap().is_none(),
            "the row must be gone the instant sweep returns",
        );
    }

    /// A teardown that blocks forever would stall the sweep, but must never
    /// stall the HTTP handler: `despawn` stays fire-and-forget.
    #[test]
    fn despawn_returns_before_its_teardown_finishes() {
        use std::sync::mpsc;

        struct GatedProvisioner {
            entered: mpsc::SyncSender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl GoopyProvisioner for GatedProvisioner {
            fn provision(&self, _goopy: &Goopy) -> Result<(), Error> {
                Ok(())
            }
            fn deprovision(&self, _goopy: &Goopy) -> Result<(), Error> {
                self.entered.send(()).expect("test receiver must be alive");
                self.release
                    .lock()
                    .unwrap()
                    .recv()
                    .expect("test sender must be alive");
                Ok(())
            }
            fn kind(&self) -> ProvisionerKind {
                ProvisionerKind::Hello
            }
            fn service_version(&self) -> &str {
                "9.9.9-mock"
            }
        }

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel();

        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "slow-teardown", 9000, Status::Done);
        let gm = manager_with_provisioner(
            registry,
            GatedProvisioner {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            },
        );

        gm.despawn("slow-teardown".to_string())
            .expect("despawn should be accepted immediately");

        // The teardown thread is now parked inside `deprovision`, so the row
        // cannot have been removed yet — proving `despawn` did not wait.
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the teardown thread should have reached deprovision");
        let row = gm
            .get("slow-teardown")
            .unwrap()
            .expect("the row must still exist while the teardown is in flight");
        assert_eq!(row.status, Status::Despawning);

        release_tx.send(()).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while gm.get("slow-teardown").unwrap().is_some() {
            assert!(std::time::Instant::now() < deadline, "despawn timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // ── the failure outlives the instance (#118) ──────────────────────────

    /// A provisioner whose `provision` always fails, reproducing a spawn that
    /// ends `Failed` — the state the June droplet was stuck in ten times over.
    struct UnprovisionableProvisioner;

    impl GoopyProvisioner for UnprovisionableProvisioner {
        fn provision(&self, _goopy: &Goopy) -> Result<(), Error> {
            Err(Error::Subprocess(
                "ghost install: EACCES /opt/goopy-life/data".into(),
            ))
        }
        fn deprovision(&self, _goopy: &Goopy) -> Result<(), Error> {
            Ok(())
        }
        fn kind(&self) -> ProvisionerKind {
            ProvisionerKind::Hello
        }
        fn service_version(&self) -> &str {
            "9.9.9-mock"
        }
    }

    /// Block until `slug` leaves `Spawning`, so the spawn thread's writes are
    /// visible before the assertions run.
    fn wait_for_spawn_to_settle<R, P>(gm: &GoopyManager<R, P>, slug: &str)
    where
        R: GoopyRegistry + Send + Sync + 'static,
        P: GoopyProvisioner + Send + Sync + 'static,
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match gm.get(slug).unwrap() {
                Some(g) if g.status != Status::Spawning => break,
                _ => {}
            }
            assert!(std::time::Instant::now() < deadline, "spawn timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_failed_spawn_records_the_reason() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        let gm = manager_with_provisioner(registry, UnprovisionableProvisioner);

        let (slug, _) = gm.spawn().unwrap();
        wait_for_spawn_to_settle(&gm, &slug);

        assert_eq!(gm.get(&slug).unwrap().unwrap().status, Status::Failed);

        let events = gm.events(Some(&slug), 10).unwrap();
        assert_eq!(events.len(), 1, "expected one event, got {events:?}");
        assert_eq!(events[0].phase, EventPhase::Spawn);
        assert_eq!(events[0].outcome, EventOutcome::Failed);
        assert_eq!(events[0].code, "subprocess");
        assert!(
            events[0]
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("EACCES")),
            "the operator-only detail must keep the stderr: {events:?}"
        );
    }

    #[test]
    fn a_failed_teardown_records_the_reason() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "stuck", 9000, Status::Failed);

        let gm = manager_with_provisioner(
            registry,
            UndeprovisionableProvisioner {
                deprovision_calls: Arc::new(Mutex::new(0)),
            },
        );

        gm.despawn_blocking("stuck").unwrap_err();

        let events = gm.events(Some("stuck"), 10).unwrap();
        assert_eq!(events.len(), 1, "expected one event, got {events:?}");
        assert_eq!(events[0].phase, EventPhase::Despawn);
        assert_eq!(events[0].outcome, EventOutcome::Failed);
        assert_eq!(events[0].code, "subprocess");
        assert_eq!(
            gm.get("stuck").unwrap().unwrap().status,
            Status::Failed,
            "the row goes back to Failed, and now says why"
        );
    }

    /// The sweep retries every `Failed` row, so an instance that cannot be torn
    /// down fails on every run. Recording each attempt would bury every other
    /// instance's failures under this one slug.
    #[test]
    fn a_stuck_instance_is_recorded_once_across_sweeps() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "stuck", 9000, Status::Failed);

        let deprovision_calls = Arc::new(Mutex::new(0));
        let gm = manager_with_provisioner(
            registry,
            UndeprovisionableProvisioner {
                deprovision_calls: deprovision_calls.clone(),
            },
        );

        gm.sweep().unwrap();
        gm.sweep().unwrap();

        assert_eq!(*deprovision_calls.lock().unwrap(), 2, "both sweeps retried");
        let events = gm.events(Some("stuck"), 10).unwrap();
        assert_eq!(events.len(), 1, "one reason, not one per sweep: {events:?}");
        assert_eq!(events[0].phase, EventPhase::Sweep);
        assert_eq!(gm.get("stuck").unwrap().unwrap().status, Status::Failed);
    }

    /// The sweep's `reaped` event hangs off `despawn_blocking`'s completion
    /// signal — the one #117 added — so it can only be written when the row
    /// really left the registry.
    #[test]
    fn the_sweeper_records_what_it_reaped() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        let expired = make_goopy("reaped-one", 10, 9000, Status::Done);
        registry.save(&expired).unwrap();
        registry.acquire_port("reaped-one", 9000, 9001).unwrap();

        let gm = manager_with_provisioner(registry, NoopProvisioner);

        let (swept, errors) = gm.sweep().unwrap();
        assert_eq!(swept, 1);
        assert!(errors.is_empty());
        assert!(gm.get("reaped-one").unwrap().is_none());

        let events = gm.events(Some("reaped-one"), 10).unwrap();
        assert_eq!(events.len(), 1, "expected one event, got {events:?}");
        assert_eq!(events[0].outcome, EventOutcome::Reaped);
        assert_eq!(events[0].phase, EventPhase::Sweep);
        assert_eq!(events[0].code, InstanceEvent::NO_ERROR);
    }

    /// A sweep that removed nothing must record nothing — the same guarantee
    /// #117 gave the log line, now for the durable copy.
    #[test]
    fn a_sweep_that_reclaims_nothing_records_no_reap() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "stuck", 9000, Status::Failed);

        let gm = manager_with_provisioner(
            registry,
            UndeprovisionableProvisioner {
                deprovision_calls: Arc::new(Mutex::new(0)),
            },
        );

        let (swept, errors) = gm.sweep().unwrap();
        assert_eq!(swept, 0);
        assert_eq!(errors.len(), 1);

        let events = gm.events(Some("stuck"), 10).unwrap();
        assert!(
            events.iter().all(|e| e.outcome != EventOutcome::Reaped),
            "nothing was reclaimed, so no reap may be recorded: {events:?}"
        );
    }

    /// The sweeper and a hand-driven despawn run the same teardown, and only
    /// the phase can tell them apart once the row is gone.
    #[test]
    fn a_despawn_is_recorded_as_a_despawn_not_a_sweep() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "by-hand", 9000, Status::Done);

        let gm = manager_with_provisioner(registry, NoopProvisioner);
        gm.despawn_blocking("by-hand").unwrap();

        let events = gm.events(Some("by-hand"), 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].phase, EventPhase::Despawn);
        assert_eq!(events[0].outcome, EventOutcome::Reaped);
    }

    /// A registry that cannot give a port back, standing in for a write that
    /// fails for a reason other than the allocation already being gone.
    struct UnreleasablePortRegistry(SqliteRegistry);

    impl GoopyRegistry for UnreleasablePortRegistry {
        fn release_port(&self, _port: u32) -> Result<(), Error> {
            Err(Error::Registry {
                context: "release port",
                source: RegistrySource::WalModeUnavailable("delete".into()),
            })
        }
        fn save(&self, gp: &Goopy) -> Result<(), Error> {
            self.0.save(gp)
        }
        fn load(&self, slug: &str) -> Result<Option<Goopy>, Error> {
            self.0.load(slug)
        }
        fn delete(&self, slug: &str) -> Result<(), Error> {
            self.0.delete(slug)
        }
        fn list(&self) -> Result<Vec<Goopy>, Error> {
            self.0.list()
        }
        fn update_status(&self, slug: &str, status: Status) -> Result<(), Error> {
            self.0.update_status(slug, status)
        }
        fn acquire_port(&self, slug: &str, s: u32, e: u32) -> Result<u32, Error> {
            self.0.acquire_port(slug, s, e)
        }
        fn count_provisioned(&self) -> Result<u32, Error> {
            self.0.count_provisioned()
        }
        fn count_active(&self) -> Result<u32, Error> {
            self.0.count_active()
        }
        fn save_within_caps(&self, gp: &Goopy, mp: u32, ma: u32) -> Result<(), Error> {
            self.0.save_within_caps(gp, mp, ma)
        }
        fn record_event(&self, event: &InstanceEvent) -> Result<(), Error> {
            self.0.record_event(event)
        }
        fn fail_with_event(&self, slug: &str, event: &InstanceEvent) -> Result<(), Error> {
            self.0.fail_with_event(slug, event)
        }
        fn delete_with_event(&self, slug: &str, event: &InstanceEvent) -> Result<(), Error> {
            self.0.delete_with_event(slug, event)
        }
        fn prune_events_before(&self, cutoff: chrono::DateTime<Utc>) -> Result<u32, Error> {
            self.0.prune_events_before(cutoff)
        }
        fn events(&self, slug: Option<&str>, limit: u32) -> Result<Vec<InstanceEvent>, Error> {
            self.0.events(slug, limit)
        }
    }

    /// A port that fails to return to the pool is the quietest loss in the
    /// codebase: the sweep counts the row as swept, correctly, and the range
    /// shrinks by one with the owning row already deleted. The event is the
    /// only thing left that names the slug (#117).
    #[test]
    fn a_leaked_port_is_recorded_against_the_slug_that_leaked_it() {
        let inner = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&inner, "leaky", 9000, Status::Done);

        let gm = manager_with(UnreleasablePortRegistry(inner), NoopProvisioner, 100, 100);
        gm.despawn_blocking("leaky")
            .expect("the row still left the registry, so the teardown succeeded");

        let events = gm.events(Some("leaky"), 10).unwrap();
        assert_eq!(events.len(), 2, "expected reap + leak, got {events:?}");

        let leak = events
            .iter()
            .find(|e| e.outcome == EventOutcome::Failed)
            .expect("the leaked port must be recorded");
        assert_eq!(leak.code, "registry");
        assert!(
            leak.detail
                .as_deref()
                .is_some_and(|d| d.contains("releasing port 9000")),
            "the detail must say what was being attempted: {leak:?}"
        );
    }

    /// The trap on the other side: a spawn that failed released its own port,
    /// so the sweep that later reaps the row finds nothing to release. That is
    /// the normal path, and recording it would put leaks that never happened
    /// into the one log that has to be trustworthy.
    #[test]
    fn a_port_already_released_by_a_failed_spawn_is_not_recorded_as_a_leak() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // Saved without `acquire_port`, exactly as a failed spawn leaves it.
        registry
            .save(&make_goopy("already-released", 0, 9000, Status::Failed))
            .unwrap();

        let gm = manager_with_provisioner(registry, NoopProvisioner);
        gm.despawn_blocking("already-released").unwrap();

        let events = gm.events(Some("already-released"), 10).unwrap();
        assert_eq!(events.len(), 1, "only the reap belongs here: {events:?}");
        assert_eq!(events[0].outcome, EventOutcome::Reaped);
    }

    /// June 2026, reconstructed: an instance fails, the sweep reaps it, and
    /// months later somebody asks why. Before #118 the answer was a deleted
    /// row and a rotated journal.
    #[test]
    fn the_reason_a_reaped_instance_failed_is_still_queryable() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        let gm = manager_with_provisioner(registry, UnprovisionableProvisioner);

        let (slug, _) = gm.spawn().unwrap();
        wait_for_spawn_to_settle(&gm, &slug);
        assert_eq!(gm.get(&slug).unwrap().unwrap().status, Status::Failed);

        // The sweep reaps `Failed` rows unconditionally, taking the only
        // evidence with it — that was the whole problem.
        let (swept, errors) = gm.sweep().unwrap();
        assert_eq!(swept, 1);
        assert!(errors.is_empty());
        assert!(
            gm.get(&slug).unwrap().is_none(),
            "the instance must really be gone"
        );

        let events = gm.events(Some(&slug), 10).unwrap();
        assert_eq!(events.len(), 2, "failure and reap, got {events:?}");

        let failure = events
            .iter()
            .find(|e| e.outcome == EventOutcome::Failed)
            .expect("the failure must have survived the reap");
        assert_eq!(failure.phase, EventPhase::Spawn);
        assert_eq!(failure.code, "subprocess");
        assert!(
            failure
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("EACCES")),
            "{failure:?}"
        );

        let reap = events
            .iter()
            .find(|e| e.outcome == EventOutcome::Reaped)
            .expect("the reap must close the story");
        assert_eq!(reap.phase, EventPhase::Sweep);
        assert!(
            reap.occurred_at >= failure.occurred_at,
            "failed at T, reaped at T+n: {events:?}"
        );
    }

    /// Append-only means unbounded unless something trims it, and the sweep is
    /// the only periodic task there is.
    #[test]
    fn sweep_drops_events_past_the_retention_window() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();

        let mut stale = InstanceEvent::reaped("long-gone", EventPhase::Sweep);
        stale.occurred_at = Utc::now() - Duration::days(31);
        registry.record_event(&stale).unwrap();

        let fresh = InstanceEvent::reaped("recent", EventPhase::Sweep);
        registry.record_event(&fresh).unwrap();

        // 30-day window, as `manager_with` configures.
        let gm = manager_with_provisioner(registry, NoopProvisioner);
        gm.sweep().unwrap();

        let left = gm.events(None, 10).unwrap();
        assert_eq!(left.len(), 1, "only the stale event should go: {left:?}");
        assert_eq!(left[0].slug, "recent");
    }

    /// Retention is housekeeping, not a reap: a prune must not show up in the
    /// numbers gl-serv reads to decide whether a sweep went well.
    #[test]
    fn pruning_events_does_not_count_as_sweeping_an_instance() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        let mut stale = InstanceEvent::reaped("long-gone", EventPhase::Sweep);
        stale.occurred_at = Utc::now() - Duration::days(400);
        registry.record_event(&stale).unwrap();

        let gm = manager_with_provisioner(registry, NoopProvisioner);
        let (swept, errors) = gm.sweep().unwrap();

        assert_eq!(swept, 0, "no instance was reclaimed");
        assert!(errors.is_empty(), "a prune is not a per-instance failure");
        assert!(gm.events(None, 10).unwrap().is_empty(), "but it did prune");
    }

    // ── capacity caps ─────────────────────────────────────────────────────

    /// Seed a row directly into the registry (bypassing spawn) so cap tests can
    /// set up a precise mix of statuses without racing the spawn thread.
    fn seed_row(registry: &SqliteRegistry, slug: &str, port: u32, status: Status) {
        let goopy = make_goopy(slug, 0, port, status);
        registry.save(&goopy).unwrap();
        registry.acquire_port(slug, port, port + 1).unwrap();
    }

    /// A provisioner that never objects; the caps are the variable.
    fn manager_with_caps(
        registry: SqliteRegistry,
        max_active: u32,
        max_provisioned: u32,
    ) -> GoopyManager<SqliteRegistry, NoopProvisioner> {
        manager_with(registry, NoopProvisioner, max_active, max_provisioned)
    }

    #[test]
    fn capacity_reports_zero_usage_against_configured_caps() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        let gm = manager_with_caps(registry, 10, 20);

        let cap = gm.capacity().unwrap();

        assert_eq!(
            cap,
            Capacity {
                active: 0,
                max_active: 10,
                provisioned: 0,
                max_provisioned: 20,
            }
        );
        assert!(!cap.is_full(), "an empty registry is not full");
    }

    #[test]
    fn capacity_counts_failed_as_provisioned_but_not_active() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "done-one", 9030, Status::Done);
        seed_row(&registry, "failed-one", 9031, Status::Failed);
        let gm = manager_with_caps(registry, 10, 20);

        let cap = gm.capacity().unwrap();

        // Both rows hold a registry slot, but the Failed one holds no RAM —
        // the same asymmetry the caps themselves enforce.
        assert_eq!(cap.active, 1, "only the Done row is resident");
        assert_eq!(cap.provisioned, 2, "both rows occupy a slot");
    }

    #[test]
    fn capacity_binding_pair_is_the_cap_with_less_headroom() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "done-one", 9050, Status::Done);
        seed_row(&registry, "failed-one", 9051, Status::Failed);
        // active = 1/10 (9 free), provisioned = 2/4 (2 free) → provisioned binds.
        let gm = manager_with_caps(registry, 10, 4);

        assert_eq!(gm.capacity().unwrap().binding(), (2, 4));
    }

    #[test]
    fn capacity_binding_pair_follows_the_active_cap_when_it_is_tighter() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "done-one", 9060, Status::Done);
        seed_row(&registry, "done-two", 9061, Status::Done);
        // active = 2/3 (1 free), provisioned = 2/10 (8 free) → active binds.
        let gm = manager_with_caps(registry, 3, 10);

        assert_eq!(gm.capacity().unwrap().binding(), (2, 3));
    }

    #[test]
    fn capacity_binding_pair_never_contradicts_is_full() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // Two Failed rows meet a provisioned cap of 2 while holding no RAM: the
        // active count is 0, so reporting it would read "0 / 10" on a server
        // that refuses every spawn.
        seed_row(&registry, "failed-one", 9070, Status::Failed);
        seed_row(&registry, "failed-two", 9071, Status::Failed);
        let gm = manager_with_caps(registry, 10, 2);

        let cap = gm.capacity().unwrap();
        let (used, total) = cap.binding();

        assert_eq!(cap.active, 0, "Failed rows hold no RAM");
        assert_eq!((used, total), (2, 2));
        assert!(cap.is_full());
        assert_eq!(
            used >= total,
            cap.is_full(),
            "the displayed pair must agree with is_full: {cap:?}"
        );
    }

    #[test]
    fn capacity_is_full_when_either_cap_is_met() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "failed-one", 9040, Status::Failed);
        // Provisioned cap of 1 is met by the Failed row; the active cap is not.
        let gm = manager_with_caps(registry, 10, 1);

        let cap = gm.capacity().unwrap();

        assert!(cap.active < cap.max_active, "active cap has headroom");
        assert!(
            cap.is_full(),
            "hitting only the provisioned cap must still read as full: {cap:?}"
        );
    }

    #[test]
    fn spawn_refused_when_provisioned_cap_hit() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // A single Failed row fills a provisioned=1 cap. Failed counts toward
        // max_provisioned — it still occupies a registry slot until the sweep
        // reaps it — but not toward max_active, since its process is gone.
        seed_row(&registry, "failed-one", 9010, Status::Failed);
        // Generous active cap so only the provisioned cap can trip.
        let gm = manager_with_caps(registry, 100, 1);

        let err = gm.spawn().unwrap_err();
        assert!(
            matches!(
                err,
                Error::CapacityFull {
                    kind: CapacityKind::Provisioned
                }
            ),
            "expected CapacityFull(max_provisioned), got {err:?}"
        );
    }

    #[test]
    fn spawn_refused_when_active_cap_hit() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // A single Done row fills an active=1 cap; provisioned cap is generous.
        seed_row(&registry, "done-one", 9020, Status::Done);
        let gm = manager_with_caps(registry, 1, 100);

        let err = gm.spawn().unwrap_err();
        assert!(
            matches!(
                err,
                Error::CapacityFull {
                    kind: CapacityKind::Active
                }
            ),
            "expected CapacityFull(max_active), got {err:?}"
        );
    }

    #[test]
    fn failed_counts_toward_provisioned_not_active() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_row(&registry, "failed-a", 9030, Status::Failed);
        seed_row(&registry, "failed-b", 9032, Status::Failed);
        assert_eq!(registry.count_provisioned().unwrap(), 2);
        assert_eq!(registry.count_active().unwrap(), 0);
    }

    #[test]
    fn despawn_frees_an_active_and_provisioned_slot() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // caps of 1/1: exactly one instance may exist and be resident.
        let gm = manager_with_caps(registry, 1, 1);

        // First spawn succeeds and (via NoopProvisioner) reaches Done.
        let (slug, _) = gm.spawn().expect("first spawn should succeed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let g = gm.get(&slug).unwrap().unwrap();
            if g.status == Status::Done {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "spawn timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Second spawn must be refused — both caps are full.
        let err = gm.spawn().unwrap_err();
        assert!(
            matches!(err, Error::CapacityFull { .. }),
            "expected CapacityFull, got {err:?}"
        );

        // Despawn the first instance to free the slot.
        gm.despawn(slug.clone()).expect("despawn should succeed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while gm.get(&slug).unwrap().is_some() {
            assert!(std::time::Instant::now() < deadline, "despawn timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Now spawning succeeds again.
        gm.spawn()
            .expect("spawn should succeed after freeing a slot");
    }

    #[test]
    fn concurrent_spawns_cannot_exceed_provisioned_cap() {
        const THREADS: usize = 8;
        // The window between a check and its insert is only microseconds wide,
        // so one contended round catches a check-then-act implementation just
        // over a tenth of the time. Repeating the whole scenario against a fresh
        // database each round turns that into a near-certainty, while a correct
        // implementation passes every round.
        const ROUNDS: usize = 40;

        for round in 0..ROUNDS {
            // A file-backed DB, not `:memory:`: the pool is capped at one
            // connection for in-memory databases, which would serialise the
            // threads at the pool and hide the race entirely.
            let dir = tempfile::tempdir().expect("tempdir");
            let registry = SqliteRegistry::new(&dir.path().join("caps.db")).unwrap();

            // One slot, contended by every thread at once.
            let gm = Arc::new(manager_with_caps(registry, 100, 1));
            let barrier = Arc::new(std::sync::Barrier::new(THREADS));

            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    let gm = Arc::clone(&gm);
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        gm.spawn()
                    })
                })
                .collect();

            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

            let winners = results.iter().filter(|r| r.is_ok()).count();
            assert_eq!(
                winners, 1,
                "round {round}: exactly one spawn may win the single slot, got {winners}"
            );
            for err in results.iter().filter_map(|r| r.as_ref().err()) {
                assert!(
                    matches!(
                        err,
                        Error::CapacityFull {
                            kind: CapacityKind::Provisioned
                        }
                    ),
                    "round {round}: losers must be refused on capacity, got {err:?}"
                );
            }

            // The cap is on rows, so the registry is the authority: a
            // check-then-insert leaves several rows here even when the return
            // values look right.
            assert_eq!(
                gm.registry.count_provisioned().unwrap(),
                1,
                "round {round}: the cap must hold at the row level"
            );
        }
    }
}
