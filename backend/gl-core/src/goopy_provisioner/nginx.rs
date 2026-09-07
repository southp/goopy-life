//! Shared nginx site management for provisioners.
//!
//! Every provisioner fronts its instance with the same reverse-proxy site:
//! `{slug}.{domain}` on 443 (with the shared wildcard cert), an `auth_request`
//! expiry check against gl-serv, and a redirect to `/expired` when that check
//! returns 410. Keeping one template here means a change to the proxy layer
//! (e.g. #89's `auth_request` caching) is made once rather than per provisioner.

use crate::shared_types::Error;
use crate::sys_utils::SysRunner;

/// Renders the nginx site for one instance.
///
/// `api_address` is where gl-serv listens; the `auth_request` subrequest is
/// proxied there to ask whether the instance is still alive.
fn render_site(slug: &str, domain: &str, port: u32, api_address: &str) -> String {
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

    location = /goopy-alive-check {{
        internal;
        proxy_pass http://{api_address}/goopies/{slug}/alive;
        proxy_pass_request_body off;
        proxy_set_header Content-Length "";
    }}

    location @expired {{
        return 302 https://goopy.life/expired;
    }}

    location / {{
        auth_request /goopy-alive-check;
        error_page 410 = @expired;
        proxy_pass http://127.0.0.1:{port};
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }}
}}
"#,
    )
}

fn available_path(slug: &str) -> String {
    format!("/etc/nginx/sites-available/goopy-{slug}")
}

fn enabled_path(slug: &str) -> String {
    format!("/etc/nginx/sites-enabled/goopy-{slug}")
}

/// Writes the site config, symlinks it into `sites-enabled`, and reloads nginx.
pub(crate) fn install_site(
    sys: &dyn SysRunner,
    slug: &str,
    domain: &str,
    port: u32,
    api_address: &str,
) -> Result<(), Error> {
    let content = render_site(slug, domain, port, api_address);
    let available = available_path(slug);
    sys.sudo_write(&available, &content)?;
    sys.sudo_run(&["ln", "-sf", &available, &enabled_path(slug)])?;
    reload(sys)
}

/// Removes both the symlink and the site config, then reloads nginx.
pub(crate) fn remove_site(sys: &dyn SysRunner, slug: &str) -> Result<(), Error> {
    sys.sudo_run(&["rm", "-f", &enabled_path(slug)])?;
    sys.sudo_run(&["rm", "-f", &available_path(slug)])?;
    reload(sys)
}

/// Validates the nginx config and reloads the running server.
fn reload(sys: &dyn SysRunner) -> Result<(), Error> {
    sys.sudo_run(&["nginx", "-t"])?;
    sys.sudo_run(&["systemctl", "reload", "nginx"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys_utils::MockSysRunner;

    /// The symlink must be in place before `nginx -t`, and `nginx -t` must pass
    /// before the reload — reloading first would push an unvalidated config to
    /// the running server. The whole sequence is pinned rather than filtered so
    /// that a reordering of [`reload`] fails here.
    #[test]
    fn install_site_validates_before_reloading() {
        let sys = MockSysRunner::new();
        install_site(
            &sys,
            "tasty-lucky-clover",
            "goopy.life",
            9876,
            "127.0.0.1:3000",
        )
        .unwrap();

        assert_eq!(
            sys.sudo_write_paths(),
            ["/etc/nginx/sites-available/goopy-tasty-lucky-clover"]
        );
        assert_eq!(
            sys.sudo_run_args(),
            [
                "ln",
                "-sf",
                "/etc/nginx/sites-available/goopy-tasty-lucky-clover",
                "/etc/nginx/sites-enabled/goopy-tasty-lucky-clover",
                "nginx",
                "-t",
                "systemctl",
                "reload",
                "nginx",
            ]
        );
    }

    /// The symlink goes first: removing the config while `sites-enabled` still
    /// points at it would leave a dangling link that fails the next `nginx -t`
    /// for every other instance.
    #[test]
    fn remove_site_unlinks_before_removing_the_config() {
        let sys = MockSysRunner::new();
        remove_site(&sys, "tasty-lucky-clover").unwrap();

        assert!(
            sys.sudo_write_paths().is_empty(),
            "removal must not write anything"
        );
        assert_eq!(
            sys.sudo_run_args(),
            [
                "rm",
                "-f",
                "/etc/nginx/sites-enabled/goopy-tasty-lucky-clover",
                "rm",
                "-f",
                "/etc/nginx/sites-available/goopy-tasty-lucky-clover",
                "nginx",
                "-t",
                "systemctl",
                "reload",
                "nginx",
            ]
        );
    }

    #[test]
    fn render_site_contains_slug_domain_port() {
        let cfg = render_site("tasty-lucky-clover", "goopy.life", 9876, "127.0.0.1:3000");
        assert!(cfg.contains("tasty-lucky-clover.goopy.life"));
        assert!(cfg.contains("proxy_pass http://127.0.0.1:9876"));
        assert!(cfg.contains("/etc/letsencrypt/live/goopy.life/"));
    }

    #[test]
    fn render_site_contains_auth_request_directives() {
        let cfg = render_site("tasty-lucky-clover", "goopy.life", 9876, "127.0.0.1:3000");
        assert!(
            cfg.contains("auth_request /goopy-alive-check;"),
            "nginx config must include auth_request directive"
        );
        assert!(
            cfg.contains("proxy_pass http://127.0.0.1:3000/goopies/tasty-lucky-clover/alive;"),
            "alive-check location must proxy to the correct gl-serv endpoint"
        );
        assert!(
            cfg.contains("error_page 410 = @expired;"),
            "nginx config must map 410 to @expired named location"
        );
        assert!(
            cfg.contains("return 302 https://goopy.life/expired;"),
            "expired location must redirect to /expired page"
        );
    }
}
