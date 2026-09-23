use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, instrument};

use super::readiness::{self, ReadinessBudget};
use super::{GoopyProvisioner, dev_process, nginx, systemd};
use crate::Goopy;
use crate::shared_types::*;
use crate::storage_allocator::StorageAllocator;
use crate::sys_utils::SysRunner;

/// Provisions a real, SQLite-backed Ghost instance from a shared base install.
///
/// # The soft-link boundary
///
/// A full Ghost install is ~200 MB of `node_modules`, and installing one per
/// instance would dominate provisioning time. Instead every instance shares a
/// single prepared install at [`GhostConfig::source_dir`] (see
/// `docs/GHOST_PROVISIONER.md` for how to prepare it) and the working directory
/// is assembled from it in two parts:
///
/// * **Symlinked — everything Ghost only ever reads.** The application code
///   ([`SHARED_ENTRIES`]: `index.js`, `core/`, `node_modules/`, `package.json`)
///   and the stock theme. These are identical for every instance, so sharing
///   them costs nothing and pins the instance to the base install's version.
/// * **Materialised — everything Ghost writes.** The `content/` subdirectories
///   ([`CONTENT_DIRS`]) are created as real, empty, per-instance directories:
///   the SQLite database in `content/data`, plus uploaded images, generated
///   assets, logs and settings. Sharing any of these would leak one sandbox's
///   state into another.
///
/// The rule is simply *symlink what is read, materialise what is written*. An
/// instance directory therefore costs a handful of symlinks and empty
/// directories rather than a copy of Ghost.
///
/// Ghost is started with its working directory set to the instance directory,
/// which is where it looks for `config.production.json`, and creates and
/// migrates its own SQLite database on first boot.
///
/// In **production mode** (`dev_mode = false`) the provisioner writes a
/// `goopy-{slug}.service` systemd unit and an nginx reverse-proxy site.
/// **Prerequisite:** a wildcard TLS certificate for the domain must already
/// exist at `/etc/letsencrypt/live/<domain>/`.
///
/// In **dev mode** (`dev_mode = true`) it spawns Ghost directly as a detached
/// background process and records the PID for later cleanup, with no systemd or
/// nginx involvement.
pub struct GhostProvisioner {
    domain: String,
    dev_mode: bool,
    /// Address at which nginx can reach gl-serv, for the `auth_request`
    /// subrequest in each instance's site. A connect destination, not gl-serv's
    /// listen address — see `Config::resolved_api_address` (#149).
    api_address: String,
    ghost: GhostConfig,
    storage: Arc<dyn StorageAllocator>,
    sys: Arc<dyn SysRunner>,
}

/// Ghost-specific settings, deserialized from the `[provisioner]` TOML section
/// when `kind = "Ghost"`.
///
/// These values travel together from config to provisioner, so they are one
/// type rather than a handful of constructor arguments.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GhostConfig {
    /// Prepared base Ghost install that every instance soft-links to.
    /// See `docs/GHOST_PROVISIONER.md` for how to prepare it.
    pub source_dir: PathBuf,
    /// Version of the base install, recorded on each instance it provisions.
    ///
    /// Deliberately configured rather than read from `source_dir/package.json`:
    /// upgrading is an operator action (prepare a new install, repoint the
    /// symlink, bump this), and pinning must not silently follow the base
    /// install if that symlink moves under running instances.
    pub version: String,
    /// Node.js binary used to run Ghost. systemd requires an absolute path.
    #[serde(default = "default_node_bin")]
    pub node_bin: String,
    /// Unprivileged OS user the per-instance systemd unit runs as.
    #[serde(default = "default_service_user")]
    pub service_user: String,
    /// How long a freshly started instance may take to serve, in seconds,
    /// before the spawn is given up on and the instance marked `Failed`.
    ///
    /// See [`readiness`] for why the wait exists. The default has an order of
    /// magnitude of headroom over the ~13 s a lone Ghost took to boot on the
    /// dev droplet, because a loaded host is slow rather than broken.
    #[serde(default = "default_ready_timeout_secs")]
    pub ready_timeout_secs: u64,
    /// Gap between readiness probes, in milliseconds.
    #[serde(default = "default_ready_poll_ms")]
    pub ready_poll_ms: u64,
}

fn default_node_bin() -> String {
    "/usr/bin/node".to_string()
}

fn default_service_user() -> String {
    "goopy".to_string()
}

fn default_ready_timeout_secs() -> u64 {
    readiness::DEFAULT_READY_TIMEOUT_SECS
}

fn default_ready_poll_ms() -> u64 {
    readiness::DEFAULT_READY_POLL_MS
}

/// Entries symlinked from the base install: Ghost's own code, which it reads
/// but never writes.
///
/// Public because `gl-serv --check-config` reads it to verify a configured
/// `source_dir` on the host before a deploy swaps the config in. A directory
/// that is merely present satisfies `is_dir` but is not a prepared Ghost
/// install; tying the check to this list keeps it honest as the list changes.
pub const SHARED_ENTRIES: &[&str] = &["index.js", "core", "node_modules", "package.json"];

/// Per-instance writable directories under `content/`. Ghost creates files in
/// all of these, so each instance needs its own.
const CONTENT_DIRS: &[&str] = &[
    "data", "images", "logs", "settings", "adapters", "public", "files", "media", "themes",
];

impl GhostProvisioner {
    pub fn new(
        domain: String,
        dev_mode: bool,
        api_address: String,
        ghost: GhostConfig,
        storage: Arc<dyn StorageAllocator>,
        sys: Arc<dyn SysRunner>,
    ) -> Self {
        Self {
            domain,
            dev_mode,
            api_address,
            ghost,
            storage,
            sys,
        }
    }

    // ── Instance directory layout ───────────────────────────────────────

    /// Creates `link` pointing at `target`, replacing any existing entry.
    ///
    /// Replacing rather than failing keeps `provision` re-runnable after a
    /// partial failure.
    fn force_symlink(target: &Path, link: &Path) -> Result<(), Error> {
        match fs::remove_file(link) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(e)),
        }
        unix_fs::symlink(target, link).map_err(Error::Io)
    }

    /// Assembles the instance directory: symlinks to the shared install for the
    /// read-only parts, real empty directories for everything Ghost writes.
    fn materialize_instance_dir(&self, working_dir: &Path) -> Result<(), Error> {
        info!(
            working_dir = %working_dir.display(),
            source = %self.ghost.source_dir.display(),
            "linking instance against base Ghost install"
        );

        for entry in SHARED_ENTRIES {
            Self::force_symlink(&self.ghost.source_dir.join(entry), &working_dir.join(entry))?;
        }

        let content = working_dir.join("content");
        for dir in CONTENT_DIRS {
            fs::create_dir_all(content.join(dir)).map_err(Error::Io)?;
        }

        // Stock themes are read-only, so they are shared like the code. Mirror
        // whatever the base install ships rather than naming one: which themes
        // Ghost bundles — and which one a fresh site activates — varies by
        // release. A user-uploaded theme lands in the instance's own (real)
        // content/themes directory alongside these links.
        let source_themes = self.ghost.source_dir.join("content").join("themes");
        for entry in fs::read_dir(&source_themes).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            Self::force_symlink(
                &entry.path(),
                &content.join("themes").join(entry.file_name()),
            )?;
        }

        Ok(())
    }

    // ── Template rendering ──────────────────────────────────────────────

    /// Scheme and host Ghost is told to serve itself at.
    ///
    /// In production this is the nginx-fronted subdomain over TLS; in dev mode
    /// there is no proxy, so the instance is addressed directly on its assigned
    /// port over plain HTTP.
    ///
    /// The readiness probe reads this too, not just the rendered config: Ghost
    /// redirects any request that disagrees with its canonical `url`, so a
    /// probe that claimed the wrong scheme would never see a `200` from a
    /// perfectly healthy instance. Deriving both from one place is what keeps
    /// them from drifting apart.
    fn instance_origin(&self, slug: &str, port: u32) -> (&'static str, String) {
        if self.dev_mode {
            ("http", format!("127.0.0.1:{port}"))
        } else {
            ("https", format!("{}.{}", slug, self.domain))
        }
    }

    /// Public URL Ghost is told to serve itself at.
    fn instance_url(&self, slug: &str, port: u32) -> String {
        let (scheme, host) = self.instance_origin(slug, port);
        format!("{scheme}://{host}")
    }

    /// Renders the per-instance `config.production.json`.
    ///
    /// Built through `serde_json` rather than string formatting so that paths
    /// and the URL are escaped correctly whatever they contain.
    fn render_ghost_config(&self, goopy: &Goopy) -> Result<String, Error> {
        let content = goopy.working_dir.join("content");
        let config = serde_json::json!({
            "url": self.instance_url(&goopy.slug, goopy.port),
            "server": {
                "host": "127.0.0.1",
                "port": goopy.port,
            },
            "database": {
                "client": "sqlite3",
                "connection": {
                    "filename": content.join("data").join("ghost.db"),
                },
                "useNullAsDefault": true,
            },
            // Sandboxes are throwaway, so no mail service is configured.
            // "Direct" attempts SMTP straight from the droplet, which providers
            // block on port 25 — so assume mail never leaves.
            "mail": {
                "transport": "Direct",
            },
            // Ghost 6 defaults `security:staffDeviceVerification` to true and
            // gates *every* staff sign-in behind a code it mails out. With no
            // working transport the code cannot arrive, so the owner account
            // created at setup can never log in: `POST /ghost/api/admin/session/`
            // answers 500 `EmailError/ESOCKET` and the admin UI — the entire
            // point of the sandbox — is unreachable. It does not degrade to the
            // log the way invite and reset URLs do.
            //
            // Turning it off is what Ghost's own config.development.json and
            // config.testing.json do. It costs nothing here: an instance lives a
            // day, and the "account" is whatever address a visitor typed into the
            // setup form, so there is no mailbox behind it and no identity to
            // protect. session-service.js marks the session verified at login, so
            // no code is generated and the mailer is never reached.
            "security": {
                "staffDeviceVerification": false,
            },
            // Ghost's own config.production.json points `explore.update_url`
            // at https://explore.ghost.org/api/update, and on boot
            // `explore-ping-service` POSTs the instance's public URL, site
            // UUID, theme and post counts there. For a sandbox that lives a
            // day, that publishes the address of every instance we hand out to
            // a third party — and it happens once per instance, so it scales
            // with traffic rather than with deployments.
            //
            // The empty string is load-bearing, not an unset leftover.
            // Clearing the URL is Ghost's own supported off-switch: `ping()`
            // opens with
            //
            //     const exploreUrl = this.config.get('explore:update_url');
            //     if (!exploreUrl) { return; }
            //
            // Our per-instance config loads after config.production.json, so
            // this overrides it. Filling the key in — or deleting it as dead
            // weight — turns the ping back on.
            //
            // `explore.testimonials_url` is deliberately not touched: that one
            // is a GET the admin UI makes, not a POST of our data.
            "explore": {
                "update_url": "",
            },
            // `privacy` defaults to `false` in Ghost's defaults.json, which
            // makes `isPrivacyDisabled(flag)` return false for everything, so
            // every privacy-gated feature is on. Two of them reach third
            // parties with data that belongs to whoever is holding the
            // sandbox:
            //
            //   useGravatar  — lib/image/gravatar.js and the member avatar
            //                  service hash an email and fetch it from
            //                  Gravatar. A visitor types a real address into
            //                  the /ghost/ setup form, so that address reaches
            //                  a third party.
            //   useIndexNow  — services/indexnow-ping announces each published
            //                  post's absolute URL to search engines. It skips
            //                  `env == development`, but instances run under
            //                  NODE_ENV=production, so it is live here. This
            //                  one leaks the sandbox URL the same way the
            //                  explore ping does.
            //
            // Named individually rather than with `useTinfoil: true`. Tinfoil
            // is the broader hammer and would also switch off
            // `useStructuredData`, which emits schema.org and OG tags in
            // ghost_head and makes no outbound request at all. Stripping those
            // would quietly change what a sandbox renders — the goal here is
            // to stop instances phoning home, not to hand out a degraded Ghost.
            //
            // These are the only privacy flags 6.63.0 reads in `core/`; a
            // Ghost upgrade can add more, which is why docs/GHOST_PROVISIONER.md
            // calls this list out for re-checking.
            "privacy": {
                "useGravatar": false,
                "useIndexNow": false,
            },
            // Ghost's defaults.json ships
            // `updateCheck: {"url": "https://updates.ghost.org"}` and registers
            // a daily job for it. Emptying the URL stops the request, but —
            // unlike the explore ping — it is NOT a clean off-switch, and the
            // difference is worth knowing before anyone "fixes" this line.
            //
            // Checked against the 6.63.0 source. `update-check/index.js`
            // returns early only on the environment, never on an empty URL, and
            // `update-check-service.js` goes straight from
            // `const checkEndpoint = this.config.checkEndpoint;` to
            // `await this.request(checkEndpoint, reqObj)` with no guard. So the
            // job still runs; the request throws on the empty URL and lands in
            // `updateCheckError`, which records the next check timestamp and
            // logs "Update check failed". `rethrowErrors` defaults to false, so
            // nothing propagates.
            //
            // The cost is therefore one error line per instance per day, in the
            // instance's own content/logs — no outbound request, no effect on
            // boot, nothing the visitor sees. The job is scheduled at a random
            // time in a 24h window and instances live about a day, so most
            // never reach it at all. That is the better end of the trade: the
            // alternative is leaving the default and letting every sandbox make
            // a daily call to a third party.
            //
            // Note the 6.x payload is narrower than 5.x's: it is now a GET
            // carrying only `ghost_version`, not a POST of site stats. Ghost 5's
            // `privacy.useUpdateCheck` is gone in 6 and only ever downgraded
            // POST to GET — it never disabled the check — so there is no
            // privacy flag to use here instead.
            "updateCheck": {
                "url": "",
            },
            "logging": {
                "transports": ["file", "stdout"],
                "level": "info",
                "path": content.join("logs"),
            },
            "paths": {
                "contentPath": content,
            },
        });
        serde_json::to_string_pretty(&config)
            .map_err(|e| Error::Config(format!("could not render Ghost config: {e}")))
    }

    fn write_ghost_config(&self, goopy: &Goopy) -> Result<(), Error> {
        let path = goopy.working_dir.join("config.production.json");
        info!(path = %path.display(), "writing Ghost config");
        fs::write(&path, self.render_ghost_config(goopy)?).map_err(Error::Io)
    }

    fn render_service_file(&self, slug: &str, working_dir: &Path) -> String {
        format!(
            r#"[Unit]
Description=Goopy Ghost - {slug}
After=network.target

[Service]
Type=simple
User={user}
Group={user}
WorkingDirectory={working_dir}
Environment=NODE_ENV=production
ExecStart={node_bin} "{working_dir}/index.js"
Restart=on-failure
RestartSec=5
KillMode=mixed
TimeoutStopSec=30

[Install]
WantedBy=multi-user.target
"#,
            user = self.ghost.service_user,
            node_bin = self.ghost.node_bin,
            working_dir = working_dir.display(),
        )
    }

    fn service_name(slug: &str) -> String {
        format!("goopy-{slug}")
    }

    /// How long to wait for a freshly started instance, and how often to ask.
    fn readiness_budget(&self) -> ReadinessBudget {
        ReadinessBudget {
            timeout: Duration::from_secs(self.ghost.ready_timeout_secs),
            poll_interval: Duration::from_millis(self.ghost.ready_poll_ms),
        }
    }

    // ── Inner provision (post-allocate steps) ───────────────────────────

    fn provision_inner(&self, goopy: &Goopy) -> Result<(), Error> {
        self.materialize_instance_dir(&goopy.working_dir)?;
        self.write_ghost_config(goopy)?;

        if self.dev_mode {
            // NODE_ENV selects config.production.json; dev mode refers to *our*
            // mode, not Ghost's, and there is only ever one config per instance.
            dev_process::spawn(
                self.sys.as_ref(),
                &goopy.working_dir,
                &self.ghost.node_bin,
                &["index.js"],
                &[("NODE_ENV", "production")],
                "ghost.log",
            )?;
            let (scheme, host) = self.instance_origin(&goopy.slug, goopy.port);
            readiness::wait_until_ready(
                self.sys.as_ref(),
                &goopy.slug,
                goopy.port,
                readiness::InstanceOrigin {
                    scheme,
                    host: &host,
                },
                self.readiness_budget(),
            )?;
        } else {
            systemd::install_and_start(
                self.sys.as_ref(),
                &Self::service_name(&goopy.slug),
                &self.render_service_file(&goopy.slug, &goopy.working_dir),
            )?;
            // Before nginx, not after: the site is the visitor-facing half, and
            // there is no point publishing a route to a Ghost that still
            // answers its maintenance page. A `Type=simple` unit is "active"
            // the moment `node` forks, so this is the only step that knows
            // whether the instance actually works.
            let (scheme, host) = self.instance_origin(&goopy.slug, goopy.port);
            readiness::wait_until_ready(
                self.sys.as_ref(),
                &goopy.slug,
                goopy.port,
                readiness::InstanceOrigin {
                    scheme,
                    host: &host,
                },
                self.readiness_budget(),
            )?;
            nginx::install_site(
                self.sys.as_ref(),
                &goopy.slug,
                &self.domain,
                goopy.port,
                &self.api_address,
            )?;
        }
        Ok(())
    }
}

impl GoopyProvisioner for GhostProvisioner {
    fn kind(&self) -> ProvisionerKind {
        ProvisionerKind::Ghost
    }

    /// The configured version of the base install. Recorded on each instance so
    /// it stays pinned to the Ghost it was created with, even after the operator
    /// upgrades the base install for subsequent instances.
    fn service_version(&self) -> &str {
        &self.ghost.version
    }

    #[instrument(skip(self), fields(slug = %goopy.slug, dev_mode = self.dev_mode))]
    fn provision(&self, goopy: &Goopy) -> Result<(), Error> {
        // Step 1: allocate storage
        self.storage.allocate(&goopy.working_dir)?;

        // Step 2: all post-allocate steps; release storage on failure
        if let Err(e) = self.provision_inner(goopy) {
            let _ = self.storage.release(&goopy.working_dir);
            return Err(e);
        }

        info!(slug = %goopy.slug, version = %self.ghost.version, "provisioning complete");
        Ok(())
    }

    #[instrument(skip(self), fields(slug = %goopy.slug, dev_mode = self.dev_mode))]
    fn deprovision(&self, goopy: &Goopy) -> Result<(), Error> {
        let result = if self.dev_mode {
            dev_process::kill(self.sys.as_ref(), &goopy.working_dir)
        } else {
            systemd::stop_and_remove(self.sys.as_ref(), &Self::service_name(&goopy.slug))
                .and_then(|_| nginx::remove_site(self.sys.as_ref(), &goopy.slug))
        };
        // Releasing the working directory removes the symlinks themselves, not
        // the shared base install they point at.
        if let Err(e) = self.storage.release(&goopy.working_dir) {
            tracing::warn!(error = %e, slug = %goopy.slug, "storage release failed during deprovision");
        }
        result?;
        info!(slug = %goopy.slug, "deprovisioning complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_allocator::PlainDirAllocator;
    use crate::sys_utils::{MOCK_SPAWNED_PID, MockCall, MockProbe, MockSysRunner};
    use tempfile::{TempDir, tempdir};

    /// Themes a stock Ghost 5.x install ships. A fresh site activates `source`,
    /// so linking only `casper` leaves the instance without its active theme.
    const FAKE_STOCK_THEMES: &[&str] = &["casper", "source"];

    /// Builds a stand-in for a prepared base Ghost install: the entries the
    /// provisioner symlinks, and the stock themes.
    fn fake_ghost_source() -> TempDir {
        let source = tempdir().unwrap();
        fs::write(source.path().join("index.js"), "// ghost entrypoint").unwrap();
        fs::write(
            source.path().join("package.json"),
            r#"{"version":"5.87.1"}"#,
        )
        .unwrap();
        fs::create_dir_all(source.path().join("core")).unwrap();
        fs::create_dir_all(source.path().join("node_modules")).unwrap();
        for theme in FAKE_STOCK_THEMES {
            fs::create_dir_all(source.path().join("content").join("themes").join(theme)).unwrap();
        }
        source
    }

    fn test_goopy(working_dir: &Path, port: u32) -> Goopy {
        Goopy {
            slug: "tasty-lucky-clover".to_string(),
            life_in_days: 7,
            created_at: chrono::Utc::now(),
            working_dir: working_dir.to_path_buf(),
            port,
            status: Status::Spawning,
            provisioner_kind: ProvisionerKind::Ghost,
            service_version: "5.87.1".to_string(),
        }
    }

    fn provisioner(dev_mode: bool, source: &TempDir, sys: Arc<dyn SysRunner>) -> GhostProvisioner {
        provisioner_with_budget(dev_mode, source, sys, default_ready_timeout_secs(), 5)
    }

    /// A provisioner with an explicit readiness budget, for the tests that are
    /// about waiting rather than about provisioning steps.
    fn provisioner_with_budget(
        dev_mode: bool,
        source: &TempDir,
        sys: Arc<dyn SysRunner>,
        ready_timeout_secs: u64,
        ready_poll_ms: u64,
    ) -> GhostProvisioner {
        GhostProvisioner::new(
            "goopy.life".to_string(),
            dev_mode,
            "127.0.0.1:3000".to_string(),
            GhostConfig {
                source_dir: source.path().to_path_buf(),
                version: "5.87.1".to_string(),
                node_bin: "/usr/bin/node".to_string(),
                service_user: "goopy".to_string(),
                ready_timeout_secs,
                ready_poll_ms,
            },
            Arc::new(PlainDirAllocator),
            sys,
        )
    }

    #[test]
    fn kind_is_ghost() {
        let source = fake_ghost_source();
        let p = provisioner(true, &source, Arc::new(MockSysRunner::new()));
        assert_eq!(p.kind(), ProvisionerKind::Ghost);
    }

    #[test]
    fn service_version_reports_configured_ghost_version() {
        let source = fake_ghost_source();
        let p = provisioner(true, &source, Arc::new(MockSysRunner::new()));
        assert_eq!(
            p.service_version(),
            "5.87.1",
            "instances must be pinned to the configured base-install version"
        );
    }

    #[test]
    fn dev_provision_symlinks_shared_code_and_materializes_content() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(true, &source, Arc::new(MockSysRunner::new()));
        p.provision(&test_goopy(&working_dir, 9876))
            .expect("dev provision should succeed");

        for entry in SHARED_ENTRIES {
            let link = working_dir.join(entry);
            assert!(
                link.symlink_metadata().unwrap().file_type().is_symlink(),
                "{entry} should be a symlink into the base install"
            );
            assert_eq!(
                fs::read_link(&link).unwrap(),
                source.path().join(entry),
                "{entry} should point at the base install"
            );
        }

        for dir in CONTENT_DIRS {
            let path = working_dir.join("content").join(dir);
            assert!(
                !path.symlink_metadata().unwrap().file_type().is_symlink(),
                "content/{dir} must be a real per-instance directory, not shared"
            );
            assert!(path.is_dir(), "content/{dir} should exist");
        }

        for theme in FAKE_STOCK_THEMES {
            let link = working_dir.join("content").join("themes").join(theme);
            assert!(
                link.symlink_metadata().unwrap().file_type().is_symlink(),
                "stock theme {theme} is read-only and should be shared"
            );
            assert_eq!(
                fs::read_link(&link).unwrap(),
                source.path().join("content").join("themes").join(theme),
                "stock theme {theme} should point at the base install"
            );
        }
    }

    #[test]
    fn dev_provision_links_every_theme_the_base_install_ships() {
        let source = fake_ghost_source();
        // Not one of Ghost's current defaults: the provisioner mirrors the base
        // install rather than a hardcoded list of theme names.
        fs::create_dir_all(source.path().join("content").join("themes").join("edition")).unwrap();

        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(true, &source, Arc::new(MockSysRunner::new()));
        p.provision(&test_goopy(&working_dir, 9876))
            .expect("dev provision should succeed");

        let link = working_dir.join("content").join("themes").join("edition");
        assert_eq!(
            fs::read_link(&link).unwrap(),
            source.path().join("content").join("themes").join("edition"),
            "every theme in the base install should be linked, not just the known defaults"
        );
    }

    #[test]
    fn dev_provision_writes_sqlite_backed_config() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(true, &source, Arc::new(MockSysRunner::new()));
        p.provision(&test_goopy(&working_dir, 9876))
            .expect("dev provision should succeed");

        let raw = fs::read_to_string(working_dir.join("config.production.json"))
            .expect("config.production.json should be written");
        let cfg: serde_json::Value =
            serde_json::from_str(&raw).expect("config should be valid JSON");

        assert_eq!(cfg["database"]["client"], "sqlite3");
        assert_eq!(
            cfg["database"]["connection"]["filename"],
            working_dir
                .join("content")
                .join("data")
                .join("ghost.db")
                .display()
                .to_string(),
            "the database must live in the instance's own content/data"
        );
        assert_eq!(cfg["server"]["port"], 9876);
        assert_eq!(
            cfg["paths"]["contentPath"],
            working_dir.join("content").display().to_string()
        );
    }

    #[test]
    fn prod_config_url_is_the_instance_subdomain() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(false, &source, Arc::new(MockSysRunner::new()));
        let raw = p
            .render_ghost_config(&test_goopy(&working_dir, 9876))
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(cfg["url"], "https://tasty-lucky-clover.goopy.life");
    }

    /// Ghost 6 mails a device-verification code on every staff sign-in unless
    /// this is off, and these instances have no working mail transport — so
    /// leaving it at Ghost's default makes the owner account created at setup
    /// impossible to log in with, and the admin UI unreachable. The failure is
    /// a 500 from `POST /ghost/api/admin/session/`, nowhere near provisioning,
    /// which is why it is pinned here rather than left to a droplet to find.
    #[test]
    fn prod_config_disables_staff_device_verification() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(false, &source, Arc::new(MockSysRunner::new()));
        let raw = p
            .render_ghost_config(&test_goopy(&working_dir, 9876))
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();

        // Ghost reads this as `config.get(...) !== true`, so it must be present
        // and must not be the string "false".
        assert_eq!(
            cfg["security"]["staffDeviceVerification"],
            serde_json::Value::Bool(false),
            "staff device verification must be disabled, or no one can log in \
             to an instance: Ghost 6 defaults it on and mails a code that a \
             sandbox with no mail transport can never deliver"
        );
    }

    /// The explore ping POSTs the instance's public URL and site stats to
    /// explore.ghost.org on every boot. Ghost gates it on the URL being
    /// truthy, so the empty string is the off-switch — and it is exactly the
    /// kind of line that reads like an oversight and gets "tidied up", which
    /// is why it is pinned by a test.
    #[test]
    fn prod_config_disables_the_explore_ping() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(false, &source, Arc::new(MockSysRunner::new()));
        let raw = p
            .render_ghost_config(&test_goopy(&working_dir, 9876))
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(
            cfg["explore"]["update_url"],
            serde_json::Value::String(String::new()),
            "an empty explore update_url is what stops Ghost POSTing this \
             sandbox's public URL to explore.ghost.org on boot"
        );
        assert!(
            cfg["explore"].get("testimonials_url").is_none(),
            "testimonials_url is a GET the admin UI makes, not a POST of our \
             data, so we deliberately leave it at Ghost's default"
        );
    }

    /// Gravatar sends the email a visitor typed at setup to a third party, and
    /// the IndexNow ping announces the sandbox's post URLs to search engines.
    /// Ghost reads these as `config.get('privacy')[flag] === false`, so they
    /// must be present and must be real booleans, not the string "false".
    #[test]
    fn prod_config_disables_the_privacy_gated_phone_homes() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(false, &source, Arc::new(MockSysRunner::new()));
        let raw = p
            .render_ghost_config(&test_goopy(&working_dir, 9876))
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(
            cfg["privacy"]["useGravatar"],
            serde_json::Value::Bool(false),
            "the email a visitor types at /ghost/ setup must not be hashed and \
             sent to Gravatar"
        );
        assert_eq!(
            cfg["privacy"]["useIndexNow"],
            serde_json::Value::Bool(false),
            "instances run under NODE_ENV=production, where the IndexNow ping \
             is live and would announce this sandbox's post URLs to search \
             engines"
        );
    }

    /// `useTinfoil` would disable every privacy-gated feature in one line,
    /// including `useStructuredData`, which only emits schema.org and OG tags
    /// and makes no outbound request. A sandbox should render like a real
    /// Ghost, so the flags are named individually on purpose.
    #[test]
    fn prod_config_leaves_render_only_privacy_features_alone() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(false, &source, Arc::new(MockSysRunner::new()));
        let raw = p
            .render_ghost_config(&test_goopy(&working_dir, 9876))
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert!(
            cfg["privacy"].get("useTinfoil").is_none(),
            "useTinfoil is the blanket switch and would also strip structured \
             data from the rendered pages"
        );
        assert!(
            cfg["privacy"].get("useStructuredData").is_none(),
            "structured data makes no outbound request, so it stays at Ghost's \
             default"
        );
    }

    /// Emptying the update-check URL stops the outbound call, but Ghost 6.63.0
    /// has no guard on it, so the daily job still runs and logs a failure. That
    /// is a deliberate trade, not an oversight — the assertion exists so the
    /// key is not silently dropped, and the comment on it survives with it.
    #[test]
    fn prod_config_clears_the_update_check_url() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(false, &source, Arc::new(MockSysRunner::new()));
        let raw = p
            .render_ghost_config(&test_goopy(&working_dir, 9876))
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(
            cfg["updateCheck"]["url"],
            serde_json::Value::String(String::new()),
            "an empty update-check URL is what stops the daily call to \
             updates.ghost.org"
        );
        assert!(
            cfg["updateCheck"].get("forceUpdate").is_none(),
            "forceUpdate would schedule an extra check at boot on top of the \
             daily job"
        );
    }

    /// Every instance is handed to a stranger for a day, so none of these may
    /// regress independently of the others. Reading them off one rendered
    /// config is what makes "this sandbox is not reported to anyone" checkable
    /// in one place rather than spread across four tests.
    #[test]
    fn prod_config_leaves_no_third_party_endpoint_enabled() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(false, &source, Arc::new(MockSysRunner::new()));
        let raw = p
            .render_ghost_config(&test_goopy(&working_dir, 9876))
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(cfg["explore"]["update_url"], "");
        assert_eq!(cfg["updateCheck"]["url"], "");
        assert_eq!(
            cfg["privacy"]["useGravatar"],
            serde_json::Value::Bool(false)
        );
        assert_eq!(
            cfg["privacy"]["useIndexNow"],
            serde_json::Value::Bool(false)
        );

        // Ghost's stats pipeline is off only because `tinybird` defaults to
        // null. If a future Ghost ships a default endpoint for it, this catches
        // the day the default changes rather than the day someone notices the
        // traffic.
        assert!(
            cfg.get("tinybird").is_none(),
            "tinybird defaults to null upstream; if that stops being true this \
             config has to set it explicitly"
        );
    }

    /// Dev instances are just as exposed as production ones — they run on a
    /// public droplet under a real hostname — so the same keys apply.
    #[test]
    fn dev_config_also_disables_the_phone_homes() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(true, &source, Arc::new(MockSysRunner::new()));
        let raw = p
            .render_ghost_config(&test_goopy(&working_dir, 9876))
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(cfg["explore"]["update_url"], "");
        assert_eq!(cfg["updateCheck"]["url"], "");
        assert_eq!(
            cfg["privacy"]["useGravatar"],
            serde_json::Value::Bool(false)
        );
        assert_eq!(
            cfg["privacy"]["useIndexNow"],
            serde_json::Value::Bool(false)
        );
    }

    #[test]
    fn dev_config_url_addresses_the_port_directly() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(true, &source, Arc::new(MockSysRunner::new()));
        let raw = p
            .render_ghost_config(&test_goopy(&working_dir, 9876))
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            cfg["url"], "http://127.0.0.1:9876",
            "dev mode has no nginx in front, so the URL must be directly reachable"
        );
    }

    #[test]
    fn dev_provision_spawns_ghost_and_records_pid() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let mock = Arc::new(MockSysRunner::new());
        let p = provisioner(true, &source, mock.clone());
        p.provision(&test_goopy(&working_dir, 9876))
            .expect("dev provision should succeed");

        let calls = mock.recorded_calls();
        let spawn = calls
            .iter()
            .find_map(|c| match c {
                MockCall::SpawnDetached {
                    program,
                    args,
                    working_dir,
                    envs,
                    ..
                } => Some((program, args, working_dir, envs)),
                _ => None,
            })
            .expect("dev mode should spawn Ghost");

        assert_eq!(spawn.0, "/usr/bin/node");
        assert_eq!(spawn.1, &["index.js"]);
        assert_eq!(
            spawn.2, &working_dir,
            "Ghost must run from the instance dir"
        );
        assert!(
            spawn
                .3
                .contains(&("NODE_ENV".to_string(), "production".to_string())),
            "NODE_ENV=production is what makes Ghost read config.production.json"
        );

        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, MockCall::SudoRun { .. } | MockCall::SudoWrite { .. })),
            "dev mode must not touch systemd or nginx"
        );

        let pid = fs::read_to_string(working_dir.join("server.pid")).expect("PID file");
        assert_eq!(pid, MOCK_SPAWNED_PID.to_string());
    }

    #[test]
    fn dev_deprovision_kills_ghost_and_removes_working_dir() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let mock = Arc::new(MockSysRunner::new());
        let p = provisioner(true, &source, mock.clone());
        let goopy = test_goopy(&working_dir, 9876);
        p.provision(&goopy).expect("dev provision should succeed");
        p.deprovision(&goopy)
            .expect("dev deprovision should succeed");

        let killed = mock
            .recorded_calls()
            .into_iter()
            .any(|c| matches!(c, MockCall::KillPid { pid } if pid == MOCK_SPAWNED_PID));
        assert!(killed, "deprovision should kill the spawned Ghost process");
        assert!(
            !working_dir.exists(),
            "working dir should be removed after deprovision"
        );
        assert!(
            source.path().join("node_modules").is_dir(),
            "releasing an instance must not follow symlinks into the base install"
        );
    }

    #[test]
    fn prod_provision_calls_expected_sys_commands() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let mock = Arc::new(MockSysRunner::new());
        let p = provisioner(false, &source, mock.clone());
        p.provision(&test_goopy(&working_dir, 9876))
            .expect("prod provision should succeed");

        let unit = mock
            .recorded_calls()
            .into_iter()
            .find_map(|c| match c {
                MockCall::SudoWrite { path, content }
                    if path == "/etc/systemd/system/goopy-tasty-lucky-clover.service" =>
                {
                    Some(content)
                }
                _ => None,
            })
            .expect("should write a systemd unit");
        assert!(unit.contains("Environment=NODE_ENV=production"));
        assert!(unit.contains("User=goopy"), "Ghost must not run as root");
        assert!(unit.contains(&format!(
            "ExecStart=/usr/bin/node \"{}/index.js\"",
            working_dir.display()
        )));

        assert!(
            mock.sudo_write_paths()
                .contains(&"/etc/nginx/sites-available/goopy-tasty-lucky-clover".to_string()),
            "should write the nginx site"
        );

        let args = mock.sudo_run_args();
        let verb_seq: Vec<&str> = args
            .iter()
            .map(String::as_str)
            .filter(|a| ["daemon-reload", "enable", "start", "ln", "reload"].contains(a))
            .collect();
        assert_eq!(
            verb_seq,
            ["daemon-reload", "enable", "start", "ln", "reload"]
        );
    }

    /// `Done` has to mean "serving". The nginx site is the visitor-facing half,
    /// so it must not be published until the instance answers — otherwise the
    /// URL is handed over while Ghost is still showing its maintenance page.
    #[test]
    fn prod_provision_waits_for_the_instance_before_publishing_the_nginx_site() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let mock = Arc::new(MockSysRunner::new());
        let p = provisioner(false, &source, mock.clone());
        p.provision(&test_goopy(&working_dir, 9876))
            .expect("prod provision should succeed");

        let calls = mock.recorded_calls();
        let probe_at = calls
            .iter()
            .position(|c| matches!(c, MockCall::HttpProbe { .. }))
            .expect("provisioning must probe the instance");
        let site_at = calls
            .iter()
            .position(|c| {
                matches!(c, MockCall::SudoWrite { path, .. }
                    if path == "/etc/nginx/sites-available/goopy-tasty-lucky-clover")
            })
            .expect("provisioning must write the nginx site");
        let start_at = calls
            .iter()
            .position(
                |c| matches!(c, MockCall::SudoRun { args } if args.contains(&"start".to_string())),
            )
            .expect("provisioning must start the unit");

        assert!(
            start_at < probe_at && probe_at < site_at,
            "the probe belongs between starting the unit and publishing the site"
        );
        assert_eq!(
            mock.http_probes(),
            [("127.0.0.1:9876".to_string(), "/".to_string())],
            "the instance is probed on its own port, not through the public URL"
        );
    }

    /// Measured on the dev droplet: a fully booted Ghost 6.63.0 answers `301`,
    /// not `200`, to a bare loopback request, because it enforces its canonical
    /// `https://{slug}.{domain}` url. The probe takes a shortcut around nginx
    /// and so has to carry the origin nginx would have forwarded, or the wait
    /// can only ever end in a timeout against a perfectly healthy instance.
    #[test]
    fn prod_probe_claims_the_https_origin_the_instance_serves() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let mock = Arc::new(MockSysRunner::new());
        let p = provisioner(false, &source, mock.clone());
        p.provision(&test_goopy(&working_dir, 9876))
            .expect("prod provision should succeed");

        assert_eq!(
            mock.http_probe_origins(),
            [(
                "https".to_string(),
                "tasty-lucky-clover.goopy.life".to_string()
            )],
            "the probe must present the origin Ghost is configured for"
        );
    }

    /// The mirror image, and why the scheme cannot be a constant: a dev
    /// instance's canonical url is `http://127.0.0.1:{port}`, so a probe
    /// claiming `https` would be redirected exactly as firmly.
    #[test]
    fn dev_probe_claims_the_http_origin_the_instance_serves() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let mock = Arc::new(MockSysRunner::new());
        let p = provisioner(true, &source, mock.clone());
        p.provision(&test_goopy(&working_dir, 9876))
            .expect("dev provision should succeed");

        assert_eq!(
            mock.http_probe_origins(),
            [("http".to_string(), "127.0.0.1:9876".to_string())],
            "a dev instance serves itself over plain HTTP on its own port"
        );
    }

    /// The probe origin and the url written into `config.production.json` are
    /// derived from one place, so they cannot drift into disagreeing.
    #[test]
    fn the_probe_origin_matches_the_url_ghost_is_configured_with() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        for dev_mode in [false, true] {
            let p = provisioner(dev_mode, &source, Arc::new(MockSysRunner::new()));
            let goopy = test_goopy(&working_dir, 9876);
            let cfg: serde_json::Value =
                serde_json::from_str(&p.render_ghost_config(&goopy).unwrap()).unwrap();
            let (scheme, host) = p.instance_origin(&goopy.slug, goopy.port);

            assert_eq!(
                cfg["url"],
                format!("{scheme}://{host}"),
                "dev_mode={dev_mode}: the probe would claim an origin Ghost \
                 does not serve"
            );
        }
    }

    /// An instance that never boots must not be handed to anyone. Failing here
    /// is what releases its port and marks it `Failed`, which `sweep()` reaps.
    #[test]
    fn prod_provision_fails_when_the_instance_never_serves() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        // A Ghost stuck on its maintenance page for longer than its budget.
        let mock = Arc::new(MockSysRunner::with_probes(vec![MockProbe::Status(503)]));
        let p = provisioner_with_budget(false, &source, mock.clone(), 1, 50);

        let err = p
            .provision(&test_goopy(&working_dir, 9876))
            .expect_err("an instance that never serves is a failed spawn");

        match err {
            Error::ReadinessTimeout { last, .. } => {
                assert!(last.contains("503"), "got {last:?}");
            }
            other => panic!("expected a readiness timeout, got {other:?}"),
        }
        assert!(
            !mock
                .sudo_write_paths()
                .contains(&"/etc/nginx/sites-available/goopy-tasty-lucky-clover".to_string()),
            "a Ghost that never served must never get a public route"
        );
        assert!(
            !working_dir.exists(),
            "a failed provision releases its working directory"
        );
    }

    /// Dev mode has no nginx, but it starts the same Ghost and therefore has the
    /// same race — a local run would otherwise report `Done` seconds early.
    #[test]
    fn dev_provision_also_waits_for_the_instance() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let mock = Arc::new(MockSysRunner::new());
        let p = provisioner(true, &source, mock.clone());
        p.provision(&test_goopy(&working_dir, 9876))
            .expect("dev provision should succeed");

        assert_eq!(
            mock.http_probes(),
            [("127.0.0.1:9876".to_string(), "/".to_string())]
        );
    }

    #[test]
    fn prod_deprovision_calls_expected_sys_commands() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let mock = Arc::new(MockSysRunner::new());
        let p = provisioner(false, &source, mock.clone());
        p.deprovision(&test_goopy(&working_dir, 9876))
            .expect("prod deprovision should succeed");

        let args = mock.sudo_run_args();
        let verb_seq: Vec<&str> = args
            .iter()
            .map(String::as_str)
            .filter(|a| ["stop", "disable", "daemon-reload", "reload"].contains(a))
            .collect();
        assert_eq!(verb_seq, ["stop", "disable", "daemon-reload", "reload"]);

        assert!(
            args.contains(&"/etc/systemd/system/goopy-tasty-lucky-clover.service".to_string()),
            "should remove the systemd unit"
        );
        assert!(
            args.contains(&"/etc/nginx/sites-available/goopy-tasty-lucky-clover".to_string()),
            "should remove the nginx site"
        );
    }

    /// A `Failed` instance may never have had its unit installed. sweep() reaps
    /// those, so deprovision must push past a failing stop/disable and still
    /// remove the unit file, the nginx site and the working directory —
    /// otherwise the instance is stranded and holds its capacity slot forever.
    #[test]
    fn prod_deprovision_survives_a_never_installed_unit() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let sys = Arc::new(MockSysRunner::failing_sudo_run(|args| {
            matches!(args.get(1), Some(&"stop") | Some(&"disable"))
        }));
        let p = provisioner(false, &source, sys.clone());

        p.deprovision(&test_goopy(&working_dir, 9876))
            .expect("deprovision must succeed even when the unit does not exist");

        let args = sys.sudo_run_args();
        assert!(
            args.contains(&"/etc/systemd/system/goopy-tasty-lucky-clover.service".to_string()),
            "unit file must still be removed"
        );
        assert!(
            args.contains(&"/etc/nginx/sites-available/goopy-tasty-lucky-clover".to_string()),
            "nginx site must still be removed"
        );
    }

    #[test]
    fn slug_is_never_passed_through_a_shell() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let mock = Arc::new(MockSysRunner::new());
        let p = provisioner(false, &source, mock.clone());
        p.provision(&test_goopy(&working_dir, 9876)).unwrap();
        p.deprovision(&test_goopy(&working_dir, 9876)).unwrap();

        for arg in mock.sudo_run_args() {
            assert!(
                !arg.contains(' ') && !arg.contains(';') && !arg.contains('&'),
                "argument {arg:?} looks like a shell string; each value must be its own arg"
            );
        }
    }

    #[test]
    fn provision_is_rerunnable_after_a_partial_failure() {
        let source = fake_ghost_source();
        let base = tempdir().unwrap();
        let working_dir = base.path().join("tasty-lucky-clover");

        let p = provisioner(true, &source, Arc::new(MockSysRunner::new()));
        let goopy = test_goopy(&working_dir, 9876);
        p.provision(&goopy).expect("first provision");
        p.provision(&goopy)
            .expect("re-provisioning over an existing instance dir should replace the symlinks");

        assert_eq!(
            fs::read_link(working_dir.join("core")).unwrap(),
            source.path().join("core")
        );
    }
}
