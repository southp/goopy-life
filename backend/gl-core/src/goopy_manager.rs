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

    pub fn is_job_finished(&self, job_id: &ThreadId) -> Option<bool> {
        if let Some(handle) = self.jobs.get(job_id) {
            return Some(handle.is_finished());
        }

        None
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
