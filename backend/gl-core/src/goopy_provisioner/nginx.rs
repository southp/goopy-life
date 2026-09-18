//! Shared nginx site management for provisioners.
//!
//! Every provisioner fronts its instance with the same reverse-proxy site:
//! `{slug}.{domain}` on 443 (with the shared wildcard cert), an `auth_request`
//! expiry check against gl-serv, and a redirect to `/expired` when that check
//! denies the request. Keeping one template here means a change to the proxy
//! layer (e.g. #89's `auth_request` caching) is made once rather than per
//! provisioner.
//!
//! The expiry check speaks `auth_request`'s vocabulary, which is narrower than
//! it looks: `ngx_http_auth_request_module` forwards **401 and 403 only**,
//! treats any 2xx as "allow", and collapses every other status into a 500. So
//! the gate is 200/403 and the site maps 403 to `@expired`. It used to map 410,
//! which the parent request never sees — an expired instance rendered a 500
//! instead of redirecting, and the template test passed the whole time because
//! it asserted the string rather than the behaviour.
//!
//! # The `goopy_alive` cache zone
//!
//! The expiry check runs once per *request*, not once per page, so a Ghost
//! screen pulling ~45 subresources asks gl-serv 45 times whether the instance
//! is alive. `proxy_cache` collapses that to roughly one question per slug per
//! few seconds.
//!
//! How long "a few seconds" is, is **not decided here**. gl-serv sends
//! `Cache-Control` on each response and nginx honours it: an affirmative answer
//! carries a short `max-age`, a denial carries `no-store`. Keeping the decision
//! at the origin is what makes the cache safe for #96 — a suspended instance's
//! denial is never remembered, so the request always reaches the wake path.
//!
//! The zone itself must be declared in nginx's `http {{}}` block, which this
//! per-site template cannot reach. It ships as `deploy/nginx/goopy-cache.conf`
//! and is a **one-time install**: without it `nginx -t` fails on every site
//! rendered here, so it must be in place before the first instance is
//! provisioned with this template. See docs/GHOST_PROVISIONER.md.
//!
//! ## Two invariants that are invisible in the rendered config
//!
//! Both of these look like omissions and are not. Measured against nginx
//! 1.30.5 with this template verbatim, a 45-subresource page load produces
//! **one** request to gl-serv; changing either invariant breaks that silently,
//! because nothing about the site file reads as wrong afterwards.
//!
//! **There is deliberately no `proxy_cache_valid`.** nginx already honours an
//! upstream `Cache-Control: max-age=N` on its own — `proxy_cache_valid` exists
//! to supply a TTL for responses that do *not* carry one. Adding it would hand
//! a `200` a fallback lifetime for the case where gl-serv stops sending the
//! header, which is exactly the case that must fail closed: freshness is the
//! origin's decision, and that is the whole basis of the #96 wake-safety
//! argument above.
//!
//! **The cache key is per-slug only because `proxy_pass` carries a URI.** When
//! `proxy_pass` has a URI component — ours is `/goopies/{slug}/alive` —
//! nginx's default key uses *that* URI rather than `$request_uri`. This matters
//! because inside an `auth_request` subrequest `$request_uri` is the parent's
//! path (`/assets/foo.css`), so a key built from it would mint a separate entry
//! per subresource and collapse nothing. Shortening the `proxy_pass` to a bare
//! `http://{api_address}` would do precisely that.

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
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_cache goopy_alive;
    }}

    location @expired {{
        return 302 https://goopy.life/expired;
    }}

    location / {{
        auth_request /goopy-alive-check;
        error_page 403 = @expired;
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
            "alive-check location must proxy to the correct gl-serv endpoint — and the \
             URI part is load-bearing beyond routing: nginx's default cache key uses the \
             `proxy_pass` URI when there is one, which is what gives the cache one entry \
             per slug. Drop it and the key falls back to `$request_uri`, which inside an \
             `auth_request` subrequest is the *parent's* path — one entry per subresource, \
             collapsing nothing"
        );
        assert!(
            cfg.contains("error_page 403 = @expired;"),
            "nginx config must map 403 to @expired named location"
        );
        assert!(
            cfg.contains("return 302 https://goopy.life/expired;"),
            "expired location must redirect to /expired page"
        );
        assert!(
            cfg.contains("proxy_cache goopy_alive;"),
            "alive-check location must use the shared cache zone, or every \
             subresource re-asks gl-serv whether the instance is alive"
        );
        assert!(
            !cfg.contains("proxy_cache_valid"),
            "proxy_cache_valid must not appear: nginx already honours the upstream \
             `Cache-Control`, so this directive would only add a fallback TTL for the \
             one case that must fail closed — gl-serv not sending the header at all. \
             Freshness is the origin's decision; that is what keeps a denial uncacheable \
             and the #96 wake path reachable"
        );
        assert!(
            !cfg.contains("error_page 410"),
            "410 must not appear: auth_request never surfaces it to the parent \
             request, so an error_page matching it can never fire"
        );
    }

    /// The `auth_request` subrequest must carry the client's IP.
    ///
    /// `proxy_set_header` does not inherit across locations, so the headers set
    /// in `location /` do not reach this one. Without them the rate limiter's
    /// `SmartIpKeyExtractor` falls back to the peer address — nginx itself on
    /// localhost — and every visitor of every instance on the host shares a
    /// single bucket.
    #[test]
    fn alive_check_subrequest_forwards_the_client_ip() {
        let cfg = render_site("tasty-lucky-clover", "goopy.life", 40123, "127.0.0.1:3000");

        let subrequest = cfg
            .split("location = /goopy-alive-check {")
            .nth(1)
            .and_then(|rest| rest.split("}").next())
            .expect("rendered config must contain the alive-check location");

        assert!(
            subrequest.contains("proxy_set_header X-Real-IP $remote_addr;"),
            "alive-check subrequest must forward X-Real-IP, or the rate limiter \
             buckets every instance's traffic under nginx's own address"
        );
        assert!(
            subrequest.contains("proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;"),
            "alive-check subrequest must forward X-Forwarded-For"
        );
    }
}
