use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use tracing::{error, info, instrument};

use super::GoopyProvisioner;
use crate::shared_types::*;
use crate::storage_allocator::StorageAllocator;
use crate::Goopy;

/// A minimal HTTP provisioner that serves a "Hello, I am {slug}" page.
///
/// Uses `python3 -m http.server`-style approach: writes a custom `server.py`
/// that listens on the goopy's assigned port.
///
/// In **production mode** (`dev_mode = false`), the provisioner also writes a
/// systemd service unit and an nginx reverse-proxy config.
///
/// In **dev mode** (`dev_mode = true`), it spawns `python3 server.py` as a
/// background process and records the PID for later cleanup.
pub struct HelloProvisioner {
    pub domain: String,
    pub dev_mode: bool,
    storage: Box<dyn StorageAllocator>,
}

impl HelloProvisioner {
    pub fn new(domain: String, dev_mode: bool, storage: Box<dyn StorageAllocator>) -> Self {
        Self {
            domain,
            dev_mode,
            storage,
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
ExecStart=/usr/bin/python3 {working_dir}/server.py
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

    /// Write a file to a privileged path via `sudo tee`.
    fn sudo_write(path: &str, content: &str) -> Result<(), Error> {
        info!(path, "writing privileged file via sudo tee");
        let mut child = Command::new("sudo")
            .args(["tee", path])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .map_err(|e| Error::Other(format!("failed to spawn sudo tee: {e}")))?;

        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(content.as_bytes())
            .map_err(|e| Error::Other(format!("failed to write to sudo tee stdin: {e}")))?;

        let status = child
            .wait()
            .map_err(|e| Error::Other(format!("sudo tee wait failed: {e}")))?;

        if !status.success() {
            return Err(Error::Other(format!(
                "sudo tee {path} exited with status {status}"
            )));
        }
        Ok(())
    }

    /// Run a command, returning `Ok(())` on success or an `Error::Other` with
    /// stderr on failure.
    fn run(program: &str, args: &[&str]) -> Result<(), Error> {
        info!(program, ?args, "running command");
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| Error::Other(format!("failed to run {program}: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!(program, %stderr, "command failed");
            return Err(Error::Other(format!(
                "{program} failed (exit {}): {}",
                output.status,
                stderr.trim()
            )));
        }
        Ok(())
    }

    // ── Production provisioning steps ───────────────────────────────────

    fn write_service_file(slug: &str, working_dir: &Path) -> Result<(), Error> {
        let content = Self::render_service_file(slug, working_dir);
        let path = format!("/etc/systemd/system/{}.service", Self::service_name(slug));
        Self::sudo_write(&path, &content)
    }

    fn enable_service(slug: &str) -> Result<(), Error> {
        let svc = format!("{}.service", Self::service_name(slug));
        Self::run("sudo", &["systemctl", "daemon-reload"])?;
        Self::run("sudo", &["systemctl", "enable", &svc])?;
        Self::run("sudo", &["systemctl", "start", &svc])
    }

    fn write_nginx_config(slug: &str, domain: &str, port: u32) -> Result<(), Error> {
        let content = Self::render_nginx_config(slug, domain, port);
        let path = format!("/etc/nginx/sites-available/{slug}");
        Self::sudo_write(&path, &content)
    }

    fn enable_nginx_site(slug: &str) -> Result<(), Error> {
        let available = format!("/etc/nginx/sites-available/{slug}");
        let enabled = format!("/etc/nginx/sites-enabled/{slug}");
        Self::run("sudo", &["ln", "-sf", &available, &enabled])
    }

    fn reload_nginx() -> Result<(), Error> {
        Self::run("sudo", &["nginx", "-t"])?;
        Self::run("sudo", &["systemctl", "reload", "nginx"])
    }

    // ── Production deprovisioning steps ─────────────────────────────────

    fn stop_service(slug: &str) -> Result<(), Error> {
        let svc = format!("{}.service", Self::service_name(slug));
        Self::run("sudo", &["systemctl", "stop", &svc])?;
        Self::run("sudo", &["systemctl", "disable", &svc])?;

        let path = format!("/etc/systemd/system/{svc}");
        Self::run("sudo", &["rm", "-f", &path])?;
        Self::run("sudo", &["systemctl", "daemon-reload"])
    }

    fn remove_nginx_site(slug: &str) -> Result<(), Error> {
        let enabled = format!("/etc/nginx/sites-enabled/{slug}");
        let available = format!("/etc/nginx/sites-available/{slug}");
        Self::run("sudo", &["rm", "-f", &enabled])?;
        Self::run("sudo", &["rm", "-f", &available])?;
        Self::reload_nginx()
    }

    // ── Dev-mode helpers ────────────────────────────────────────────────

    fn spawn_dev_server(working_dir: &Path) -> Result<(), Error> {
        let script = working_dir.join("server.py");
        info!(script = %script.display(), "spawning dev server");

        let child = Command::new("python3")
            .args([script.to_string_lossy().as_ref()])
            .current_dir(working_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| Error::Other(format!("failed to spawn python3: {e}")))?;

        let pid = child.id();
        let pid_path = working_dir.join("server.pid");
        info!(%pid, pid_path = %pid_path.display(), "writing PID file");
        fs::write(&pid_path, pid.to_string()).map_err(Error::Io)?;
        Ok(())
    }

    fn kill_dev_server(working_dir: &Path) -> Result<(), Error> {
        let pid_path = working_dir.join("server.pid");
        if !pid_path.exists() {
            info!(pid_path = %pid_path.display(), "no PID file found, nothing to kill");
            return Ok(());
        }

        let pid_str = fs::read_to_string(&pid_path).map_err(Error::Io)?;
        let pid = pid_str.trim();
        info!(%pid, "killing dev server");

        // kill may fail if the process already exited — that is fine.
        let _ = Command::new("kill").args([pid]).output();
        let _ = fs::remove_file(&pid_path);
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

        // Step 2: write the Python server script
        Self::write_server_script(&goopy.working_dir, &goopy.slug, goopy.port)?;

        if self.dev_mode {
            // Dev mode: just spawn the process and record PID
            Self::spawn_dev_server(&goopy.working_dir)?;
        } else {
            // Production mode: systemd + nginx
            Self::write_service_file(&goopy.slug, &goopy.working_dir)?;
            Self::enable_service(&goopy.slug)?;
            Self::write_nginx_config(&goopy.slug, &self.domain, goopy.port)?;
            Self::enable_nginx_site(&goopy.slug)?;
            Self::reload_nginx()?;
        }

        info!(slug = %goopy.slug, "provisioning complete");
        Ok(())
    }

    #[instrument(skip(self), fields(slug = %goopy.slug, dev_mode = self.dev_mode))]
    fn deprovision(&self, goopy: &Goopy) -> Result<(), Error> {
        if self.dev_mode {
            Self::kill_dev_server(&goopy.working_dir)?;
        } else {
            // Production: tear down in reverse order
            Self::stop_service(&goopy.slug)?;
            Self::remove_nginx_site(&goopy.slug)?;
        }

        // Release storage last
        self.storage.release(&goopy.working_dir)?;

        info!(slug = %goopy.slug, "deprovisioning complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_allocator::PlainDirAllocator;
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

    #[test]
    fn render_server_py_contains_slug_and_port() {
        let py = HelloProvisioner::render_server_py("tasty-lucky-clover", 9876);
        assert!(py.contains("Hello, I am tasty-lucky-clover"));
        assert!(py.contains("9876"));
    }

    #[test]
    fn render_service_file_contains_slug_and_working_dir() {
        let svc =
            HelloProvisioner::render_service_file("tasty-lucky-clover", Path::new("/data/goopies/tasty-lucky-clover"));
        assert!(svc.contains("Goopy Hello - tasty-lucky-clover"));
        assert!(svc.contains("/data/goopies/tasty-lucky-clover/server.py"));
    }

    #[test]
    fn render_nginx_config_contains_slug_domain_port() {
        let cfg = HelloProvisioner::render_nginx_config("tasty-lucky-clover", "goopy.life", 9876);
        assert!(cfg.contains("tasty-lucky-clover.goopy.life"));
        assert!(cfg.contains("proxy_pass http://127.0.0.1:9876"));
        assert!(cfg.contains("/etc/letsencrypt/live/goopy.life/"));
    }

    #[test]
    fn dev_provision_writes_server_py() {
        let base = tempdir().unwrap();
        let working_dir = base.path().join("test-goopy");

        let provisioner = HelloProvisioner::new(
            "localhost".to_string(),
            true,
            Box::new(PlainDirAllocator),
        );

        let goopy = test_goopy(&working_dir);
        provisioner.provision(&goopy).expect("dev provision should succeed");

        let script = working_dir.join("server.py");
        assert!(script.exists(), "server.py should be written");

        let content = fs::read_to_string(&script).unwrap();
        assert!(content.contains("Hello, I am tasty-lucky-clover"));
        assert!(content.contains("9876"));

        // PID file should also exist
        let pid_path = working_dir.join("server.pid");
        assert!(pid_path.exists(), "server.pid should be written");

        // Clean up: kill the spawned process
        let pid = fs::read_to_string(&pid_path).unwrap();
        let _ = Command::new("kill").args([pid.trim()]).output();
    }

    #[test]
    fn dev_deprovision_cleans_up() {
        let base = tempdir().unwrap();
        let working_dir = base.path().join("test-goopy-deprov");

        let provisioner = HelloProvisioner::new(
            "localhost".to_string(),
            true,
            Box::new(PlainDirAllocator),
        );

        let goopy = test_goopy(&working_dir);
        provisioner.provision(&goopy).expect("dev provision should succeed");
        provisioner.deprovision(&goopy).expect("dev deprovision should succeed");

        // Working dir should be gone (PlainDirAllocator removes it)
        assert!(!working_dir.exists(), "working dir should be removed after deprovision");
    }
}
