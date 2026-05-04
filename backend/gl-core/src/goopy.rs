use crate::shared_types::*;

use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct Goopy {
    pub slug: String,
    pub life_in_days: i32,
    pub created_at: DateTime<Utc>,
    pub status: Status,
    pub working_dir: PathBuf,
    pub port: u32
}

impl Goopy {
    pub fn new(
        slug: String,
        life_in_days: i32,
        created_at: DateTime<Utc>,
        working_dir: &Path,
        port: u32,
        status: Status,
    ) -> Result<Self, Error> {
        if life_in_days <= 0 {
            return Err(Error::Invalid);
        }

        if slug.is_empty() {
            return Err(Error::Invalid);
        }

        Ok(Self {
            slug,
            life_in_days,
            created_at,
            working_dir: working_dir.into(),
            port,
            status,
        })
    }

    pub(crate) fn from_stored(slug: String, life_in_days: i32, created_at: DateTime<Utc>, working_dir: &Path, port: u32, status: Status) -> Self {
        Self {
            slug,
            life_in_days,
            created_at,
            working_dir: working_dir.into(),
            port,
            status,
        }
    }
}
