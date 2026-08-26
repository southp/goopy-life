use std::fs;
use std::path::Path;
use std::sync::Arc;

use tracing::{info, instrument};

use super::{GoopyProvisioner, nginx};
use crate::Goopy;
use crate::shared_types::*;
use crate::storage_allocator::StorageAllocator;
use crate::sys_utils::SysRunner;

/// A minimal HTTP provisioner that serves a "Hello, I am {slug}" page.
///
/// Uses a small Python 3 HTTP server: writes a custom `server.py` that listens
/// on the goopy's assigned port.
///
/// In **production mode** (`dev_mode = false`), the provisioner also writes a
/// systemd service unit and an nginx reverse-proxy config.
/// **Prerequisite:** a wildcard TLS certificate for the domain must already
/// exist at `/etc/letsencrypt/live/<domain>/` (e.g. via Certbot + DNS-01
/// challenge — see issue #5 for the one-time setup steps).
///
/// In **dev mode** (`dev_mode = true`), it spawns `python3 server.py` as a
/// detached background process and records the PID for later cleanup.
pub struct HelloProvisioner {
    domain: String,
    dev_mode: bool,
    /// Address on which gl-serv listens; used by nginx `auth_request` subrequests.
    api_address: String,
    storage: Arc<dyn StorageAllocator>,
    sys: Arc<dyn SysRunner>,
}

impl HelloProvisioner {
    pub fn new(
        domain: String,
        dev_mode: bool,
        api_address: String,
        storage: Arc<dyn StorageAllocator>,
        sys: Arc<dyn SysRunner>,
    ) -> Self {
        Self {
            domain,
            dev_mode,
            api_address,
            storage,
            sys,
        }
    }

    // ── Template rendering ──────────────────────────────────────────────

    fn render_server_py(slug: &str, port: u32) -> String {
        format!(
            r#"import http.server, socketserver

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"Hello, I am {slug}"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass

socketserver.TCPServer(("127.0.0.1", {port}), H).serve_forever()
"#,
            slug = slug,
            port = port,
        )
    }

    fn render_service_file(slug: &str, working_dir: &Path) -> String {
        format!(
            r#"[Unit]
Description=Goopy Hello - {slug}
After=network.target

[Service]
ExecStart=/usr/bin/python3 "{working_dir}/server.py"
Restart=on-failure
WorkingDirectory={working_dir}

[Install]
WantedBy=multi-user.target
"#,
            slug = slug,
            working_dir = working_dir.display(),
        )
    }

    fn service_name(slug: &str) -> String {
        format!("goopy-hello-{slug}")
    }

    // ── Production provisioning steps ───────────────────────────────────

    fn write_service_file(&self, slug: &str, working_dir: &Path) -> Result<(), Error> {
        let content = Self::render_service_file(slug, working_dir);
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

        // `stop`/`disable` are tolerant of a missing or never-installed unit:
        // a `Failed` instance may have only partial state, so a non-zero exit
        // here (unit not loaded / not enabled) is expected and non-fatal. The
        // authoritative cleanup is the `rm -f` + `daemon-reload` below.
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
        info!(working_dir = %working_dir.display(), "spawning dev server");
        let log_path = working_dir.join("server.log");
        let pid =
            self.sys
                .spawn_detached("python3", &["server.py"], working_dir, &[], &log_path)?;
        let pid_path = working_dir.join("server.pid");
        info!(%pid, pid_path = %pid_path.display(), "writing PID file");
        std::fs::write(&pid_path, pid.to_string()).map_err(Error::Io)
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

        info!(%pid, "killing dev server");
        self.sys.kill_pid(pid)?;
        std::fs::remove_file(&pid_path).map_err(Error::Io)
    }

    // ── Inner provision (post-allocate steps) ───────────────────────────

    fn provision_inner(&self, goopy: &Goopy) -> Result<(), Error> {
        let server_py = goopy.working_dir.join("server.py");
        info!(path = %server_py.display(), "writing server.py");
        std::fs::write(&server_py, Self::render_server_py(&goopy.slug, goopy.port))
            .map_err(Error::Io)?;

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

impl GoopyProvisioner for HelloProvisioner {
    fn kind(&self) -> ProvisionerKind {
        ProvisionerKind::Hello
    }

    /// The Hello PoC ships with the crate, so its version *is* the crate version.
    fn service_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    #[instrument(skip(self), fields(slug = %goopy.slug, dev_mode = self.dev_mode))]
    fn provision(&self, goopy: &Goopy) -> Result<(), Error> {
        // Step 1: allocate storage
        self.storage.allocate(&goopy.working_dir)?;

        // Step 2: all post-allocate steps; release storage on failure (C1)
        if let Err(e) = self.provision_inner(goopy) {
            let _ = self.storage.release(&goopy.working_dir);
            return Err(e);
        }

        info!(slug = %goopy.slug, "provisioning complete");
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
    use crate::sys_utils::{MockCall, MockSysRunner, RealSysRunner};
    use tempfile::tempdir;

    fn test_goopy(working_dir: &Path, port: u32) -> Goopy {
        Goopy {
            slug: "tasty-lucky-clover".to_string(),
            life_in_days: 7,
            created_at: chrono::Utc::now(),
            working_dir: working_dir.to_path_buf(),
            port,
            status: Status::Spawning,
            provisioner_kind: ProvisionerKind::Hello,
            service_version: "0.1.0".to_string(),
        }
    }

    fn find_free_port() -> u32 {
        // Binds to port 0 to let the OS pick a free port, then releases it.
        // A narrow TOCTOU window exists, but this is the idiomatic approach for tests.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port() as u32
    }

    fn dev_provisioner() -> HelloProvisioner {
        HelloProvisioner::new(
            "localhost".to_string(),
            true,
            "127.0.0.1:3000".to_string(),
            Arc::new(PlainDirAllocator),
            Arc::new(RealSysRunner),
        )
    }

    #[test]
    fn render_server_py_contains_slug_and_port() {
        let py = HelloProvisioner::render_server_py("tasty-lucky-clover", 9876);
        assert!(py.contains("Hello, I am tasty-lucky-clover"));
        assert!(py.contains("9876"));
    }

    #[test]
    fn render_service_file_contains_slug_and_working_dir() {
        let svc = HelloProvisioner::render_service_file(
            "tasty-lucky-clover",
            Path::new("/data/goopies/tasty-lucky-clover"),
        );
        assert!(svc.contains("Goopy Hello - tasty-lucky-clover"));
        assert!(svc.contains("/data/goopies/tasty-lucky-clover/server.py"));
    }

    /// Verifies that `provision` in dev mode writes the server script.
    /// Requires `python3` to be installed.
    #[test]
    #[ignore = "requires python3 to be installed"]
    fn dev_provision_writes_server_py() {
        let base = tempdir().unwrap();
        let working_dir = base.path().join("test-goopy");

        let port = find_free_port();
        let provisioner = dev_provisioner();
        let goopy = test_goopy(&working_dir, port);
        provisioner
            .provision(&goopy)
            .expect("dev provision should succeed");

        let script = working_dir.join("server.py");
        assert!(script.exists(), "server.py should be written");

        let content = fs::read_to_string(&script).unwrap();
        assert!(content.contains("Hello, I am tasty-lucky-clover"));
        assert!(content.contains(&port.to_string()));

        let pid_path = working_dir.join("server.pid");
        assert!(pid_path.exists(), "server.pid should be written");

        // Clean up the spawned process
        let pid = fs::read_to_string(&pid_path).unwrap();
        let _ = std::process::Command::new("kill")
            .args([pid.trim()])
            .output();
    }

    /// Verifies that `deprovision` in dev mode removes the working directory.
    /// Requires `python3` to be installed.
    #[test]
    #[ignore = "requires python3 to be installed"]
    fn dev_deprovision_cleans_up() {
        let base = tempdir().unwrap();
        let working_dir = base.path().join("test-goopy-deprov");

        let port = find_free_port();
        let provisioner = dev_provisioner();
        let goopy = test_goopy(&working_dir, port);
        provisioner
            .provision(&goopy)
            .expect("dev provision should succeed");
        provisioner
            .deprovision(&goopy)
            .expect("dev deprovision should succeed");

        assert!(
            !working_dir.exists(),
            "working dir should be removed after deprovision"
        );
    }

    /// Verifies that `provision` in production mode issues the expected sequence
    /// of privileged system calls — without executing them.
    #[test]
    fn prod_provision_calls_expected_sys_commands() {
        let base = tempdir().unwrap();
        let working_dir = base.path().join("test-goopy-prod");

        let mock_sys = Arc::new(MockSysRunner::new());
        let sys: Arc<dyn SysRunner> = mock_sys.clone();
        let provisioner = HelloProvisioner::new(
            "goopy.life".to_string(),
            false,
            "127.0.0.1:3000".to_string(),
            Arc::new(PlainDirAllocator),
            sys,
        );

        let goopy = test_goopy(&working_dir, 9876);
        provisioner
            .provision(&goopy)
            .expect("prod provision should succeed");

        // server.py is written via std::fs::write directly — check the file on disk.
        let server_py = working_dir.join("server.py");
        assert!(server_py.exists(), "server.py should be written to disk");
        let content = fs::read_to_string(&server_py).unwrap();
        assert!(
            content.contains("tasty-lucky-clover"),
            "server.py should contain slug"
        );
        assert!(content.contains("9876"), "server.py should contain port");

        let calls = mock_sys.recorded_calls();

        let sudo_writes: Vec<&str> = calls
            .iter()
            .filter_map(|c| {
                if let MockCall::SudoWrite { path, .. } = c {
                    Some(path.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            sudo_writes
                .iter()
                .any(|p| p.contains("/etc/systemd/system/")),
            "should write systemd service file"
        );
        assert!(
            sudo_writes
                .iter()
                .any(|p| p.contains("/etc/nginx/sites-available/")),
            "should write nginx config"
        );

        let verb_seq: Vec<&str> = calls
            .iter()
            .filter_map(|c| {
                if let MockCall::SudoRun { args } = c {
                    Some(args)
                } else {
                    None
                }
            })
            .flat_map(|args| args.iter().map(|s| s.as_str()))
            .filter(|a| ["daemon-reload", "enable", "start", "ln", "reload"].contains(a))
            .collect();
        assert_eq!(
            verb_seq,
            ["daemon-reload", "enable", "start", "ln", "reload"]
        );
    }

    /// Verifies that `deprovision` in production mode issues the expected sequence
    /// of privileged system calls — without executing them.
    #[test]
    fn prod_deprovision_calls_expected_sys_commands() {
        let base = tempdir().unwrap();
        let working_dir = base.path().join("test-goopy-prod-deprov");

        let mock_sys = Arc::new(MockSysRunner::new());
        let sys: Arc<dyn SysRunner> = mock_sys.clone();
        let provisioner = HelloProvisioner::new(
            "goopy.life".to_string(),
            false,
            "127.0.0.1:3000".to_string(),
            Arc::new(PlainDirAllocator),
            sys,
        );

        let goopy = test_goopy(&working_dir, 9876);
        provisioner
            .deprovision(&goopy)
            .expect("prod deprovision should succeed");

        let calls = mock_sys.recorded_calls();

        // Verify ordered stop → disable → daemon-reload → reload verb sequence.
        let verb_seq: Vec<&str> = calls
            .iter()
            .filter_map(|c| {
                if let MockCall::SudoRun { args } = c {
                    Some(args)
                } else {
                    None
                }
            })
            .flat_map(|args| args.iter().map(|s| s.as_str()))
            .filter(|a| ["stop", "disable", "daemon-reload", "reload"].contains(a))
            .collect();
        assert_eq!(verb_seq, ["stop", "disable", "daemon-reload", "reload"]);

        // Verify rm was issued for the systemd service file and nginx configs.
        let run_args: Vec<&[String]> = calls
            .iter()
            .filter_map(|c| {
                if let MockCall::SudoRun { args } = c {
                    Some(args.as_slice())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            run_args
                .iter()
                .any(|args| args.iter().any(|a| a.contains("/etc/systemd/system/"))),
            "should remove systemd service file"
        );
        assert!(
            run_args.iter().any(|args| args
                .iter()
                .any(|a| a.contains("/etc/nginx/sites-available/"))),
            "should remove nginx config"
        );
    }
}
