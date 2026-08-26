use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::goopy_manager::GoopyManagerConfig;
use crate::goopy_provisioner::GoopyProvisioner;
use crate::goopy_provisioner::ghost_provisioner::GhostProvisioner;
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
#[derive(Debug, serde::Deserialize)]
pub struct ProvisionerConfig {
    pub kind: ProvisionerKind,
    /// Prepared base Ghost install that instances soft-link to.
    /// Required when `kind = "Ghost"`; ignored otherwise.
    /// See `docs/GHOST_PROVISIONER.md` for how to prepare it.
    #[serde(default)]
    pub ghost_source_dir: PathBuf,
    /// Version of the base install, recorded as each instance's `service_version`.
    /// Required when `kind = "Ghost"`; ignored otherwise. Bump it after upgrading
    /// the base install — existing instances keep the version they were created with.
    #[serde(default)]
    pub ghost_version: String,
    /// Node.js binary used to run Ghost. systemd needs an absolute path, so this
    /// is not resolved through `PATH`. Ignored unless `kind = "Ghost"`.
    #[serde(default = "default_node_bin")]
    pub node_bin: String,
    /// Unprivileged OS user each instance's systemd unit runs as. Must be able to
    /// write the instance working directory. Ignored unless `kind = "Ghost"`.
    #[serde(default = "default_service_user")]
    pub service_user: String,
}

fn default_node_bin() -> String {
    "/usr/bin/node".to_string()
}

fn default_service_user() -> String {
    "goopy".to_string()
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
    /// Maximum number of **resident** (running) instances allowed simultaneously.
    ///
    /// RAM-bound. Each Ghost process is roughly 150–250 MB. On a 2 GB droplet
    /// minus OS/nginx/gl-serv overhead that leaves capacity for ~10 concurrent
    /// instances (2 GB ÷ ~200 MB ≈ 10). Conservative default.
    ///
    /// Raise this cap once the machine is upgraded or profile data shows lower
    /// per-instance RSS in practice.
    ///
    /// Must be `<= max_provisioned`, which `Config::from_file` enforces — a
    /// larger value could never be reached. See [`Config::max_provisioned`] for
    /// when this cap is reachable at all.
    #[serde(default = "default_max_active")]
    pub max_active: u32,
    /// Maximum number of instances that may **exist on disk** at any time.
    ///
    /// Disk-bound. On a 50 GB droplet with a 512 MB per-instance quota the
    /// theoretical ceiling is ~90 instances (50 GB ÷ 512 MB). The beta default
    /// is kept equal to `max_active` because scale-to-zero (#96) has not landed
    /// yet; once idle instances can suspend to ~0 RAM, raise this toward the
    /// disk ceiling while `max_active` stays small.
    ///
    /// # Reachability of the two caps
    ///
    /// Active instances are a subset of provisioned ones, and this cap is
    /// checked first, so while `max_active == max_provisioned` the RAM cap can
    /// never trip and gl-serv can only ever answer `server_full`, never
    /// `server_busy`. The `max_active` cap — and that error code — become
    /// reachable once the two diverge, which is what #96 enables.
    ///
    /// # Why `Failed` instances count
    ///
    /// Not because they hold resources: a spawn that failed has already had its
    /// port released (`GoopyManager::spawn`) and its working directory released
    /// (`HelloProvisioner::provision`). It is counted because it is still a row
    /// in the registry, and because a `Failed` row left by a failed *despawn*
    /// does still hold both its port and its directory.
    ///
    /// The sweep reaps `Failed` rows unconditionally, so one occupies a slot for
    /// at most `sweep_interval_secs`.
    #[serde(default = "default_max_provisioned")]
    pub max_provisioned: u32,
    pub registry: RegistryConfig,
    pub allocator: AllocatorConfig,
    pub provisioner: ProvisionerConfig,
    #[serde(default)]
    pub ratelimit: RateLimitConfig,
}

fn default_sweep_interval_secs() -> u64 {
    86400
}

/// Default RAM-bound resident-instance cap. See [`Config::max_active`].
fn default_max_active() -> u32 {
    10
}

/// Default disk-bound total-instance cap. See [`Config::max_provisioned`].
///
/// Kept equal to [`default_max_active`] until scale-to-zero (#96) ships, which
/// means the RAM cap is unreachable under the shipped defaults — see the
/// reachability note on [`Config::max_provisioned`].
fn default_max_provisioned() -> u32 {
    10
}

impl Config {
    /// Build the provisioner named by `self.provisioner.kind`.
    ///
    /// `dev_mode` is passed explicitly so callers can override the value from
    /// the config file (e.g. `gl-cli` forces dev mode unless `--prod` is given).
    ///
    /// Returned boxed because the kind is only known at runtime; the forwarding
    /// impl in `goopy_provisioner` keeps it usable as `GoopyManager`'s generic
    /// provisioner parameter.
    pub fn build_provisioner(
        &self,
        dev_mode: bool,
        sys: Arc<dyn SysRunner>,
    ) -> Box<dyn GoopyProvisioner + Send + Sync> {
        let storage = self.allocator.build();
        match self.provisioner.kind {
            ProvisionerKind::Hello => Box::new(HelloProvisioner::new(
                self.domain.clone(),
                dev_mode,
                self.bind_address.clone(),
                storage,
                sys,
            )),
            ProvisionerKind::Ghost => Box::new(GhostProvisioner::new(
                self.domain.clone(),
                dev_mode,
                self.bind_address.clone(),
                self.provisioner.ghost_source_dir.clone(),
                self.provisioner.ghost_version.clone(),
                self.provisioner.node_bin.clone(),
                self.provisioner.service_user.clone(),
                storage,
                sys,
            )),
        }
    }

    /// Build a [`GoopyManagerConfig`] from the current configuration.
    pub fn build_manager_config(&self) -> GoopyManagerConfig {
        GoopyManagerConfig {
            base_dir: self.base_dir.clone(),
            domain: self.domain.clone(),
            life_in_days: self.life_in_days,
            port_range_start: self.port_range_start,
            port_range_end: self.port_range_end,
            max_active: self.max_active,
            max_provisioned: self.max_provisioned,
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
        // A zero cap turns every spawn into a 503 with no startup error at all,
        // which is an easy typo to make and near-impossible to diagnose from
        // outside the service.
        if cfg.max_active == 0 {
            return Err(Error::Config("max_active must be > 0".into()));
        }
        if cfg.max_provisioned == 0 {
            return Err(Error::Config("max_provisioned must be > 0".into()));
        }
        // Active instances are a subset of provisioned ones, so a larger
        // max_active is silently inert rather than merely generous.
        if cfg.max_active > cfg.max_provisioned {
            return Err(Error::Config(
                "max_active must be <= max_provisioned".into(),
            ));
        }
        // A zero burst or period cannot be turned into a rate limiter, so reject
        // it here rather than letting gl-serv panic while building its router.
        if cfg.ratelimit.provision_burst == 0 {
            return Err(Error::Config(
                "ratelimit.provision_burst must be > 0".into(),
            ));
        }
        if cfg.ratelimit.provision_period_secs == 0 {
            return Err(Error::Config(
                "ratelimit.provision_period_secs must be > 0".into(),
            ));
        }
        if cfg.ratelimit.read_burst == 0 {
            return Err(Error::Config("ratelimit.read_burst must be > 0".into()));
        }
        if cfg.ratelimit.read_period_secs == 0 {
            return Err(Error::Config(
                "ratelimit.read_period_secs must be > 0".into(),
            ));
        }
        if let ProvisionerKind::Ghost = cfg.provisioner.kind {
            if cfg.provisioner.ghost_source_dir.as_os_str().is_empty() {
                return Err(Error::Config(
                    "provisioner.ghost_source_dir must be set when kind = \"Ghost\"".into(),
                ));
            }
            if cfg.provisioner.ghost_version.trim().is_empty() {
                return Err(Error::Config(
                    "provisioner.ghost_version must be set when kind = \"Ghost\"".into(),
                ));
            }
            if cfg.provisioner.node_bin.trim().is_empty() {
                return Err(Error::Config(
                    "provisioner.node_bin must not be empty when kind = \"Ghost\"".into(),
                ));
            }
            if cfg.provisioner.service_user.trim().is_empty() {
                return Err(Error::Config(
                    "provisioner.service_user must not be empty when kind = \"Ghost\"".into(),
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

    /// Splice top-level `keys` into `base` ahead of its first table header.
    /// Appending them instead would land them inside the trailing
    /// `[provisioner]` table, where they are silently ignored.
    fn with_caps(base: &str, keys: &str) -> String {
        base.replace("[registry]", &format!("{keys}\n[registry]"))
    }

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
    fn zero_max_active_rejected() {
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            with_caps(VALID_BASE, "max_active = 0")
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("max_active")));
    }

    #[test]
    fn zero_max_provisioned_rejected() {
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            with_caps(VALID_BASE, "max_provisioned = 0")
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("max_provisioned")));
    }

    #[test]
    fn max_active_above_max_provisioned_rejected() {
        // An active cap above the provisioned cap can never be reached, since
        // active instances are a subset of provisioned ones.
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            with_caps(VALID_BASE, "max_active = 20\nmax_provisioned = 10")
        );
        let err = write_config(&toml).unwrap_err();
        assert!(
            matches!(err, Error::Config(ref s) if s.contains("max_active must be <= max_provisioned")),
            "got {err:?}"
        );
    }

    #[test]
    fn max_active_below_max_provisioned_accepted() {
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            with_caps(VALID_BASE, "max_active = 10\nmax_provisioned = 20")
        );
        let cfg = write_config(&toml).expect("diverged caps are valid");
        assert_eq!(cfg.max_active, 10);
        assert_eq!(cfg.max_provisioned, 20);
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

    const GHOST_BASE: &str = r#"
base_dir = "/tmp/goopy"
domain = "goopy.life"
life_in_days = 7
port_range_start = 9000
port_range_end = 9100
dev_mode = false
cors_origin = "https://goopy.life"
bind_address = "127.0.0.1:8080"
[registry]
path = "/tmp/goopy.db"
[allocator]
kind = "PlainDir"
"#;

    #[test]
    fn provisioner_section_ghost_kind_parses_with_defaults() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Ghost"
ghost_source_dir = "/opt/goopy-life/ghost"
ghost_version = "5.87.1"
"#,
            GHOST_BASE
        );
        let cfg = write_config(&toml).expect("should parse");
        assert_eq!(cfg.provisioner.kind, ProvisionerKind::Ghost);
        assert_eq!(
            cfg.provisioner.ghost_source_dir,
            PathBuf::from("/opt/goopy-life/ghost")
        );
        assert_eq!(cfg.provisioner.ghost_version, "5.87.1");
        assert_eq!(cfg.provisioner.node_bin, "/usr/bin/node");
        assert_eq!(cfg.provisioner.service_user, "goopy");
    }

    #[test]
    fn ghost_missing_source_dir_rejected() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Ghost"
ghost_version = "5.87.1"
"#,
            GHOST_BASE
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("ghost_source_dir")));
    }

    #[test]
    fn ghost_missing_version_rejected() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Ghost"
ghost_source_dir = "/opt/goopy-life/ghost"
"#,
            GHOST_BASE
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("ghost_version")));
    }

    #[test]
    fn hello_kind_ignores_missing_ghost_fields() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Hello"
"#,
            GHOST_BASE
        );
        assert!(
            write_config(&toml).is_ok(),
            "Ghost-only fields must not be required for kind = \"Hello\""
        );
    }

    #[test]
    fn build_provisioner_returns_the_configured_kind() {
        for (kind, extra) in [
            ("Hello", ""),
            (
                "Ghost",
                "ghost_source_dir = \"/opt/goopy-life/ghost\"\nghost_version = \"5.87.1\"",
            ),
        ] {
            let toml = format!("{GHOST_BASE}\n[provisioner]\nkind = \"{kind}\"\n{extra}\n");
            let cfg = write_config(&toml).expect("should parse");
            let provisioner = cfg.build_provisioner(true, Arc::new(crate::RealSysRunner));
            assert_eq!(
                provisioner.kind().to_string(),
                kind,
                "build_provisioner should honour provisioner.kind"
            );
        }
    }

    #[test]
    fn ghost_provisioner_stamps_the_configured_version() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Ghost"
ghost_source_dir = "/opt/goopy-life/ghost"
ghost_version = "5.87.1"
"#,
            GHOST_BASE
        );
        let cfg = write_config(&toml).expect("should parse");
        let provisioner = cfg.build_provisioner(true, Arc::new(crate::RealSysRunner));
        assert_eq!(provisioner.service_version(), "5.87.1");
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

    /// Build a config with `[allocator]` plus a custom `[ratelimit]` section.
    fn write_config_with_ratelimit(ratelimit: &str) -> Result<Config, Error> {
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
{}
"#,
            VALID_BASE, ratelimit
        );
        write_config(&toml)
    }

    #[test]
    fn omitted_ratelimit_section_uses_defaults() {
        let cfg = write_config_with_ratelimit("").expect("should parse");
        assert_eq!(cfg.ratelimit.provision_burst, 2);
        assert_eq!(cfg.ratelimit.provision_period_secs, 60);
        assert_eq!(cfg.ratelimit.read_burst, 30);
        assert_eq!(cfg.ratelimit.read_period_secs, 2);
    }

    #[test]
    fn partial_ratelimit_section_defaults_the_rest() {
        let cfg = write_config_with_ratelimit("[ratelimit]\nprovision_burst = 5\n")
            .expect("should parse");
        assert_eq!(cfg.ratelimit.provision_burst, 5);
        assert_eq!(cfg.ratelimit.read_burst, 30);
    }

    #[test]
    fn zero_provision_burst_returns_config_error() {
        let err = write_config_with_ratelimit("[ratelimit]\nprovision_burst = 0\n").unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("provision_burst")));
    }

    #[test]
    fn zero_provision_period_returns_config_error() {
        let err =
            write_config_with_ratelimit("[ratelimit]\nprovision_period_secs = 0\n").unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("provision_period_secs")));
    }

    #[test]
    fn zero_read_burst_returns_config_error() {
        let err = write_config_with_ratelimit("[ratelimit]\nread_burst = 0\n").unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("read_burst")));
    }

    #[test]
    fn zero_read_period_returns_config_error() {
        let err = write_config_with_ratelimit("[ratelimit]\nread_period_secs = 0\n").unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("read_period_secs")));
    }
}
