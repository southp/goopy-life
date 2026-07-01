use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::goopy_manager::GoopyManagerConfig;
use crate::goopy_provisioner::hello_provisioner::HelloProvisioner;
use crate::shared_types::{AllocatorKind, Error, ProvisionerKind};
use crate::storage_allocator::{PlainDirAllocator, StorageAllocator, ZfsAllocator};
use crate::sys_utils::SysRunner;

// Design note: a fully abstract design would store these as `dyn RegistryConfig` /
// `dyn AllocatorConfig` traits. We use concrete structs instead — the number of
// registry and allocator implementations is small and well-bounded for the
// foreseeable future, so the extra indirection isn't worth it.
#[derive(Debug, serde::Deserialize)]
pub struct RegistryConfig {
    pub path: PathBuf,
}

#[derive(Debug, serde::Deserialize)]
pub struct AllocatorConfig {
    pub kind: AllocatorKind,
    /// ZFS pool name. Required when `kind = "Zfs"`; ignored otherwise.
    #[serde(default)]
    pub pool: String,
    /// Per-instance disk quota in MB. Required when `kind = "Zfs"`; ignored otherwise.
    #[serde(default)]
    pub quota_mb: u64,
}

impl AllocatorConfig {
    pub fn build(&self) -> Arc<dyn StorageAllocator> {
        match self.kind {
            AllocatorKind::PlainDir => Arc::new(PlainDirAllocator),
            AllocatorKind::Zfs => Arc::new(ZfsAllocator::new(self.pool.clone(), self.quota_mb)),
        }
    }
}

/// Configuration for the provisioner subsection (`[provisioner]` in TOML).
///
/// Mirrors [`AllocatorConfig`]: a flat struct with a `kind` discriminant plus
/// kind-specific fields that are silently ignored when the kind does not apply.
/// Ghost-specific fields (e.g. `ghost_source_dir`) will be added here in #18
/// without breaking the `Hello` case.
#[derive(Debug, serde::Deserialize)]
pub struct ProvisionerConfig {
    pub kind: ProvisionerKind,
}

/// Rate limiting configuration (`[ratelimit]` section in TOML).
///
/// Two independent GCRA (Generic Cell Rate Algorithm) buckets are configured:
///
/// * **Provision limit** — applied to `POST /goopies` only.  Defaults to a
///   burst of 2 requests with one token replenished every 60 seconds, so a
///   single IP can spawn at most 2 instances back-to-back and then must wait
///   1 minute per additional spawn.  This matches the expected interaction
///   pattern (one deliberate click) while blocking trivial abuse on the
///   expensive provisioning path.
///
/// * **Read limit** — applied to all other endpoints (`GET /goopies/:slug`,
///   `GET /goopies/:slug/alive`, `GET /config`).  Defaults to a burst of 30
///   requests with one token replenished every 2 seconds, comfortable for a
///   frontend that polls `alive` every few seconds but still rejects floods.
///
/// Both limits are per **real client IP**, resolved from the `X-Real-IP`
/// header that nginx sets (falling back to `X-Forwarded-For` and then the
/// TCP peer address).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RateLimitConfig {
    /// Burst size for `POST /goopies`.
    #[serde(default = "default_provision_burst")]
    pub provision_burst: u32,
    /// Token replenishment period (seconds) for `POST /goopies`.
    #[serde(default = "default_provision_period_secs")]
    pub provision_period_secs: u64,
    /// Burst size for read endpoints.
    #[serde(default = "default_read_burst")]
    pub read_burst: u32,
    /// Token replenishment period (seconds) for read endpoints.
    #[serde(default = "default_read_period_secs")]
    pub read_period_secs: u64,
}

fn default_provision_burst() -> u32 {
    2
}
fn default_provision_period_secs() -> u64 {
    60
}
fn default_read_burst() -> u32 {
    30
}
fn default_read_period_secs() -> u64 {
    2
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            provision_burst: default_provision_burst(),
            provision_period_secs: default_provision_period_secs(),
            read_burst: default_read_burst(),
            read_period_secs: default_read_period_secs(),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct Config {
    pub base_dir: PathBuf,
    pub domain: String,
    pub life_in_days: i32,
    pub port_range_start: u32,
    pub port_range_end: u32,
    pub dev_mode: bool,
    pub cors_origin: String,
    pub bind_address: String,
    #[serde(default = "default_sweep_interval_secs")]
    pub sweep_interval_secs: u64,
    pub registry: RegistryConfig,
    pub allocator: AllocatorConfig,
    pub provisioner: ProvisionerConfig,
    #[serde(default)]
    pub ratelimit: RateLimitConfig,
}

fn default_sweep_interval_secs() -> u64 {
    86400
}

impl Config {
    /// Build a [`HelloProvisioner`] from the current configuration.
    ///
    /// `dev_mode` is passed explicitly so callers can override the value from
    /// the config file (e.g. `gl-cli` forces dev mode unless `--prod` is given).
    ///
    /// The provisioner kind is read from `self.provisioner.kind`; only
    /// [`ProvisionerKind::Hello`] is supported today.
    pub fn build_provisioner(&self, dev_mode: bool, sys: Arc<dyn SysRunner>) -> HelloProvisioner {
        let _kind = &self.provisioner.kind; // consulted here; extended in #18 for Ghost
        let storage = self.allocator.build();
        HelloProvisioner::new(
            self.domain.clone(),
            dev_mode,
            self.bind_address.clone(),
            storage,
            sys,
        )
    }

    /// Build a [`GoopyManagerConfig`] from the current configuration.
    pub fn build_manager_config(&self) -> GoopyManagerConfig {
        GoopyManagerConfig {
            base_dir: self.base_dir.clone(),
            domain: self.domain.clone(),
            life_in_days: self.life_in_days,
            port_range_start: self.port_range_start,
            port_range_end: self.port_range_end,
        }
    }

    pub fn from_file(path: &Path) -> Result<Self, Error> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("could not read {}: {}", path.display(), e)))?;
        let cfg: Self = toml::from_str(&contents)
            .map_err(|e| Error::Config(format!("could not parse {}: {}", path.display(), e)))?;
        if cfg.life_in_days <= 0 {
            return Err(Error::Config("life_in_days must be > 0".into()));
        }
        if cfg.port_range_start >= cfg.port_range_end {
            return Err(Error::Config(
                "port_range_start must be less than port_range_end".into(),
            ));
        }
        if let AllocatorKind::Zfs = cfg.allocator.kind {
            if cfg.allocator.pool.trim().is_empty() {
                return Err(Error::Config(
                    "allocator.pool must be set when kind = \"Zfs\"".into(),
                ));
            }
            if cfg.allocator.quota_mb == 0 {
                return Err(Error::Config(
                    "allocator.quota_mb must be > 0 when kind = \"Zfs\"".into(),
                ));
            }
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_config(toml: &str) -> Result<Config, Error> {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(toml.as_bytes()).unwrap();
        Config::from_file(f.path())
    }

    const VALID_BASE: &str = r#"
base_dir = "/tmp/goopy"
domain = "goopy.life"
life_in_days = 7
port_range_start = 9000
port_range_end = 9100
dev_mode = true
cors_origin = "https://goopy.life"
bind_address = "127.0.0.1:8080"
[registry]
path = "/tmp/goopy.db"
[provisioner]
kind = "Hello"
"#;

    #[test]
    fn valid_config_deserializes_correctly() {
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            VALID_BASE
        );
        let cfg = write_config(&toml).expect("should parse");
        assert_eq!(cfg.domain, "goopy.life");
        assert_eq!(cfg.life_in_days, 7);
        assert_eq!(cfg.port_range_start, 9000);
        assert_eq!(cfg.sweep_interval_secs, 86400);
    }

    #[test]
    fn missing_required_field_returns_config_error() {
        // Omit `domain`
        let toml = r#"
base_dir = "/tmp/goopy"
life_in_days = 7
port_range_start = 40000
port_range_end = 49999
dev_mode = false
cors_origin = "https://goopy.life"
bind_address = "0.0.0.0:3000"

[registry]
path = "/tmp/goopy.db"

[allocator]
kind = "PlainDir"

[provisioner]
kind = "Hello"
"#;
        let err = write_config(toml).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn valid_zfs_config_accepted() {
        let toml = format!(
            r#"{}
[allocator]
kind = "Zfs"
pool = "tank"
quota_mb = 512
"#,
            VALID_BASE
        );
        assert!(write_config(&toml).is_ok());
    }

    #[test]
    fn zfs_empty_pool_rejected() {
        let toml = format!(
            r#"{}
[allocator]
kind = "Zfs"
pool = ""
quota_mb = 512
"#,
            VALID_BASE
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("pool")));
    }

    #[test]
    fn zfs_zero_quota_rejected() {
        let toml = format!(
            r#"{}
[allocator]
kind = "Zfs"
pool = "tank"
quota_mb = 0
"#,
            VALID_BASE
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("quota_mb")));
    }

    #[test]
    fn build_manager_config_maps_fields_correctly() {
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            VALID_BASE
        );
        let cfg = write_config(&toml).expect("should parse");
        let manager_cfg = cfg.build_manager_config();
        assert_eq!(manager_cfg.base_dir, cfg.base_dir);
        assert_eq!(manager_cfg.domain, cfg.domain);
        assert_eq!(manager_cfg.life_in_days, cfg.life_in_days);
        assert_eq!(manager_cfg.port_range_start, cfg.port_range_start);
        assert_eq!(manager_cfg.port_range_end, cfg.port_range_end);
        // port_range_start and port_range_end are both u32 — assert distinct
        // values so a field swap in build_manager_config would fail this test.
        assert_ne!(manager_cfg.port_range_start, manager_cfg.port_range_end);
    }

    #[test]
    fn provisioner_section_hello_kind_parses() {
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            VALID_BASE
        );
        let cfg = write_config(&toml).expect("should parse");
        assert_eq!(cfg.provisioner.kind, ProvisionerKind::Hello);
    }

    #[test]
    fn missing_provisioner_section_returns_config_error() {
        let toml = r#"
base_dir = "/tmp/goopy"
domain = "goopy.life"
ssl_email = "admin@goopy.life"
life_in_days = 7
port_range_start = 9000
port_range_end = 9100
dev_mode = true
cors_origin = "https://goopy.life"
bind_address = "127.0.0.1:8080"
[registry]
path = "/tmp/goopy.db"
[allocator]
kind = "PlainDir"
"#;
        let err = write_config(toml).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn top_level_provisioner_kind_field_is_rejected() {
        let toml = r#"
base_dir = "/tmp/goopy"
domain = "goopy.life"
ssl_email = "admin@goopy.life"
life_in_days = 7
provisioner_kind = "Hello"
port_range_start = 9000
port_range_end = 9100
dev_mode = true
cors_origin = "https://goopy.life"
bind_address = "127.0.0.1:8080"
[registry]
path = "/tmp/goopy.db"
[allocator]
kind = "PlainDir"
"#;
        let err = write_config(toml).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }
}
