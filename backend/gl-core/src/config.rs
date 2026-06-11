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
    use std::io::Write as _;

    #[test]
    fn valid_config_deserializes_correctly() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
base_dir = "/tmp/goopy"
domain = "goopy.life"
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
"#
        )
        .unwrap();
        let cfg = Config::from_file(f.path()).expect("should parse");
        assert_eq!(cfg.domain, "goopy.life");
        assert_eq!(cfg.life_in_days, 7);
        assert_eq!(cfg.port_range_start, 40000);
        assert_eq!(cfg.sweep_interval_secs, 86400);
    }

    #[test]
    fn missing_required_field_returns_config_error() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        // Omit `domain`
        write!(
            f,
            r#"
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
"#
        )
        .unwrap();
        let err = Config::from_file(f.path()).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
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
        Ok(cfg)
    }
}
