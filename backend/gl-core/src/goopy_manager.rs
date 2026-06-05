use crate::goopy::*;
use crate::goopy_provisioner::*;
use crate::goopy_registry::*;
use crate::shared_types::*;

use chrono::Utc;
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
            return Err(Error::Other("slug generation failed: too many collisions".into()));
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

    pub fn is_job_finished(&self, job_id: &ThreadId) -> bool {
        self.jobs.get(job_id).is_some_and(|h| h.is_finished())
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
}
