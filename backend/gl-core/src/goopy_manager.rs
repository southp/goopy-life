use crate::goopy::*;
use crate::goopy_store::*;
use crate::shared_types::*;

use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command};
use std::sync::Arc;
use std::thread::{ThreadId, JoinHandle};
use std::fs;

pub struct GoopyManager<T: GoopyStore + Send + Sync + 'static> {
    pub base_dir: PathBuf,
    pub domain: String,
    pub ssl_email: String,
    pub goopy_life_in_days: i32,
    pub store: Arc<T>,

    // TODO: This will also need to be cleaned up regularly
    jobs: HashMap<ThreadId, JoinHandle<()>>,
}

impl<T> GoopyManager<T> where T: GoopyStore + Send + Sync + 'static {
    pub fn new(base_dir: PathBuf, domain: String, ssl_email:String, goopy_life_in_days: i32, store: T) -> Self {
        Self {
            base_dir,
            domain,
            ssl_email,
            goopy_life_in_days,
            store: Arc::new(store),
            jobs: HashMap::new(),
        }
    }

    pub fn spawn(&mut self, slug: String, port: u32) -> Result<ThreadId, Error> {
        if let Some(_) = self.get(&slug)? {
            return Err(Error::AlreadyExists);
        }

        let new_goopy = Goopy::from_stored(
            slug.clone(),
            self.goopy_life_in_days,
            Utc::now(),
            Status::Spawning
        );

        self.store.save(&new_goopy)?;

        // now, spawn the job
        let store_clone = Arc::clone(&self.store);
        let slug_clone = slug.clone();
        let goopy_dir = self.base_dir.join(&slug);
        let goopy_clone = new_goopy.clone();

        let handle = std::thread::spawn(move || {
            let result = fs::create_dir_all(&goopy_dir)
                .and_then(|_| Command::new("ghost")
                    .args([
                        "install",
                        "6.28.0",
                        "--pname", &slug_clone,
                        "--port", &port.to_string(),
                        "--local",
                    ])
                    .current_dir(&goopy_dir)
                    .output()
                );

            match result {
                Ok(cmd) => {
                    println!("job for goopy: {} exits with status: {}", slug_clone, cmd.status);

                    if cmd.status.success() {
                        if let Err(e) = store_clone.update_status(&goopy_clone.slug, Status::Done) {
                            eprintln!("update {} error: {:?}", goopy_clone.slug, e);
                        }
                    } else {
                        eprintln!("stderr: {}", String::from_utf8_lossy(&cmd.stderr));

                        if let Err(e) = store_clone.update_status(&goopy_clone.slug, Status::Failed) {
                            eprintln!("update {} error: {:?}", goopy_clone.slug, e);
                        }
                    }
                },
                Err(err) => {
                    eprintln!("job for goopy: {} failed: {}", slug_clone, err);

                    if let Err(e) = store_clone.update_status(&goopy_clone.slug, Status::Failed) {
                        eprintln!("update {} error: {:?}", goopy_clone.slug, e);
                    }
                }
            }
        });

        let id = handle.thread().id();
        self.jobs.insert(id, handle);

        Ok(id)
    }

    pub fn despawn(&mut self, slug: String) -> Result<ThreadId, Error> {
        let Some(goopy) = self.get(&slug)? else {
            return Err(Error::NotFound);
        };

        if goopy.status == Status::Spawning {
            return Err(Error::Invalid);
        }

        // annotate the status
        self.store.update_status(&slug, Status::Despawning)?;

        let instance_dir = self.base_dir.join(&goopy.slug);
        let goopy_clone = goopy.clone();
        let store_clone = Arc::clone(&self.store);

        let handle = std::thread::spawn(move || {
            // remove the instance through the "provisioner"
            let result = Command::new("ghost")
                .args([
                    "uninstall",
                    "-f"
                ])
                .current_dir(instance_dir)
                .output();

            match result {
                Ok(cmd) => {
                    println!("despawning job for goopy: {} exits with status: {}", goopy_clone.slug, cmd.status);

                    if cmd.status.success() {
                        return;
                    } else {
                        eprintln!("stderr: {}", String::from_utf8_lossy(&cmd.stderr));

                        if let Err(e) = store_clone.update_status(&goopy_clone.slug, Status::Failed) {
                            eprintln!("update {} error: {:?}", goopy_clone.slug, e);
                        }
                        return;
                    }
                }

                Err(err) => {
                    eprintln!("despawning for: {} failed because: {}", slug, err);

                    if let Err(e) = store_clone.update_status(&goopy_clone.slug, Status::Failed) {
                        eprintln!("update {} error: {:?}", goopy_clone.slug, e);
                    }
                    return;
                }
            }
        });

        let id = handle.thread().id();
        self.jobs.insert(id, handle);

        Ok(id)
    }

    pub fn get(&self, slug: &String) -> Result<Option<Goopy>, Error> {
        self.store.load(slug)
    }

    pub fn is_job_finished(&self, job_id: &ThreadId) -> Option<bool> {
        if let Some(handle) = self.jobs.get(job_id) {
            return Some(handle.is_finished());
        }

        None
    }
}

impl<T> Drop for GoopyManager<T> where T: GoopyStore + Send + Sync + 'static {
    fn drop(&mut self) {
        for (_, handle) in self.jobs.drain() {
            if let Err(e) = handle.join() {
                eprintln!("worker thread panicked: {:?}", e);
            }
        }
    }
}
