use std::path::{Path, PathBuf};

use crate::shared_types::{AllocatorKind, Error, ProvisionerKind};

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

#[derive(Debug, serde::Deserialize)]
pub struct Config {
    pub base_dir: PathBuf,
    pub domain: String,
    pub ssl_email: String,
    pub life_in_days: i32,
    pub provisioner_kind: ProvisionerKind,
    pub port_range_start: u32,
    pub port_range_end: u32,
    pub dev_mode: bool,
    pub cors_origin: String,
    pub bind_address: String,
    #[serde(default = "default_sweep_interval_secs")]
    pub sweep_interval_secs: u64,
    pub registry: RegistryConfig,
    pub allocator: AllocatorConfig,
}

fn default_sweep_interval_secs() -> u64 {
    86400
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
ssl_email = "admin@goopy.life"
life_in_days = 7
provisioner_kind = "Hello"
port_range_start = 40000
port_range_end = 49999
dev_mode = false
cors_origin = "https://goopy.life"
bind_address = "0.0.0.0:3000"

[registry]
path = "/tmp/goopy.db"

[allocator]
kind = "PlainDir"
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
}

impl Config {
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
            if cfg.allocator.pool.is_empty() {
                return Err(Error::Config(
            if cfg.allocator.pool.trim().is_empty() {
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
