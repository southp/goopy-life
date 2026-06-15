use crate::shared_types::*;

use std::path::PathBuf;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Goopy {
    pub slug: String,
    pub life_in_days: i32,
    pub created_at: DateTime<Utc>,
    pub status: Status,
    pub working_dir: PathBuf,
    pub port: u32,
    pub provisioner_kind: ProvisionerKind,
    pub service_version: String,
}

