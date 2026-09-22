use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::goopy_manager::GoopyManagerConfig;
use crate::goopy_provisioner::GoopyProvisioner;
use crate::goopy_provisioner::ghost_provisioner::{GhostConfig, GhostProvisioner};
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
/// Unlike [`AllocatorConfig`], which stays a flat struct because its extra
/// fields are simply ignored under the other kind, a provisioner's settings are
/// strictly kind-specific: Ghost needs values that are meaningless to Hello, and
/// two of them are mandatory. Making this a tagged enum lets serde enforce that
/// at parse time — a `Ghost` section missing `source_dir` fails to load — rather
/// than needing a block of hand-written validation in [`Config::from_file`].
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum ProvisionerConfig {
    Hello,
    Ghost(GhostConfig),
}

impl ProvisionerConfig {
    /// The kind discriminant, for logging and display.
    pub fn kind(&self) -> ProvisionerKind {
        match self {
            ProvisionerConfig::Hello => ProvisionerKind::Hello,
            ProvisionerConfig::Ghost(_) => ProvisionerKind::Ghost,
        }
    }
}

/// Upper bound on [`RateLimitConfig::alive_cache_secs`].
///
/// A cached affirmative answer keeps an instance reachable for up to this long
/// after it expires, so the window is capped rather than left to the operator:
/// expiry is a promise to the person who spawned the instance, and a config
/// typo should not be able to stretch it into minutes.
const MAX_ALIVE_CACHE_SECS: u64 = 60;

/// Rate limiting configuration (`[ratelimit]` section in TOML).
///
/// Three independent GCRA (Generic Cell Rate Algorithm) buckets are configured:
///
/// * **Provision limit** — applied to `POST /goopies` only.  Defaults to a
///   burst of 2 requests with one token replenished every 60 seconds, so a
///   single IP can spawn at most 2 instances back-to-back and then must wait
///   1 minute per additional spawn.  This matches the expected interaction
///   pattern (one deliberate click) while blocking trivial abuse on the
///   expensive provisioning path.
///
/// * **Read limit** — applied to the endpoints a browser or the frontend calls
///   directly (`GET /goopies/:slug`, `GET /config`, `GET /capacity`).
///   Defaults to a burst of 30 requests with one token replenished every 2
///   seconds, comfortable for a frontend that polls every few seconds but
///   still rejects floods.
///
/// * **Alive limit** — applied to `GET /goopies/:slug/alive` alone.  This one
///   is not a user-facing read: nginx runs it as an `auth_request` subrequest
///   **once per HTTP request to an instance**, so its natural rate is the
///   instance's entire traffic volume rather than a person clicking around.
///   Sharing the read budget meant a single Ghost admin page, which pulls
///   ~45 subresources at once, exhausted it and every asset came back 500
///   (`auth_request` turns any non-2xx/401/403 into a 500).  Defaults to a
///   burst of 600 — roughly a dozen such page loads back-to-back — with one
///   token replenished every second.
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
    /// Burst size for the nginx `auth_request` liveness check.
    #[serde(default = "default_alive_burst")]
    pub alive_burst: u32,
    /// Token replenishment period (seconds) for the liveness check.
    #[serde(default = "default_alive_period_secs")]
    pub alive_period_secs: u64,
    /// How long nginx may cache an *affirmative* liveness answer, in seconds.
    ///
    /// Sent as `Cache-Control: max-age` on the 200. Denials are always
    /// `no-store`, so this never delays an expiry or (once #96 lands) a wake.
    /// The tradeoff it does buy: an instance that expires mid-window stays
    /// reachable for up to this long.
    #[serde(default = "default_alive_cache_secs")]
    pub alive_cache_secs: u64,
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
fn default_alive_burst() -> u32 {
    600
}
fn default_alive_period_secs() -> u64 {
    1
}
fn default_alive_cache_secs() -> u64 {
    5
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            provision_burst: default_provision_burst(),
            provision_period_secs: default_provision_period_secs(),
            read_burst: default_read_burst(),
            read_period_secs: default_read_period_secs(),
            alive_burst: default_alive_burst(),
            alive_period_secs: default_alive_period_secs(),
            alive_cache_secs: default_alive_cache_secs(),
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
    /// Where gl-serv listens. A wildcard is meaningful here and nowhere else —
    /// see [`Config::api_address`].
    pub bind_address: String,
    /// Where things *on the host* connect to reach gl-serv — today the
    /// `auth_request` subrequest in every per-instance nginx site.
    ///
    /// Separate from [`Config::bind_address`] because the two answer different
    /// questions, and only one of them tolerates a wildcard: `0.0.0.0` is a
    /// perfectly good instruction to listen on every interface and a
    /// meaningless address to connect to. Until #149 it was the same string,
    /// which baked `proxy_pass http://0.0.0.0:3000/goopies/{slug}/alive` into
    /// every site — working only because Linux treats a connect to `0.0.0.0` as
    /// loopback, and leaving the alive-check for every live instance one
    /// unrelated widening of the listen address away from breaking.
    ///
    /// Optional: left unset it is derived from `bind_address` by
    /// [`Config::resolved_api_address`], which is what keeps existing configs
    /// working. Set it explicitly only when nginx must reach gl-serv at an
    /// address the listen address does not imply.
    #[serde(default)]
    pub api_address: Option<String>,
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
    /// The sweep reaps `Failed` rows unconditionally, so one normally occupies
    /// a slot for at most `sweep_interval_secs`. A row whose teardown keeps
    /// failing is the exception: it is retried every sweep and counted against
    /// this cap until it succeeds, which is why the sweep reports what it
    /// removed rather than what it attempted (#117).
    #[serde(default = "default_max_provisioned")]
    pub max_provisioned: u32,
    pub registry: RegistryConfig,
    pub allocator: AllocatorConfig,
    pub provisioner: ProvisionerConfig,
    #[serde(default)]
    pub ratelimit: RateLimitConfig,
}

/// Default sweep frequency: hourly.
///
/// Was 24h, which bought very little — a sweep over an empty registry is a
/// single `SELECT` — and cost a lot: a slot held by an instance that expired
/// (or whose provisioning failed) stays unusable until the next sweep, so the
/// interval is the worst-case delay on reclaiming capacity. An hour keeps that
/// delay short enough that a full server recovers on its own within an hour
/// (#117).
fn default_sweep_interval_secs() -> u64 {
    3600
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
    /// The address something else on the host connects to in order to reach
    /// gl-serv, and the value a provisioner interpolates into each instance's
    /// nginx site.
    ///
    /// [`Config::api_address`] when the config names one. Otherwise
    /// `bind_address` with a wildcard replaced by the matching loopback
    /// address, because an address naming *every* interface names no
    /// destination: `0.0.0.0:3000` derives `127.0.0.1:3000` and `[::]:3000`
    /// derives `[::1]:3000`, while an address that already picks an interface
    /// — `10.0.0.5:3000`, `127.0.0.1:3000` — is a connect destination as it
    /// stands and is kept verbatim.
    ///
    /// An unparseable `bind_address` comes back untouched; [`Config::from_file`]
    /// rejects one, so that branch is unreachable for a config read from a file
    /// and exists only so this stays total for a hand-built [`Config`].
    pub fn resolved_api_address(&self) -> String {
        if let Some(explicit) = &self.api_address {
            return explicit.clone();
        }
        let Ok(bind) = self.bind_address.parse::<SocketAddr>() else {
            return self.bind_address.clone();
        };
        if !bind.ip().is_unspecified() {
            return bind.to_string();
        }
        let loopback = match bind {
            SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        };
        SocketAddr::new(loopback, bind.port()).to_string()
    }

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
        let api_address = self.resolved_api_address();
        match &self.provisioner {
            ProvisionerConfig::Hello => Box::new(HelloProvisioner::new(
                self.domain.clone(),
                dev_mode,
                api_address,
                storage,
                sys,
            )),
            ProvisionerConfig::Ghost(ghost) => Box::new(GhostProvisioner::new(
                self.domain.clone(),
                dev_mode,
                api_address,
                ghost.clone(),
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
        // A zero readiness budget would hand the instance over before it serves,
        // which is the bug the wait exists to fix (#151); a zero poll interval
        // would spin on the instance instead of waiting for it.
        if let ProvisionerConfig::Ghost(ghost) = &cfg.provisioner {
            if ghost.ready_timeout_secs == 0 {
                return Err(Error::Config(
                    "provisioner.ready_timeout_secs must be > 0".into(),
                ));
            }
            if ghost.ready_poll_ms == 0 {
                return Err(Error::Config(
                    "provisioner.ready_poll_ms must be > 0".into(),
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
        // A zero interval cannot be turned into a `tokio::time::interval`, so
        // gl-serv panics on it while spawning the sweep task — after the deploy
        // has already swapped the config and restarted the unit.
        if cfg.sweep_interval_secs == 0 {
            return Err(Error::Config("sweep_interval_secs must be > 0".into()));
        }
        // gl-serv binds this verbatim. Rejecting it here rather than at
        // `TcpListener::bind` is what lets `--check-config` catch it before the
        // swap: a hostname or a missing port is an address the process can
        // parse a config out of but never start on.
        if cfg.bind_address.parse::<SocketAddr>().is_err() {
            return Err(Error::Config(format!(
                "bind_address must be an IP address and port, e.g. \
                 \"127.0.0.1:3000\"; got {:?}",
                cfg.bind_address
            )));
        }
        // The provisioner interpolates this into every per-instance nginx site
        // as a `proxy_pass` destination, so it has to be an address something
        // can connect *to*. A wildcard is not one: it happens to reach loopback
        // on Linux, which is exactly why `0.0.0.0` sat in every rendered site
        // unnoticed until #149. Checking the *resolved* value rather than only
        // an explicit `api_address` is what keeps a future widening of
        // `bind_address` from reaching a `proxy_pass` again.
        let api_address = cfg.resolved_api_address();
        match api_address.parse::<SocketAddr>() {
            Ok(addr) if addr.ip().is_unspecified() => {
                return Err(Error::Config(format!(
                    "api_address must be an address nginx can connect to, not a \
                     wildcard; got {api_address:?}"
                )));
            }
            Ok(addr) if addr.port() == 0 => {
                return Err(Error::Config(format!(
                    "api_address must name a real port; got {api_address:?} \
                     (it takes bind_address's port when unset)"
                )));
            }
            Ok(_) => {}
            Err(_) => {
                return Err(Error::Config(format!(
                    "api_address must be an IP address and port, e.g. \
                     \"127.0.0.1:3000\"; got {api_address:?}"
                )));
            }
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
        if cfg.ratelimit.alive_burst == 0 {
            return Err(Error::Config("ratelimit.alive_burst must be > 0".into()));
        }
        if cfg.ratelimit.alive_period_secs == 0 {
            return Err(Error::Config(
                "ratelimit.alive_period_secs must be > 0".into(),
            ));
        }
        // Zero is allowed here and means "do not cache": `max-age=0` is a valid
        // instruction, unlike a zero burst or period, which cannot be turned
        // into a rate limiter at all. An operator who wants exact expiry
        // timing at the cost of one subrequest per asset can set it.
        if cfg.ratelimit.alive_cache_secs > MAX_ALIVE_CACHE_SECS {
            return Err(Error::Config(format!(
                "ratelimit.alive_cache_secs must be <= {MAX_ALIVE_CACHE_SECS}; \
                 a longer window keeps expired instances reachable"
            )));
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
        assert_eq!(cfg.sweep_interval_secs, 3600);
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
    fn zero_sweep_interval_rejected() {
        // Left to gl-serv this is a panic while spawning the sweep task, which
        // a deploy only discovers after it has swapped the config in.
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            with_caps(VALID_BASE, "sweep_interval_secs = 0")
        );
        let err = write_config(&toml).unwrap_err();
        assert!(
            matches!(err, Error::Config(ref s) if s.contains("sweep_interval_secs must be > 0")),
            "got {err:?}"
        );
    }

    #[test]
    fn bind_address_without_a_port_rejected() {
        // The shape of the typo that matters: still a valid IP, still valid
        // TOML, and unusable as the address gl-serv listens on.
        let toml = format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            VALID_BASE.replace(
                r#"bind_address = "127.0.0.1:8080""#,
                r#"bind_address = "0.0.0.0""#
            )
        );
        let err = write_config(&toml).unwrap_err();
        assert!(
            matches!(err, Error::Config(ref s) if s.contains("bind_address must be an IP address and port")),
            "got {err:?}"
        );
    }

    /// Build a parseable config whose `bind_address` is `bind`, plus any extra
    /// top-level `keys`. Both deployed configs listen on a wildcard, so this is
    /// the shape every `api_address` assertion below starts from.
    fn config_with(bind: &str, keys: &str) -> Result<Config, Error> {
        let base = VALID_BASE.replace(
            r#"bind_address = "127.0.0.1:8080""#,
            &format!(r#"bind_address = "{bind}""#),
        );
        write_config(&format!(
            r#"{}
[allocator]
kind = "PlainDir"
"#,
            with_caps(&base, keys)
        ))
    }

    /// The whole point of #149: a config that says "listen everywhere" must
    /// still hand the provisioner somewhere to *connect*, without the operator
    /// having to know that the two were ever the same field.
    #[test]
    fn a_wildcard_bind_address_derives_a_loopback_api_address() {
        let cfg = config_with("0.0.0.0:3000", "").expect("should parse");
        assert_eq!(cfg.resolved_api_address(), "127.0.0.1:3000");
    }

    /// The derived loopback follows the family of the wildcard: `127.0.0.1` is
    /// not reachable on a host whose gl-serv only ever bound IPv6.
    #[test]
    fn an_ipv6_wildcard_bind_address_derives_ipv6_loopback() {
        let cfg = config_with("[::]:3000", "").expect("should parse");
        assert_eq!(cfg.resolved_api_address(), "[::1]:3000");
    }

    /// A listen address that already picks an interface *is* a connect
    /// destination, so it is passed through rather than rewritten — rewriting
    /// it to loopback would break a host that deliberately fronts gl-serv from
    /// another address.
    #[test]
    fn a_specific_bind_address_is_its_own_api_address() {
        let cfg = config_with("10.0.0.5:3000", "").expect("should parse");
        assert_eq!(cfg.resolved_api_address(), "10.0.0.5:3000");
    }

    #[test]
    fn an_explicit_api_address_wins_over_the_derived_one() {
        let cfg =
            config_with("0.0.0.0:3000", r#"api_address = "10.0.0.5:3000""#).expect("should parse");
        assert_eq!(cfg.resolved_api_address(), "10.0.0.5:3000");
    }

    /// The wildcard must not be able to reach a `proxy_pass` by the front door
    /// either. It resolves to loopback on Linux, so this fails silently in
    /// production and only on the day someone runs the service elsewhere.
    #[test]
    fn a_wildcard_api_address_is_rejected() {
        let err = config_with("127.0.0.1:8080", r#"api_address = "0.0.0.0:3000""#).unwrap_err();
        assert!(
            matches!(err, Error::Config(ref s) if s.contains("api_address must be an address nginx can connect to")),
            "got {err:?}"
        );
    }

    #[test]
    fn an_api_address_without_a_port_is_rejected() {
        let err = config_with("127.0.0.1:8080", r#"api_address = "127.0.0.1""#).unwrap_err();
        assert!(
            matches!(err, Error::Config(ref s) if s.contains("api_address must be an IP address and port")),
            "got {err:?}"
        );
    }

    /// Port 0 means "any free port" to a listener and nothing at all to a
    /// client, so it is caught here rather than rendered into every site.
    #[test]
    fn a_portless_derived_api_address_is_rejected() {
        let err = config_with("0.0.0.0:0", "").unwrap_err();
        assert!(
            matches!(err, Error::Config(ref s) if s.contains("api_address must name a real port")),
            "got {err:?}"
        );
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
        assert_eq!(cfg.provisioner.kind(), ProvisionerKind::Hello);
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
source_dir = "/opt/goopy-life/ghost"
version = "5.87.1"
"#,
            GHOST_BASE
        );
        let cfg = write_config(&toml).expect("should parse");
        let ProvisionerConfig::Ghost(ghost) = &cfg.provisioner else {
            panic!("expected a Ghost provisioner config");
        };
        assert_eq!(ghost.source_dir, PathBuf::from("/opt/goopy-life/ghost"));
        assert_eq!(ghost.version, "5.87.1");
        assert_eq!(ghost.node_bin, "/usr/bin/node");
        assert_eq!(ghost.service_user, "goopy");
        assert_eq!(ghost.ready_timeout_secs, 120);
        assert_eq!(ghost.ready_poll_ms, 500);
    }

    #[test]
    fn ghost_readiness_budget_is_tunable() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Ghost"
source_dir = "/opt/goopy-life/ghost"
version = "5.87.1"
ready_timeout_secs = 300
ready_poll_ms = 250
"#,
            GHOST_BASE
        );
        let cfg = write_config(&toml).expect("should parse");
        let ProvisionerConfig::Ghost(ghost) = &cfg.provisioner else {
            panic!("expected a Ghost provisioner config");
        };
        assert_eq!(ghost.ready_timeout_secs, 300);
        assert_eq!(ghost.ready_poll_ms, 250);
    }

    /// A zero budget would hand the instance over before it serves — the exact
    /// bug the wait exists to fix.
    #[test]
    fn ghost_zero_ready_timeout_rejected() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Ghost"
source_dir = "/opt/goopy-life/ghost"
version = "5.87.1"
ready_timeout_secs = 0
"#,
            GHOST_BASE
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("ready_timeout_secs")));
    }

    #[test]
    fn ghost_zero_ready_poll_rejected() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Ghost"
source_dir = "/opt/goopy-life/ghost"
version = "5.87.1"
ready_poll_ms = 0
"#,
            GHOST_BASE
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("ready_poll_ms")));
    }

    #[test]
    fn ghost_missing_source_dir_rejected() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Ghost"
version = "5.87.1"
"#,
            GHOST_BASE
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("source_dir")));
    }

    #[test]
    fn ghost_missing_version_rejected() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Ghost"
source_dir = "/opt/goopy-life/ghost"
"#,
            GHOST_BASE
        );
        let err = write_config(&toml).unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("version")));
    }

    #[test]
    fn hello_kind_needs_no_ghost_fields() {
        let toml = format!(
            r#"{}
[provisioner]
kind = "Hello"
"#,
            GHOST_BASE
        );
        assert!(
            write_config(&toml).is_ok(),
            "the Hello variant carries no Ghost settings"
        );
    }

    #[test]
    fn build_provisioner_returns_the_configured_kind() {
        for (kind, extra) in [
            ("Hello", ""),
            (
                "Ghost",
                "source_dir = \"/opt/goopy-life/ghost\"\nversion = \"5.87.1\"",
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
source_dir = "/opt/goopy-life/ghost"
version = "5.87.1"
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
        assert_eq!(cfg.ratelimit.alive_burst, 600);
        assert_eq!(cfg.ratelimit.alive_period_secs, 1);
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

    #[test]
    fn zero_alive_burst_returns_config_error() {
        let err = write_config_with_ratelimit("[ratelimit]\nalive_burst = 0\n").unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("alive_burst")));
    }

    #[test]
    fn zero_alive_period_returns_config_error() {
        let err = write_config_with_ratelimit("[ratelimit]\nalive_period_secs = 0\n").unwrap_err();
        assert!(matches!(err, Error::Config(ref s) if s.contains("alive_period_secs")));
    }

    /// The liveness budget exists precisely because it must not be the read
    /// budget; a default that merely matched it would silently reintroduce the
    /// exhaustion this split was made to fix.
    #[test]
    fn alive_budget_is_far_larger_than_the_read_budget() {
        let cfg = write_config_with_ratelimit("").expect("should parse");
        assert!(
            cfg.ratelimit.alive_burst > cfg.ratelimit.read_burst * 10,
            "alive_burst {} must dwarf read_burst {} — one Ghost page spends \
             ~45 tokens on subresource liveness checks alone",
            cfg.ratelimit.alive_burst,
            cfg.ratelimit.read_burst
        );
    }
}
