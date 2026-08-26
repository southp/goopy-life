use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{info, instrument};

use super::{GoopyProvisioner, nginx};
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
/// single prepared install at `ghost_source_dir` (see
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
    /// Prepared base Ghost install that every instance soft-links to.
    ghost_source_dir: PathBuf,
    /// Version of the base install, recorded on each instance it provisions.
    ghost_version: String,
    /// Node.js binary used to run Ghost. systemd requires an absolute path.
    node_bin: String,
    /// Unprivileged OS user the per-instance systemd unit runs as.
    service_user: String,
    storage: Arc<dyn StorageAllocator>,
    sys: Arc<dyn SysRunner>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        domain: String,
        dev_mode: bool,
        api_address: String,
        ghost_source_dir: PathBuf,
        ghost_version: String,
        node_bin: String,
        service_user: String,
        storage: Arc<dyn StorageAllocator>,
        sys: Arc<dyn SysRunner>,
    ) -> Self {
        Self {
            domain,
            dev_mode,
            api_address,
            ghost_source_dir,
            ghost_version,
            node_bin,
            service_user,
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
            source = %self.ghost_source_dir.display(),
            "linking instance against base Ghost install"
        );

        for entry in SHARED_ENTRIES {
            Self::force_symlink(&self.ghost_source_dir.join(entry), &working_dir.join(entry))?;
        }

        let content = working_dir.join("content");
        for dir in CONTENT_DIRS {
            fs::create_dir_all(content.join(dir)).map_err(Error::Io)?;
        }

        // The stock theme is read-only; a user-uploaded theme lands in the
        // instance's own (real) content/themes directory alongside this link.
        Self::force_symlink(
            &self
                .ghost_source_dir
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
            slug = slug,
            user = self.service_user,
            node_bin = self.node_bin,
            working_dir = working_dir.display(),
        )
    }

    fn service_name(slug: &str) -> String {
        format!("goopy-{slug}")
    }

    // ── Production provisioning steps ───────────────────────────────────

    fn write_service_file(&self, slug: &str, working_dir: &Path) -> Result<(), Error> {
        let content = self.render_service_file(slug, working_dir);
        let path = format!("/etc/systemd/system/{}.service", Self::service_name(slug));
        self.sys.sudo_write(&path, &content)
    }

    fn enable_service(&self, slug: &str) -> Result<(), Error> {
        let svc = format!("{}.service", Self::service_name(slug));
        self.sys.sudo_run(&["systemctl", "daemon-reload"])?;
        self.sys.sudo_run(&["systemctl", "enable", &svc])?;
        self.sys.sudo_run(&["systemctl", "start", &svc])
    }

    // ── Production deprovisioning steps ─────────────────────────────────

    fn stop_service(&self, slug: &str) -> Result<(), Error> {
        let svc = format!("{}.service", Self::service_name(slug));

        // `stop`/`disable` tolerate a missing or never-installed unit: a `Failed`
        // instance may hold only partial state, and sweep() reaps those, so a
        // non-zero exit here must not block cleanup and strand the instance.
        // The `rm -f` + `daemon-reload` below is the authoritative removal.
        if let Err(e) = self.sys.sudo_run(&["systemctl", "stop", &svc]) {
            tracing::warn!(error = %e, %svc, "systemctl stop failed (unit may not exist), continuing");
        }
        if let Err(e) = self.sys.sudo_run(&["systemctl", "disable", &svc]) {
            tracing::warn!(error = %e, %svc, "systemctl disable failed (unit may not exist), continuing");
        }

        let path = format!("/etc/systemd/system/{svc}");
        self.sys.sudo_run(&["rm", "-f", &path])?;
        self.sys.sudo_run(&["systemctl", "daemon-reload"])
    }

    // ── Dev-mode helpers ────────────────────────────────────────────────

    fn spawn_dev_server(&self, working_dir: &Path) -> Result<(), Error> {
        info!(working_dir = %working_dir.display(), "spawning dev Ghost");
        let log_path = working_dir.join("ghost.log");
        // NODE_ENV selects config.production.json; dev mode refers to *our*
        // mode, not Ghost's, and there is only ever one config per instance.
        let pid = self.sys.spawn_detached(
            &self.node_bin,
            &["index.js"],
            working_dir,
            &[("NODE_ENV", "production")],
            &log_path,
        )?;
        let pid_path = working_dir.join("server.pid");
        info!(%pid, pid_path = %pid_path.display(), "writing PID file");
        fs::write(&pid_path, pid.to_string()).map_err(Error::Io)
    }

    fn kill_dev_server(&self, working_dir: &Path) -> Result<(), Error> {
        let pid_path = working_dir.join("server.pid");
        let pid_str = match fs::read_to_string(&pid_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!(pid_path = %pid_path.display(), "no PID file found, nothing to kill");
                return Ok(());
            }
            Err(e) => return Err(Error::Io(e)),
        };
        let pid = pid_str.trim();

        if pid.is_empty() {
            return Err(Error::Invalid);
        }

        if !pid.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::Subprocess(format!("invalid PID in file: {pid:?}")));
        }

        info!(%pid, "killing dev Ghost");
        self.sys.kill_pid(pid)?;
        fs::remove_file(&pid_path).map_err(Error::Io)
    }

    // ── Inner provision (post-allocate steps) ───────────────────────────

    fn provision_inner(&self, goopy: &Goopy) -> Result<(), Error> {
        self.materialize_instance_dir(&goopy.working_dir)?;
        self.write_ghost_config(goopy)?;

        if self.dev_mode {
            self.spawn_dev_server(&goopy.working_dir)?;
        } else {
            self.write_service_file(&goopy.slug, &goopy.working_dir)?;
            self.enable_service(&goopy.slug)?;
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
    fn service_version(&self) -> String {
        self.ghost_version.clone()
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

        info!(slug = %goopy.slug, version = %self.ghost_version, "provisioning complete");
        Ok(())
    }

    #[instrument(skip(self), fields(slug = %goopy.slug, dev_mode = self.dev_mode))]
    fn deprovision(&self, goopy: &Goopy) -> Result<(), Error> {
        let result = if self.dev_mode {
            self.kill_dev_server(&goopy.working_dir)
        } else {
            self.stop_service(&goopy.slug)
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
            source.path().to_path_buf(),
            "5.87.1".to_string(),
            "/usr/bin/node".to_string(),
            "goopy".to_string(),
            Arc::new(PlainDirAllocator),
            sys,
        )
    }

    fn sudo_run_args(calls: &[MockCall]) -> Vec<&[String]> {
        calls
            .iter()
            .filter_map(|c| match c {
                MockCall::SudoRun { args } => Some(args.as_slice()),
                _ => None,
            })
            .collect()
    }

    fn sudo_write_paths(calls: &[MockCall]) -> Vec<&str> {
        calls
            .iter()
            .filter_map(|c| match c {
                MockCall::SudoWrite { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect()
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

        let calls = mock.recorded_calls();

        let unit = calls
            .iter()
            .find_map(|c| match c {
                MockCall::SudoWrite { path, content } if path.contains("/etc/systemd/system/") => {
                    Some((path.clone(), content.clone()))
                }
                _ => None,
            })
            .expect("should write a systemd unit");
        assert_eq!(
            unit.0,
            "/etc/systemd/system/goopy-tasty-lucky-clover.service"
        );
        assert!(unit.1.contains("Environment=NODE_ENV=production"));
        assert!(unit.1.contains("User=goopy"), "Ghost must not run as root");
        assert!(unit.1.contains(&format!(
            "ExecStart=/usr/bin/node \"{}/index.js\"",
            working_dir.display()
        )));

        assert!(
            sudo_write_paths(&calls)
                .contains(&"/etc/nginx/sites-available/goopy-tasty-lucky-clover"),
            "should write the nginx site"
        );

        let verb_seq: Vec<&str> = sudo_run_args(&calls)
            .iter()
            .flat_map(|args| args.iter().map(|s| s.as_str()))
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

        let calls = mock.recorded_calls();
        let verb_seq: Vec<&str> = sudo_run_args(&calls)
            .iter()
            .flat_map(|args| args.iter().map(|s| s.as_str()))
            .filter(|a| ["stop", "disable", "daemon-reload", "reload"].contains(a))
            .collect();
        assert_eq!(verb_seq, ["stop", "disable", "daemon-reload", "reload"]);

        let removed: Vec<&String> = sudo_run_args(&calls)
            .iter()
            .flat_map(|args| args.iter())
            .collect();
        assert!(
            removed
                .iter()
                .any(|a| *a == "/etc/systemd/system/goopy-tasty-lucky-clover.service"),
            "should remove the systemd unit"
        );
        assert!(
            removed
                .iter()
                .any(|a| *a == "/etc/nginx/sites-available/goopy-tasty-lucky-clover"),
            "should remove the nginx site"
        );
    }

    /// SysRunner whose `systemctl stop`/`disable` always fail, standing in for a
    /// `Failed` instance whose unit was never installed.
    struct NoUnitSysRunner {
        inner: MockSysRunner,
    }

    impl SysRunner for NoUnitSysRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<(), Error> {
            self.inner.run(program, args)
        }

        fn sudo_run(&self, args: &[&str]) -> Result<(), Error> {
            self.inner.sudo_run(args)?;
            if args.first() == Some(&"systemctl")
                && matches!(args.get(1), Some(&"stop") | Some(&"disable"))
            {
                return Err(Error::Subprocess("Unit not loaded.".to_string()));
            }
            Ok(())
        }

        fn sudo_write(&self, path: &str, content: &str) -> Result<(), Error> {
            self.inner.sudo_write(path, content)
        }

        fn spawn_detached(
            &self,
            program: &str,
            args: &[&str],
            working_dir: &Path,
            envs: &[(&str, &str)],
            log_path: &Path,
        ) -> Result<u32, Error> {
            self.inner
                .spawn_detached(program, args, working_dir, envs, log_path)
        }

        fn kill_pid(&self, pid: &str) -> Result<(), Error> {
            self.inner.kill_pid(pid)
        }
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

        let sys = Arc::new(NoUnitSysRunner {
            inner: MockSysRunner::new(),
        });
        let p = provisioner(false, &source, sys.clone());

        p.deprovision(&test_goopy(&working_dir, 9876))
            .expect("deprovision must succeed even when the unit does not exist");

        let calls = sys.inner.recorded_calls();
        let args: Vec<&String> = sudo_run_args(&calls)
            .iter()
            .flat_map(|a| a.iter())
            .collect();
        assert!(
            args.iter()
                .any(|a| *a == "/etc/systemd/system/goopy-tasty-lucky-clover.service"),
            "unit file must still be removed"
        );
        assert!(
            args.iter()
                .any(|a| *a == "/etc/nginx/sites-available/goopy-tasty-lucky-clover"),
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

        for args in sudo_run_args(&mock.recorded_calls()) {
            for arg in args {
                assert!(
                    !arg.contains(' ') && !arg.contains(';') && !arg.contains('&'),
                    "argument {arg:?} looks like a shell string; each value must be its own arg"
                );
            }
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
