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
