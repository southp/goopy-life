use crate::goopy::*;
use crate::goopy_provisioner::*;
use crate::goopy_registry::*;
use crate::shared_types::*;

use chrono::{self, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{JoinHandle, ThreadId};

pub struct GoopyManager<
    Registry: GoopyRegistry + Send + Sync + 'static,
    Provisioner: GoopyProvisioner + Send + Sync + 'static,
> {
    pub base_dir: PathBuf,
    pub domain: String,
    pub ssl_email: String,
    pub goopy_life_in_days: i32,
    pub port_range_start: u32,
    pub port_range_end: u32,

    registry: Arc<Registry>,
    provisioner: Arc<Provisioner>,

    // TODO: This will also need to be cleaned up regularly
    jobs: HashMap<ThreadId, JoinHandle<()>>,
}

impl<Registry, Provisioner> GoopyManager<Registry, Provisioner>
where
    Registry: GoopyRegistry + Send + Sync + 'static,
    Provisioner: GoopyProvisioner + Send + Sync + 'static,
{
    pub fn new(
        base_dir: PathBuf,
        domain: String,
        ssl_email: String,
        goopy_life_in_days: i32,
        port_range_start: u32,
        port_range_end: u32,
        registry: Registry,
        provisioner: Provisioner,
    ) -> Self {
        Self {
            base_dir,
            domain,
            ssl_email,
            goopy_life_in_days,
            port_range_start,
            port_range_end,
            registry: Arc::new(registry),
            provisioner: Arc::new(provisioner),
            jobs: HashMap::new(),
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn spawn(&mut self) -> Result<(String, u32, ThreadId), Error> {
        const MAX_RETRIES: usize = 10;

        let port = self.registry.acquire_port(self.port_range_start, self.port_range_end)?;

        let mut new_goopy = None;
        for _ in 0..MAX_RETRIES {
            let slug = crate::slug_generator::generate_slug();
            let candidate = Goopy::new(
                slug.clone(),
                self.goopy_life_in_days,
                Utc::now(),
                &self.base_dir.join(&slug),
                port,
                Status::Spawning,
                self.provisioner.kind(),
                env!("CARGO_PKG_VERSION").to_string(),
            )?;

            match self.registry.save(&candidate) {
                Ok(()) => {
                    new_goopy = Some(candidate);
                    break;
                }
                Err(Error::AlreadyExists) => {
                    tracing::warn!(slug = %slug, "slug collision, retrying");
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
            if let Err(e) = self.registry.release_port(port) {
                tracing::error!("spawn: release port {} error: {:?}", port, e);
            }
            return Err(Error::SlugExhausted);
        };

        let slug = new_goopy.slug.clone();

        // now, spawn the job
        let registry = Arc::clone(&self.registry);
        let goopy_clone = new_goopy.clone();
        let provisioner = Arc::clone(&self.provisioner);
        let span = tracing::Span::current();

        let handle = std::thread::spawn(move || {
            let _guard = span.enter();
            match provisioner.provision(&goopy_clone) {
                Ok(_) => {
                    if let Err(e) = registry.update_status(&goopy_clone.slug, Status::Done) {
                        tracing::error!("spawning: update {} error: {:?}", goopy_clone.slug, e);
                    }
                },
                Err(err) => {
                    tracing::error!("provisioning for goopy: {} failed: {:?}", goopy_clone.slug, err);

                    if let Err(e) = registry.release_port(port) {
                        tracing::error!("spawn: release port {} error: {:?}", port, e);
                    }
                    if let Err(e) = registry.update_status(&goopy_clone.slug, Status::Failed)
                    {
                        tracing::error!("spawning: update {} error: {:?}", goopy_clone.slug, e);
                    }
                }
            }
        });

        let id = handle.thread().id();
        self.jobs.insert(id, handle);

        Ok((slug, port, id))
    }

    #[tracing::instrument(skip(self))]
    pub fn despawn(&mut self, slug: String) -> Result<ThreadId, Error> {
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
        let handle = std::thread::spawn(move || {
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
                },
                Err(err) => {
                    tracing::error!("deprovisioning for goopy: {} failed: {:?}", goopy_clone.slug, err);

                    // Intentionally not calling release_port here: the port stays
                    // reserved so the stuck goopy remains visible for investigation.
                    // The operator can retry `despawn` once the underlying issue is
                    // resolved, which will release the port on success.
                    if let Err(e) = registry.update_status(&goopy_clone.slug, Status::Failed)
                    {
                        tracing::error!("despawning: update {} error: {:?}", goopy_clone.slug, e);
                    }
                }
            }
        });

        let id = handle.thread().id();
        self.jobs.insert(id, handle);

        Ok(id)
    }

    pub fn get(&self, slug: &str) -> Result<Option<Goopy>, Error> {
        self.registry.load(slug)
    }

    pub fn list(&self) -> Result<Vec<Goopy>, Error> {
        self.registry.list()
    }

    /// Despawn all expired goopy instances.
    ///
    /// An instance is considered expired when `now > created_at + life_in_days`.
    /// Only instances with status `Done` or `Failed` are swept; those still
    /// `Spawning` or `Despawning` are left alone to avoid interfering with
    /// in-progress operations.
    ///
    /// Meant to be called periodically (e.g. via `tokio::time::interval` in
    /// `gl-serv`).
    #[tracing::instrument(skip(self))]
    pub fn sweep(&mut self) -> Result<u32, Error> {
        let now = Utc::now();
        let goopies = self.list()?;
        let mut swept = 0u32;

        for gp in goopies {
            if gp.status == Status::Spawning || gp.status == Status::Despawning {
                continue;
            }

            let expires_at = gp.created_at + chrono::Duration::days(gp.life_in_days as i64);
            if now > expires_at {
                tracing::info!(
                    slug = %gp.slug,
                    status = %gp.status,
                    expired_at = %expires_at,
                    "sweeping expired instance"
                );
                match self.despawn(gp.slug) {
                    Ok(_) => swept += 1,
                    Err(e) => {
                        tracing::error!(error = %e, "sweep: despawn failed, skipping");
                    }
                }
            }
        }

        tracing::info!(swept, "sweep complete");
        Ok(swept)
    }

    pub fn is_job_finished(&self, job_id: &ThreadId) -> bool {
        self.jobs
            .get(job_id)
            .expect("job_id not tracked; callers must only pass IDs returned by spawn()")
            .is_finished()
    }
}

impl<Registry, Provisioner> Drop for GoopyManager<Registry, Provisioner>
where
    Registry: GoopyRegistry + Send + Sync + 'static,
    Provisioner: GoopyProvisioner + Send + Sync + 'static,
{
    fn drop(&mut self) {
        for (_, handle) in self.jobs.drain() {
            if let Err(e) = handle.join() {
                tracing::error!("worker thread panicked: {:?}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goopy::Goopy;
    use crate::goopy_provisioner::GoopyProvisioner;
    use crate::goopy_registry::GoopyRegistry;
    use std::sync::Mutex;

    struct CollideOnceRegistry {
        save_calls: Mutex<u32>,
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
        fn load(&self, _slug: &str) -> Result<Option<Goopy>, Error> { Ok(None) }
        fn delete(&self, _slug: &str) -> Result<(), Error> { Ok(()) }
        fn list(&self) -> Result<Vec<Goopy>, Error> { Ok(vec![]) }
        fn update_status(&self, _slug: &str, _new_status: Status) -> Result<(), Error> { Ok(()) }
        fn acquire_port(&self, range_start: u32, _range_end: u32) -> Result<u32, Error> { Ok(range_start) }
        fn release_port(&self, _port: u32) -> Result<(), Error> { Ok(()) }
    }

    struct NoopProvisioner;

    impl GoopyProvisioner for NoopProvisioner {
        fn provision(&self, _goopy: &Goopy) -> Result<(), Error> { Ok(()) }
        fn deprovision(&self, _goopy: &Goopy) -> Result<(), Error> { Ok(()) }
        fn kind(&self) -> ProvisionerKind { ProvisionerKind::Hello }
    }

    #[test]
    fn spawn_retries_on_collision() {
        let mut gm = GoopyManager::new(
            std::path::PathBuf::from("/tmp/test-goopy"),
            "test.example".into(),
            "test@example.com".into(),
            7,
            8080,
            9080,
            CollideOnceRegistry { save_calls: Mutex::new(0) },
            NoopProvisioner,
        );

        // First save returns AlreadyExists; spawn must retry and succeed on the second attempt.
        let result = gm.spawn();
        assert!(result.is_ok(), "spawn should succeed after retrying a slug collision");
    }

    #[test]
    fn sweep_removes_expired_instances() {
        use crate::goopy_registry::sqlite_registry::SqliteRegistry;
        use std::path::Path;

        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();

        // Insert an expired goopy: created 10 days ago, lives 7 days
        let expired = Goopy::new(
            "expired-slug".to_string(),
            7,
            Utc::now() - chrono::Duration::days(10),
            &PathBuf::from("/tmp/expired-slug"),
            9000,
            Status::Done,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap();
        registry.save(&expired).unwrap();
        registry.acquire_port(9000, 9001).unwrap();

        // Insert a non-expired goopy: created now, lives 7 days
        let alive = Goopy::new(
            "alive-slug".to_string(),
            7,
            Utc::now(),
            &PathBuf::from("/tmp/alive-slug"),
            9001,
            Status::Done,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap();
        registry.save(&alive).unwrap();
        registry.acquire_port(9001, 9002).unwrap();

        let mut gm = GoopyManager::new(
            PathBuf::from("/tmp"),
            "test.example".into(),
            "test@example.com".into(),
            7,
            9000,
            9100,
            registry,
            NoopProvisioner,
        );

        let swept = gm.sweep().unwrap();
        assert_eq!(swept, 1);

        // Wait for the despawn background thread to finish
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Expired should be gone
        assert!(gm.get("expired-slug").unwrap().is_none());
        // Alive should remain
        assert!(gm.get("alive-slug").unwrap().is_some());
    }

    #[test]
    fn sweep_skips_in_progress_instances() {
        use crate::goopy_registry::sqlite_registry::SqliteRegistry;
        use std::path::Path;

        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();

        // Insert an expired goopy with Spawning status — should be skipped
        let spawning = Goopy::new(
            "spawning-slug".to_string(),
            7,
            Utc::now() - chrono::Duration::days(10),
            &PathBuf::from("/tmp/spawning-slug"),
            9000,
            Status::Spawning,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap();
        registry.save(&spawning).unwrap();
        registry.acquire_port(9000, 9001).unwrap();

        // Insert an expired goopy with Despawning status — should be skipped
        let despawning = Goopy::new(
            "despawning-slug".to_string(),
            7,
            Utc::now() - chrono::Duration::days(10),
            &PathBuf::from("/tmp/despawning-slug"),
            9001,
            Status::Despawning,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap();
        registry.save(&despawning).unwrap();
        registry.acquire_port(9001, 9002).unwrap();

        let mut gm = GoopyManager::new(
            PathBuf::from("/tmp"),
            "test.example".into(),
            "test@example.com".into(),
            7,
            9000,
            9100,
            registry,
            NoopProvisioner,
        );

        let swept = gm.sweep().unwrap();
        assert_eq!(swept, 0);

        // Both should still exist
        assert!(gm.get("spawning-slug").unwrap().is_some());
        assert!(gm.get("despawning-slug").unwrap().is_some());
    }

    #[test]
    fn sweep_no_expired_instances() {
        use crate::goopy_registry::sqlite_registry::SqliteRegistry;
        use std::path::Path;

        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();

        // Insert a non-expired goopy
        let alive = Goopy::new(
            "fresh-slug".to_string(),
            7,
            Utc::now(),
            &PathBuf::from("/tmp/fresh-slug"),
            9000,
            Status::Done,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap();
        registry.save(&alive).unwrap();
        registry.acquire_port(9000, 9001).unwrap();

        let mut gm = GoopyManager::new(
            PathBuf::from("/tmp"),
            "test.example".into(),
            "test@example.com".into(),
            7,
            9000,
            9100,
            registry,
            NoopProvisioner,
        );

        let swept = gm.sweep().unwrap();
        assert_eq!(swept, 0);
        assert!(gm.get("fresh-slug").unwrap().is_some());
    }
}
