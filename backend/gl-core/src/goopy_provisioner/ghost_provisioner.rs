use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{info, instrument};

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
    /// Address on which gl-serv listens; used by nginx `auth_request` subrequests.
    api_address: String,
    ghost: GhostConfig,
    storage: Arc<dyn StorageAllocator>,
    sys: Arc<dyn SysRunner>,
}

/// Ghost-specific settings, deserialized from the `[provisioner]` TOML section
/// when `kind = "Ghost"`.
///
/// These four values travel together from config to provisioner, so they are one
/// type rather than four constructor arguments.
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
}

fn default_node_bin() -> String {
    "/usr/bin/node".to_string()
}

fn default_service_user() -> String {
    "goopy".to_string()
}

/// Entries symlinked from the base install: Ghost's own code, which it reads
/// but never writes.
const SHARED_ENTRIES: &[&str] = &["index.js", "core", "node_modules", "package.json"];

/// Per-instance writable directories under `content/`. Ghost creates files in
/// all of these, so each instance needs its own.
const CONTENT_DIRS: &[&str] = &[
    "data", "images", "logs", "settings", "adapters", "public", "files", "media", "themes",
];

/// Stock theme shipped with Ghost. Read-only, so it is shared like the code.
const STOCK_THEME: &str = "casper";

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

        // The stock theme is read-only; a user-uploaded theme lands in the
        // instance's own (real) content/themes directory alongside this link.
        Self::force_symlink(
            &self
                .ghost
                .source_dir
                .join("content")
                .join("themes")
                .join(STOCK_THEME),
            &content.join("themes").join(STOCK_THEME),
        )
    }

    // ── Template rendering ──────────────────────────────────────────────

    /// Public URL Ghost is told to serve itself at.
    ///
    /// In production this is the nginx-fronted subdomain; in dev mode there is
    /// no proxy, so the instance is addressed directly on its assigned port.
    fn instance_url(&self, slug: &str, port: u32) -> String {
        if self.dev_mode {
            format!("http://127.0.0.1:{port}")
        } else {
            format!("https://{}.{}", slug, self.domain)
        }
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
            // Sandboxes are throwaway, so no mail service is configured; Ghost
            // falls back to printing invite/reset URLs into its log.
            "mail": {
                "transport": "Direct",
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
        } else {
            systemd::install_and_start(
                self.sys.as_ref(),
                &Self::service_name(&goopy.slug),
                &self.render_service_file(&goopy.slug, &goopy.working_dir),
            )?;
            nginx::install_site(
                self.sys.as_ref(),
                &goopy.slug,
                &self.domain,
                goopy.port,
                &self.api_address,
            )?;
            nginx::reload(self.sys.as_ref())?;
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
    use crate::sys_utils::{MOCK_SPAWNED_PID, MockCall, MockSysRunner};
    use tempfile::{TempDir, tempdir};

    /// Builds a stand-in for a prepared base Ghost install: the entries the
    /// provisioner symlinks, and the stock theme.
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
        fs::create_dir_all(
            source
                .path()
                .join("content")
                .join("themes")
                .join(STOCK_THEME),
        )
        .unwrap();
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
        GhostProvisioner::new(
            "goopy.life".to_string(),
            dev_mode,
            "127.0.0.1:3000".to_string(),
            GhostConfig {
                source_dir: source.path().to_path_buf(),
                version: "5.87.1".to_string(),
                node_bin: "/usr/bin/node".to_string(),
                service_user: "goopy".to_string(),
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

        let theme = working_dir.join("content").join("themes").join(STOCK_THEME);
        assert!(
            theme.symlink_metadata().unwrap().file_type().is_symlink(),
            "the stock theme is read-only and should be shared"
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
            .any(|c| matches!(c, MockCall::KillPid { pid } if pid == MOCK_SPAWNED_PID.to_string()));
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
            .sudo_written_content("/etc/systemd/system/goopy-tasty-lucky-clover.service")
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
