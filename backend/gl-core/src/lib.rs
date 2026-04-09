use chrono::{DateTime, Utc};
use url::Url;
use std::path::PathBuf;
use std::process::{Command, Child};
use std::collections::hash_map::{HashMap, Entry};

#[derive(Debug)]
pub enum GlError {
    Failed(String),
    Invalid,
    NotFound,
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

    slug_job_map: HashMap<String, Child>,
}

impl GoopyManager {
    pub fn new(base_dir: PathBuf, domain: String, ssl_email:String) -> Self {
        Self {
            base_dir,
            domain,
            ssl_email,
            slug_job_map: HashMap::new(),
        }
    }

    pub fn spawn(&mut self, goopy: &Goopy) -> bool {
        // ghost install --no-prompt --dir ${ghost_dir} --db sqlite3 --dbpath content/data/${name}_prod.db --url https://${name}.southp.dev --process systemd --sslemail mail@southp.me
        let ghost_dir = self.base_dir.join(&goopy.slug);
        let db_path = format!("content/data/{}_prod.db", goopy.slug);
        let site_url = Url::parse(&format!("https://{}.{}", goopy.slug, self.domain)).expect("Url parse error!");

        let mut cmd_install = Command::new("sleep")
        .args([
            "3s",
            // "--no-prompt",
            // "--dir", ghost_dir.to_str().unwrap(),
            // "--db", "sqlite3",
            // "--dbpath", &db_path,
            // "--url", site_url.as_str(),
            // "--process", "systemd",
            // "--sslemail", &self.ssl_email
        ])
        .spawn()
        .expect("Failed to run the installation command");

        println!("Spawend pid: {}", cmd_install.id());

        match self.slug_job_map.entry(goopy.slug.clone()) {
            Entry::Vacant(e) => {
                e.insert(cmd_install);
                return true;
            }
            Entry::Occupied(_) => {
                return false;
            }
        }
    }

    pub fn despawn(&self, _goopy: &Goopy) -> Result<(), std::io::Error> {
        Ok(())
    }

    pub fn get(&self, _slug: String) -> Result<Goopy, std::io::Error> {
        Ok(Goopy{
            slug: "".to_string(),
            life_in_days: 0,
            created_at: Utc::now(),
        })
    }
}
