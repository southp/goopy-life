use std::path::{Path, PathBuf};

use crate::shared_types::{Error, ProvisionerKind};

#[derive(Debug, serde::Deserialize)]
pub struct RegistryConfig {
    pub path: PathBuf,
}

#[derive(Debug, serde::Deserialize)]
pub struct AllocatorConfig {
    pub pool: String,
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
        Ok(cfg)
    }
}
