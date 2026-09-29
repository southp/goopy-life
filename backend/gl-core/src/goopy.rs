use crate::shared_types::*;

use chrono::{DateTime, Duration, Utc};
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Goopy {
    pub slug: String,
    /// How long this instance lives, fixed when it is spawned: `expires_at` is
    /// `created_at` plus this many hours.
    pub life_in_hours: i32,
    pub created_at: DateTime<Utc>,
    pub status: Status,
    pub working_dir: PathBuf,
    pub port: u32,
    pub provisioner_kind: ProvisionerKind,
    pub service_version: String,
    /// The gl commit (`build_info::GIT_SHA`) of the binary that provisioned
    /// this instance — so a row names the code that made it, not only the
    /// Ghost version it runs.
    ///
    /// `None` only for rows created before the column existed: "not recorded"
    /// is kept apart from `Some("unknown")`, which is a recorded fact (an
    /// unstamped local build did the provisioning).
    pub build_sha: Option<String>,
}

impl Goopy {
    /// When this instance expires: `created_at` plus `life_in_hours`.
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.created_at + Duration::hours(self.life_in_hours as i64)
    }
}
