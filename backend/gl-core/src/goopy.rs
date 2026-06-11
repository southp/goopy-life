use crate::shared_types::*;

use std::path::{Path, PathBuf};
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

impl Goopy {
    pub fn new(
        slug: String,
        life_in_days: i32,
        created_at: DateTime<Utc>,
        working_dir: &Path,
        port: u32,
        status: Status,
        provisioner_kind: ProvisionerKind,
        service_version: String,
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
            provisioner_kind,
            service_version,
        })
    }

    pub(crate) fn from_stored(
        slug: String,
        life_in_days: i32,
        created_at: DateTime<Utc>,
        working_dir: &Path,
        port: u32,
        status: Status,
        provisioner_kind: ProvisionerKind,
        service_version: String,
    ) -> Self {
        Self {
            slug,
            life_in_days,
            created_at,
            working_dir: working_dir.into(),
            port,
            status,
            provisioner_kind,
            service_version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::Path;

    #[test]
    fn valid_construction_succeeds() {
        let g = Goopy::new(
            "bold-swift-oak".to_string(),
            7,
            Utc::now(),
            Path::new("/tmp/bold-swift-oak"),
            8080,
            Status::Spawning,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap();
        assert_eq!(g.slug, "bold-swift-oak");
        assert_eq!(g.life_in_days, 7);
        assert_eq!(g.status, Status::Spawning);
    }

    #[test]
    fn empty_slug_returns_invalid() {
        let err = Goopy::new(
            "".to_string(),
            7,
            Utc::now(),
            Path::new("/tmp/x"),
            8080,
            Status::Spawning,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Invalid));
    }

    #[test]
    fn zero_life_in_days_returns_invalid() {
        let err = Goopy::new(
            "some-slug".to_string(),
            0,
            Utc::now(),
            Path::new("/tmp/x"),
            8080,
            Status::Spawning,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Invalid));
    }

    #[test]
    fn negative_life_in_days_returns_invalid() {
        let err = Goopy::new(
            "some-slug".to_string(),
            -1,
            Utc::now(),
            Path::new("/tmp/x"),
            8080,
            Status::Spawning,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Invalid));
    }
}
