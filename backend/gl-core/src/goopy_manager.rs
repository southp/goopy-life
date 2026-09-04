use crate::goopy::*;
use crate::goopy_provisioner::*;
use crate::goopy_registry::*;
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
    /// Whether either cap is met, i.e. a spawn attempted right now would be
    /// refused with [`Error::CapacityFull`].
    pub fn is_full(&self) -> bool {
        self.active >= self.max_active || self.provisioned >= self.max_provisioned
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
                service_version: env!("CARGO_PKG_VERSION").to_string(),
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
                    if let Err(e) = registry.update_status(&goopy_clone.slug, Status::Failed) {
                        tracing::error!("spawning: update {} error: {:?}", goopy_clone.slug, e);
                    }
                }
            }
        });

        Ok((slug, port))
    }

    #[tracing::instrument(skip(self))]
    pub fn despawn(&self, slug: String) -> Result<String, Error> {
        let Some(goopy) = self.get(&slug)? else {
            return Err(Error::NotFound);
        };

        if goopy.status == Status::Spawning || goopy.status == Status::Despawning {
            return Err(Error::Invalid);
        }

        // annotate the status
        self.registry.update_status(&slug, Status::Despawning)?;

        let goopy_clone = goopy.clone();
        let registry = Arc::clone(&self.registry);
        let provisioner = Arc::clone(&self.provisioner);
        let span = tracing::Span::current();

        let port = goopy.port;
        let slug_for_return = slug.clone();
        std::thread::spawn(move || {
            let _guard = span.enter();
            let result = provisioner.deprovision(&goopy_clone);
            match result {
                Ok(_) => {
                    if let Err(e) = registry.delete(&goopy_clone.slug) {
                        tracing::error!("despawning: delete {} error: {:?}", goopy_clone.slug, e);
                    }
                    if let Err(e) = registry.release_port(port) {
                        tracing::error!("despawning: release port {} error: {:?}", port, e);
                    }
                }
                Err(err) => {
                    tracing::error!(
                        "deprovisioning for goopy: {} failed: {:?}",
                        goopy_clone.slug,
                        err
                    );

                    // Intentionally not calling release_port here: the port stays
                    // reserved so the stuck goopy remains visible for investigation.
                    // The operator can retry `despawn` once the underlying issue is
                    // resolved, which will release the port on success.
                    if let Err(e) = registry.update_status(&goopy_clone.slug, Status::Failed) {
                        tracing::error!("despawning: update {} error: {:?}", goopy_clone.slug, e);
                    }
                }
            }
        });

        Ok(slug_for_return)
    }

    pub fn get(&self, slug: &str) -> Result<Option<Goopy>, Error> {
        self.registry.load(slug)
    }

    pub fn list(&self) -> Result<Vec<Goopy>, Error> {
        self.registry.list()
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
    /// Returns `(swept_count, per_despawn_errors)`. Errors from individual
    /// `despawn` calls are collected rather than aborting the sweep early.
    ///
    /// Meant to be called periodically (e.g. via `tokio::time::interval` in
    /// `gl-serv`).
    #[tracing::instrument(skip(self))]
    pub fn sweep(&self) -> Result<(u32, Vec<Error>), Error> {
        let now = Utc::now();
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
                match self.despawn(gp.slug) {
                    Ok(_) => swept += 1,
                    Err(e) => {
                        tracing::error!(error = %e, "sweep: despawn failed, skipping");
                        errors.push(e);
                    }
                }
            }
        }

        tracing::info!(swept, "sweep complete");
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

    // ── capacity caps ─────────────────────────────────────────────────────

    /// Seed a row directly into the registry (bypassing spawn) so cap tests can
    /// set up a precise mix of statuses without racing the spawn thread.
    fn seed_row(registry: &SqliteRegistry, slug: &str, port: u32, status: Status) {
        let goopy = make_goopy(slug, 0, port, status);
        registry.save(&goopy).unwrap();
        registry.acquire_port(slug, port, port + 1).unwrap();
    }

    fn manager_with_caps(
        registry: SqliteRegistry,
        max_active: u32,
        max_provisioned: u32,
    ) -> GoopyManager<SqliteRegistry, NoopProvisioner> {
        GoopyManager::new(
            GoopyManagerConfig {
                base_dir: PathBuf::from("/tmp"),
                domain: "test.example".into(),
                life_in_days: 7,
                port_range_start: 9000,
                port_range_end: 9100,
                max_active,
                max_provisioned,
            },
            registry,
            NoopProvisioner,
        )
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
