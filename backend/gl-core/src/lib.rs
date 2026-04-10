use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::process::{Command};
use std::collections::hash_map::{HashMap, Entry};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Debug)]
pub enum GlError {
    Failed(String),
    Invalid,
    NotFound,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GlStatus {
    Failed,
    InProgress,
    Done,
}

pub struct Goopy {
    pub slug: String,
    pub life_in_days: i32,
    pub created_at: DateTime<Utc>,
}

impl Goopy {
    pub fn new(
        slug: String,
        life_in_days: i32,
        created_at: DateTime<Utc>,
    ) -> Result<Self, GlError> {
        if life_in_days <= 0 {
            return Err(GlError::Invalid);
        }

        if slug.is_empty() {
            return Err(GlError::Invalid);
        }

        Ok(Self {
            slug,
            life_in_days,
            created_at,
        })
    }
}

pub struct GoopyManager {
    pub base_dir: PathBuf,
    pub domain: String,
    pub ssl_email: String,
    pub goopy_life_in_days: i32,

    // TODO:
    // Once the persistence store is in-place, clean this map up in a deterministic way.
    // There should also be no slug_goopies map
    slug_status: Arc<Mutex<HashMap<String, GlStatus>>>,
    slug_goopies: HashMap<String, Goopy>,

    // TODO: This will also need to be cleaned up regularly
    jobs: Vec<JoinHandle<()>>,
}

impl GoopyManager {
    pub fn new(base_dir: PathBuf, domain: String, ssl_email:String, goopy_life_in_days: i32) -> Self {
        Self {
            base_dir,
            domain,
            ssl_email,
            goopy_life_in_days,
            slug_status: Arc::new(Mutex::new(HashMap::new())),
            slug_goopies: HashMap::new(),
            jobs: vec![],
        }
    }

    pub fn spawn(&mut self, slug: String) -> bool {
        // create a goopy instance
        match self.slug_goopies.entry(slug.clone()) {
            Entry::Vacant(e) => {
                e.insert(Goopy {
                    slug: slug.clone(),
                    life_in_days: self.goopy_life_in_days,
                    created_at: Utc::now(),
                });
            }
            Entry::Occupied(_) => {
                return false;
            }
        }

        // create a job entry
        {
            let mut m = self.slug_status.lock().unwrap();
            match m.entry(slug.clone()) {
                Entry::Vacant(e) => {
                    e.insert(GlStatus::InProgress);
                },
                Entry::Occupied(_) => {
                    return false;
                }
            }
        }

        // ghost install --no-prompt --dir ${ghost_dir} --db sqlite3 --dbpath content/data/${name}_prod.db --url https://${name}.southp.dev --process systemd --sslemail mail@southp.me
        // let ghost_dir = self.base_dir.join(&goopy.slug);
        // let db_path = format!("content/data/{}_prod.db", goopy.slug);
        // let site_url = Url::parse(&format!("https://{}.{}", goopy.slug, self.domain)).expect("Url parse error!");

        // now, spawn the job
        let status_clone = Arc::clone(&self.slug_status);
        let slug_clone = slug.clone();

        let worker_handle = std::thread::spawn(move || {
            let cmd = Command::new("sleep")
            .args([
                "3s",
            ])
            .output()
            .expect("Failed to run the installation command");

            let mut m = status_clone.lock().unwrap();
            m.entry(slug_clone.clone()).and_modify(|e| {
                *e = if cmd.status.success() { GlStatus::Done } else { GlStatus::Failed };
            });

            println!("job for goopy: {} exits with status: {}", slug_clone, cmd.status);
        });

        self.jobs.push(worker_handle);

        true
    }

    pub fn despawn(&self, _goopy: &Goopy) -> Result<(), std::io::Error> {
        Ok(())
    }

    // TODO
    // Of course, this won't work like this once the persistence layer is in
    pub fn get(&self, slug: String) -> Option<(GlStatus, &Goopy)> {
        if let Some(goopy) = self.slug_goopies.get(&slug) {
            let m = self.slug_status.lock().unwrap();

            if let Some(status) = m.get(&slug) {
                return Some((status.clone(), goopy));
            }
        }

        None
    }
}

impl Drop for GoopyManager {
    fn drop(&mut self) {
        for handle in self.jobs.drain(..) {
            handle.join().unwrap();
        }
    }
}
