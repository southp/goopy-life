use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::process::{Command};
use std::collections::hash_map::{HashMap, Entry};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::fs;

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
#[derive(Debug, Clone)]
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
    // There should also be no goopy_map map
    status_map: Arc<Mutex<HashMap<String, GlStatus>>>,
    goopy_map: Arc<Mutex<HashMap<String, Goopy>>>,

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
            status_map: Arc::new(Mutex::new(HashMap::new())),
            goopy_map: Arc::new(Mutex::new(HashMap::new())),
            jobs: vec![],
        }
    }

    // TODO:
    // Use `get` for existence test
    pub fn spawn(&mut self, slug: String, port: u32) -> bool {
        // create a goopy instance
        {
            let mut gm = self.goopy_map.lock().unwrap();
            match gm.entry(slug.clone()) {
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
        }

        // create a status entry
        {
            let mut m = self.status_map.lock().unwrap();
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
        // let db_path = format!("content/data/{}_prod.db", goopy.slug);
        // let site_url = Url::parse(&format!("https://{}.{}", goopy.slug, self.domain)).expect("Url parse error!");

        // now, spawn the job
        let status_clone = Arc::clone(&self.status_map);
        let slug_clone = slug.clone();
        let ghost_dir = self.base_dir.join(&slug);

        let worker_handle = std::thread::spawn(move || {
            let result = fs::create_dir_all(&ghost_dir)
                .and_then(|_| Command::new("ghost")
                    .args([
                        "install",
                        "local",
                        "--pname", &slug_clone,
                        "--port", &port.to_string(),
                        "--version", "6.28.0",
                    ])
                    .current_dir(&ghost_dir)
                    .output()
                );

            let mut m = status_clone.lock().unwrap();

            match result {
                Ok(cmd) => {
                    m.entry(slug_clone.clone()).and_modify(|e| {
                        *e = if cmd.status.success() { GlStatus::Done } else { GlStatus::Failed };
                    });
                    println!("job for goopy: {} exits with status: {}", slug_clone, cmd.status);

                    if ! cmd.status.success() {
                        println!("stdout: {}", String::from_utf8_lossy(&cmd.stdout));
                        println!("stderr: {}", String::from_utf8_lossy(&cmd.stderr));
                    }
                }
                Err(err) => {
                    m.entry(slug_clone.clone()).and_modify(|e| {
                        *e = GlStatus::Failed;
                    });
                    println!("job for goopy: {} failed: {}", slug_clone, err);
                }
            }

        });

        self.jobs.push(worker_handle);

        true
    }

    pub fn despawn(&mut self, slug: String) -> Result<(), GlError> {
        if let Some((status, goopy)) = self.get(&slug) {
            if status == GlStatus::InProgress {
                return Err(GlError::Failed("Can't despawn a spawning instance.".into()));
            }

            let instance_dir = self.base_dir.join(&goopy.slug);
            let status_map_clone = self.status_map.clone();
            let goopy_map_clone = self.goopy_map.clone();
            let slug_clone = goopy.slug.clone();

            let despawn_handle = std::thread::spawn(move || {
                // remove the instance through the "provisioner"
                let result = Command::new("ghost")
                    .args([
                        "uninstall",
                        "-f"
                    ])
                    .current_dir(instance_dir)
                    .output();

                // clean up the status map and the instance map if successful
                let mut sm = status_map_clone.lock().unwrap();
                let mut gm = goopy_map_clone.lock().unwrap();

                match result {
                    Ok(cmd) => {
                        println!("despawning job for goopy: {} exits with status: {}", slug_clone, cmd.status);

                        if ! cmd.status.success() {
                            println!("stderr: {}", String::from_utf8_lossy(&cmd.stderr));
                            return;
                        }

                        let rem_st = sm.remove(&slug_clone);
                        let rem_ins = gm.remove(&slug_clone);

                        if rem_st.is_none() || rem_ins.is_none() {
                            println!("entry is gone while despawning {}. Status: {:?}. Instance: {:?}", slug_clone, rem_st, rem_ins);
                        }

                    }

                    Err(err) => {
                        println!("despawning for: {} failed because: {}", slug, err);
                    }
                }
            });

            self.jobs.push(despawn_handle);

            Ok(())
        } else {
           Err(GlError::NotFound)
        }
    }

    // TODO
    // Of course, this won't work like this once the persistence layer is in
    pub fn get(&self, slug: &String) -> Option<(GlStatus, Goopy)> {
        let gm = self.goopy_map.lock().unwrap();
        if let Some(goopy) = gm.get(slug) {
            let sm = self.status_map.lock().unwrap();

            if let Some(status) = sm.get(slug) {
                return Some((status.clone(), goopy.clone()));
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
