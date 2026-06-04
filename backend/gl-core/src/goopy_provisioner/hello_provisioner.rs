use std::fs;
use std::path::Path;
use std::sync::Arc;

use tracing::{info, instrument};

use super::GoopyProvisioner;
use crate::shared_types::*;
use crate::storage_allocator::StorageAllocator;
use crate::sys_utils::SysRunner;
use crate::Goopy;

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
    pub(crate) domain: String,
    pub(crate) dev_mode: bool,
    storage: Arc<dyn StorageAllocator>,
    sys: Arc<dyn SysRunner>,
}

impl HelloProvisioner {
    pub fn new(
        domain: String,
        dev_mode: bool,
        storage: Arc<dyn StorageAllocator>,
        sys: Arc<dyn SysRunner>,
    ) -> Self {
        Self {
            domain,
            dev_mode,
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

    fn render_nginx_config(slug: &str, domain: &str, port: u32) -> String {
        format!(
            r#"server {{
    listen 80;
    server_name {slug}.{domain};
    return 301 https://$host$request_uri;
}}

server {{
    listen 443 ssl;
    server_name {slug}.{domain};

    ssl_certificate     /etc/letsencrypt/live/{domain}/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/{domain}/privkey.pem;

    location / {{
        proxy_pass http://127.0.0.1:{port};
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }}
}}
"#,
            slug = slug,
            domain = domain,
            port = port,
        )
    }

    fn service_name(slug: &str) -> String {
        format!("goopy-hello-{slug}")
    }

    // ── Helpers: file writing ───────────────────────────────────────────

    fn write_server_script(working_dir: &Path, slug: &str, port: u32) -> Result<(), Error> {
        let content = Self::render_server_py(slug, port);
        let path = working_dir.join("server.py");
        info!(path = %path.display(), "writing server.py");
        fs::write(&path, content).map_err(Error::Io)
    }

    // ── Production provisioning steps ───────────────────────────────────

    #[instrument(skip(self, working_dir), fields(slug))]
    fn write_service_file(&self, slug: &str, working_dir: &Path) -> Result<(), Error> {
        let content = Self::render_service_file(slug, working_dir);
        let path = format!("/etc/systemd/system/{}.service", Self::service_name(slug));
        self.sys.sudo_write(&path, &content)
    }

    #[instrument(skip(self), fields(slug))]
    fn enable_service(&self, slug: &str) -> Result<(), Error> {
        let svc = format!("{}.service", Self::service_name(slug));
        self.sys.run("sudo", &["systemctl", "daemon-reload"])?;
        self.sys.run("sudo", &["systemctl", "enable", &svc])?;
        self.sys.run("sudo", &["systemctl", "start", &svc])
    }

    #[instrument(skip(self), fields(slug))]
    fn write_nginx_config(&self, slug: &str, domain: &str, port: u32) -> Result<(), Error> {
        let content = Self::render_nginx_config(slug, domain, port);
        let path = format!("/etc/nginx/sites-available/{slug}");
        self.sys.sudo_write(&path, &content)
    }

    #[instrument(skip(self), fields(slug))]
    fn enable_nginx_site(&self, slug: &str) -> Result<(), Error> {
        let available = format!("/etc/nginx/sites-available/{slug}");
        let enabled = format!("/etc/nginx/sites-enabled/{slug}");
        self.sys.run("sudo", &["ln", "-sf", &available, &enabled])
    }

    #[instrument(skip(self))]
    fn reload_nginx(&self) -> Result<(), Error> {
        self.sys.run("sudo", &["nginx", "-t"])?;
        self.sys.run("sudo", &["systemctl", "reload", "nginx"])
    }

    // ── Production deprovisioning steps ─────────────────────────────────

    #[instrument(skip(self), fields(slug))]
    fn stop_service(&self, slug: &str) -> Result<(), Error> {
        let svc = format!("{}.service", Self::service_name(slug));
        self.sys.run("sudo", &["systemctl", "stop", &svc])?;
        self.sys.run("sudo", &["systemctl", "disable", &svc])?;

        let path = format!("/etc/systemd/system/{svc}");
        self.sys.run("sudo", &["rm", "-f", &path])?;
        self.sys.run("sudo", &["systemctl", "daemon-reload"])
    }

    #[instrument(skip(self), fields(slug))]
    fn remove_nginx_site(&self, slug: &str) -> Result<(), Error> {
        let enabled = format!("/etc/nginx/sites-enabled/{slug}");
        let available = format!("/etc/nginx/sites-available/{slug}");
        self.sys.run("sudo", &["rm", "-f", &enabled])?;
        self.sys.run("sudo", &["rm", "-f", &available])?;
        self.reload_nginx()
    }

    // ── Dev-mode helpers ────────────────────────────────────────────────

    fn spawn_dev_server(&self, working_dir: &Path) -> Result<(), Error> {
        info!(working_dir = %working_dir.display(), "spawning dev server");
        let log_path = working_dir.join("server.log");
        let pid = self.sys.spawn_detached("python3", &["server.py"], working_dir, &log_path)?;
        let pid_path = working_dir.join("server.pid");
        info!(%pid, pid_path = %pid_path.display(), "writing PID file");
        self.sys.write(&pid_path, &pid.to_string())
    }

    fn kill_dev_server(&self, working_dir: &Path) -> Result<(), Error> {
        let pid_path = working_dir.join("server.pid");
        if !pid_path.exists() {
            info!(pid_path = %pid_path.display(), "no PID file found, nothing to kill");
            return Ok(());
        }

        let pid_str = fs::read_to_string(&pid_path).map_err(Error::Io)?;
        let pid = pid_str.trim();

        if pid.is_empty() {
            return Err(Error::Other("PID file is empty".to_string()));
        }

        if !pid.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::Other(format!("invalid PID in file: {pid:?}")));
        }

        info!(%pid, "killing dev server");
        self.sys.kill_pid(pid)?;
        fs::remove_file(&pid_path).map_err(Error::Io)?;
        Ok(())
    }

    // ── Inner provision (post-allocate steps) ───────────────────────────

    fn provision_inner(&self, goopy: &Goopy) -> Result<(), Error> {
        Self::write_server_script(&goopy.working_dir, &goopy.slug, goopy.port)?;

        if self.dev_mode {
            self.spawn_dev_server(&goopy.working_dir)?;
        } else {
            self.write_service_file(&goopy.slug, &goopy.working_dir)?;
            self.enable_service(&goopy.slug)?;
            self.write_nginx_config(&goopy.slug, &self.domain, goopy.port)?;
            self.enable_nginx_site(&goopy.slug)?;
            self.reload_nginx()?;
        }
        Ok(())
    }
}

impl GoopyProvisioner for HelloProvisioner {
    fn kind(&self) -> ProvisionerKind {
        ProvisionerKind::Hello
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
                .and_then(|_| self.remove_nginx_site(&goopy.slug))
        };
        let _ = self.storage.release(&goopy.working_dir);
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

    fn test_goopy(working_dir: &Path) -> Goopy {
        Goopy::new(
            "tasty-lucky-clover".to_string(),
            7,
            chrono::Utc::now(),
            working_dir,
            9876,
            Status::Spawning,
            ProvisionerKind::Hello,
            "0.1.0".to_string(),
        )
        .unwrap()
    }

    fn dev_provisioner() -> HelloProvisioner {
        HelloProvisioner::new(
            "localhost".to_string(),
            true,
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

    #[test]
    fn render_nginx_config_contains_slug_domain_port() {
        let cfg =
            HelloProvisioner::render_nginx_config("tasty-lucky-clover", "goopy.life", 9876);
        assert!(cfg.contains("tasty-lucky-clover.goopy.life"));
        assert!(cfg.contains("proxy_pass http://127.0.0.1:9876"));
        assert!(cfg.contains("/etc/letsencrypt/live/goopy.life/"));
    }

    /// Verifies that `provision` in dev mode writes the server script.
    /// Requires `python3` to be installed.
    #[test]
    #[ignore = "requires python3 to be installed"]
    fn dev_provision_writes_server_py() {
        let base = tempdir().unwrap();
        let working_dir = base.path().join("test-goopy");

        let provisioner = dev_provisioner();
        let goopy = test_goopy(&working_dir);
        provisioner.provision(&goopy).expect("dev provision should succeed");

        let script = working_dir.join("server.py");
        assert!(script.exists(), "server.py should be written");

        let content = fs::read_to_string(&script).unwrap();
        assert!(content.contains("Hello, I am tasty-lucky-clover"));
        assert!(content.contains("9876"));

        let pid_path = working_dir.join("server.pid");
        assert!(pid_path.exists(), "server.pid should be written");

        // Clean up the spawned process
        let pid = fs::read_to_string(&pid_path).unwrap();
        let _ = std::process::Command::new("kill").args([pid.trim()]).output();
    }

    /// Verifies that `deprovision` in dev mode removes the working directory.
    /// Requires `python3` to be installed.
    #[test]
    #[ignore = "requires python3 to be installed"]
    fn dev_deprovision_cleans_up() {
        let base = tempdir().unwrap();
        let working_dir = base.path().join("test-goopy-deprov");

        let provisioner = dev_provisioner();
        let goopy = test_goopy(&working_dir);
        provisioner.provision(&goopy).expect("dev provision should succeed");
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
            Arc::new(PlainDirAllocator),
            sys,
        );

        let goopy = test_goopy(&working_dir);
        provisioner.provision(&goopy).expect("prod provision should succeed");

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
            sudo_writes.iter().any(|p| p.contains("/etc/systemd/system/")),
            "should write systemd service file"
        );
        assert!(
            sudo_writes.iter().any(|p| p.contains("/etc/nginx/sites-available/")),
            "should write nginx config"
        );

        let runs: Vec<(&str, &[String])> = calls
            .iter()
            .filter_map(|c| {
                if let MockCall::Run { program, args } = c {
                    Some((program.as_str(), args.as_slice()))
                } else {
                    None
                }
            })
            .collect();

        assert!(
            runs.iter().any(|(_, args)| args.iter().any(|a| a == "daemon-reload")),
            "should daemon-reload systemd"
        );
        assert!(
            runs.iter().any(|(_, args)| args.iter().any(|a| a == "enable")),
            "should enable service"
        );
        assert!(
            runs.iter().any(|(_, args)| args.iter().any(|a| a == "start")),
            "should start service"
        );
        assert!(
            runs.iter().any(|(_, args)| args.iter().any(|a| a == "ln")),
            "should create nginx symlink"
        );
        assert!(
            runs.iter().any(|(_, args)| args.iter().any(|a| a == "-t")),
            "should test nginx config"
        );
        assert!(
            runs.iter()
                .any(|(_, args)| args.iter().any(|a| a == "reload") && args.iter().any(|a| a == "nginx")),
            "should reload nginx"
        );
    }
}
