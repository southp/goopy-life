use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "config error: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, serde::Deserialize)]
pub struct Config {
    pub base_dir: PathBuf,
    pub domain: String,
    pub ssl_email: String,
    pub life_in_days: i32,
    /// "Hello" or "Ghost"
    pub provisioner_kind: String,
    pub port_range_start: u32,
    pub port_range_end: u32,
    pub zfs_pool: String,
    pub storage_quota_mb: u64,
    pub dev_mode: bool,
    pub db_path: PathBuf,
    pub cors_origin: String,
    pub bind_address: String,
    #[serde(default = "default_sweep_interval_secs")]
    pub sweep_interval_secs: u64,
}

fn default_sweep_interval_secs() -> u64 {
    86400
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| ConfigError(format!("could not read {}: {}", path.display(), e)))?;
        toml::from_str(&contents)
            .map_err(|e| ConfigError(format!("could not parse {}: {}", path.display(), e)))
    }
}
