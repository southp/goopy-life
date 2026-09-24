use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use clap::{CommandFactory, FromArgMatches, Parser};
use gl_core::config::ProvisionerConfig;
use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
use gl_core::{AllocatorKind, CapacityKind, GoopyManager, RealSysRunner};
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "gl-serv")]
#[command(about = "Goopy.Life API server")]
struct Cli {
    /// Path to the config file
    #[arg(long, default_value = "/opt/goopy-life/config.toml")]
    config: std::path::PathBuf,

    /// Parse --config, print a summary of it, and exit without starting the
    /// server. Exits 0 when the file is one this binary can run with, non-zero
    /// with the parse error otherwise.
    ///
    /// Used by deploy/push-binary.sh to gate a deploy on the config it is about
    /// to install, while the previous binary is still serving.
    #[arg(long)]
    check_config: bool,
}

/// Parse `path`, check every value this binary would otherwise only discover
/// at startup, and render a summary of what it read.
///
/// Deliberately stops short of opening a registry, binding a port or starting a
/// background task, so it is safe to run next to a live gl-serv. Running the
/// binary bare to test a config instead collides with the running service on
/// `Address in use`.
///
/// Most of the validation lives in [`gl_core::Config::from_file`], so that
/// gl-core's `tests/committed_configs.rs` catches a bad committed config at
/// review time too. `cors_origin` is the exception and is checked here: it is
/// only ever used as a [`HeaderValue`], and gl-core has no dependency on
/// `http`. Host facts are checked separately again — see [`check_host_paths`].
///
/// The summary names the values that decide what the process becomes — the two
/// kinds it will build, and the paths and address it will claim — so an
/// operator can see that the file parsed *and* that it is the environment they
/// meant to deploy.
fn check_config(path: &std::path::Path) -> Result<(gl_core::Config, String), gl_core::Error> {
    let cfg = gl_core::Config::from_file(path)?;

    // Unchecked this is an `exit(1)` while building the router, which a deploy
    // only reaches once it has swapped this config in and restarted the unit.
    cfg.cors_origin.parse::<HeaderValue>().map_err(|e| {
        gl_core::Error::Config(format!(
            "cors_origin {:?} is not usable as a header value: {e}",
            cfg.cors_origin
        ))
    })?;

    // Ghost's paths are the difference between a config that starts and one
    // that starts and then fails every spawn, so the summary names them rather
    // than just the kind.
    let provisioner = match &cfg.provisioner {
        ProvisionerConfig::Hello => "Hello".to_string(),
        ProvisionerConfig::Ghost(ghost) => format!(
            "Ghost {} (source_dir {}, node_bin {})",
            ghost.version,
            ghost.source_dir.display(),
            ghost.node_bin,
        ),
    };

    // Same treatment for the allocator, and for the same reason: `Zfs` alone
    // does not say which pool the instances will land in. `pool` and
    // `quota_mb` are ignored under `PlainDir`, so printing them there would be
    // the opposite error.
    let allocator = match cfg.allocator.kind {
        AllocatorKind::PlainDir => "PlainDir".to_string(),
        AllocatorKind::Zfs => format!(
            "Zfs (pool {}, quota {} MB)",
            cfg.allocator.pool, cfg.allocator.quota_mb,
        ),
    };

    // Built from pairs rather than one padded format string so the column
    // stays aligned when a field is added: the longest key here is already
    // wider than the block a hand-counted layout would have assumed.
    let fields = [
        ("domain", cfg.domain.clone()),
        ("bind_address", cfg.bind_address.clone()),
        // Printed next to the listen address because the two are easy to
        // conflate and used to be the same field: this is what nginx will be
        // told to connect to in every instance's alive-check (#149).
        ("api_address", cfg.resolved_api_address()),
        ("cors_origin", cfg.cors_origin.clone()),
        ("dev_mode", cfg.dev_mode.to_string()),
        ("provisioner", provisioner),
        ("allocator", allocator),
        ("registry", cfg.registry.path.display().to_string()),
        ("base_dir", cfg.base_dir.display().to_string()),
        ("life_in_days", cfg.life_in_days.to_string()),
        ("sweep_interval_secs", cfg.sweep_interval_secs.to_string()),
        ("event_retention_days", cfg.event_retention_days.to_string()),
        ("max_active", cfg.max_active.to_string()),
        ("max_provisioned", cfg.max_provisioned.to_string()),
    ];
    let width = fields.iter().map(|(key, _)| key.len()).max().unwrap_or(0);

    let mut summary = format!("{} is valid", path.display());
    for (key, value) in fields {
        summary.push_str(&format!("\n  {key:<width$}  {value}"));
    }
    Ok((cfg, summary))
}

/// Verify that the paths a configured provisioner will reach for exist on this
/// host.
///
/// Split from [`check_config`] because these are *host* facts: they exist on
/// the droplet and nowhere else, so neither CI nor gl-core's committed-config
/// test can assert them. `--check-config` is the only step of a deploy that
/// runs on the target host, which makes it the one place the check is possible
/// at all.
///
/// Left unchecked, a typo here parses, starts, and reports `systemctl
/// is-active` green — and then fails every `POST /goopies` at provision time.
///
/// `service_user` is deliberately not checked: verifying it needs an NSS
/// lookup, and unlike these two it fails loudly at provision time rather than
/// silently.
fn check_host_paths(cfg: &gl_core::Config) -> Result<(), gl_core::Error> {
    let ProvisionerConfig::Ghost(ghost) = &cfg.provisioner else {
        return Ok(());
    };

    // Collected rather than returned one at a time: an operator fixing a
    // prepared install wants both problems in one deploy attempt.
    let mut problems: Vec<String> = Vec::new();

    if !ghost.source_dir.is_dir() {
        problems.push(format!(
            "provisioner.source_dir {} is not a directory",
            ghost.source_dir.display()
        ));
    } else {
        let missing: Vec<&str> = gl_core::goopy_provisioner::ghost_provisioner::SHARED_ENTRIES
            .iter()
            .copied()
            .filter(|entry| !ghost.source_dir.join(entry).exists())
            .collect();
        if !missing.is_empty() {
            problems.push(format!(
                "provisioner.source_dir {} is missing {} — it is not a prepared \
                 Ghost install",
                ghost.source_dir.display(),
                missing.join(", "),
            ));
        }
    }

    if !is_executable(std::path::Path::new(&ghost.node_bin)) {
        problems.push(format!(
            "provisioner.node_bin {} is not an executable file",
            ghost.node_bin
        ));
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(gl_core::Error::Config(problems.join("\n")))
    }
}

/// Whether `path` is a file with an execute bit set.
///
/// Any execute bit, not specifically the calling user's: the per-instance unit
/// runs node as `service_user`, not as the deploy account running this check,
/// so asking "can *I* execute it" would be the wrong question.
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Manager abstraction (enables test injection)
// ---------------------------------------------------------------------------

trait ManagerService: Send + Sync {
    fn spawn(&self) -> Result<String, gl_core::Error>;
    fn get(&self, slug: &str) -> Result<Option<gl_core::Goopy>, gl_core::Error>;
    fn sweep(&self) -> Result<(u32, Vec<gl_core::Error>), gl_core::Error>;
    fn capacity(&self) -> Result<gl_core::Capacity, gl_core::Error>;
}

impl<R, P> ManagerService for GoopyManager<R, P>
where
    R: gl_core::goopy_registry::GoopyRegistry + Send + Sync + 'static,
    P: gl_core::goopy_provisioner::GoopyProvisioner + Send + Sync + 'static,
{
    fn spawn(&self) -> Result<String, gl_core::Error> {
        GoopyManager::spawn(self).map(|(slug, _)| slug)
    }

    fn get(&self, slug: &str) -> Result<Option<gl_core::Goopy>, gl_core::Error> {
        GoopyManager::get(self, slug)
    }

    fn sweep(&self) -> Result<(u32, Vec<gl_core::Error>), gl_core::Error> {
        GoopyManager::sweep(self)
    }

    fn capacity(&self) -> Result<gl_core::Capacity, gl_core::Error> {
        GoopyManager::capacity(self)
    }
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct AppState {
    manager: Arc<dyn ManagerService>,
    cfg: gl_core::Config,
}

// ---------------------------------------------------------------------------
// JSON response types
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct SpawnResponse {
    slug: String,
    status: String,
}

#[derive(serde::Serialize)]
struct GoopyResponse {
    slug: String,
    status: String,
    url: String,
    created_at: String,
    expires_at: String,
    is_expired: bool,
}

#[derive(serde::Serialize)]
struct ConfigResponse {
    life_in_days: i32,
    storage_quota_mb: u64,
    domain: String,
}

/// Body of `GET /version` — which commit this process was built from.
///
/// Deliberately separate from [`ConfigResponse`], for the same reason
/// [`CapacityResponse`] is and then some: the frontend fetches `/config` once
/// at Vercel build time (`force-static`), and `frontend/vercel.json`'s
/// `ignoreCommand` skips the Vercel build entirely for backend-only changes. A
/// backend sha routed through `/config` would therefore freeze at whatever it
/// was the last time the *frontend* happened to build, and go on being served
/// as fact through any number of backend deploys. A version display that is
/// confidently wrong is worse than none, so this is a runtime fetch.
///
/// Public on purpose: the repo is public, so the commit id discloses nothing
/// that is not already readable, and the browser has no other way to be told.
#[derive(serde::Serialize)]
struct VersionResponse {
    /// The abbreviated commit id, for display.
    sha: String,
    /// The full commit id, for linking to the commit — and what
    /// `deploy/push-binary.sh` matches against what it just built.
    sha_full: &'static str,
    built_at: &'static str,
    /// The crate version. Never bumped so far, so it is reported for
    /// completeness rather than as the answer to "what is running".
    version: &'static str,
}

/// Body of `GET /capacity`.
///
/// Deliberately separate from [`ConfigResponse`]: the frontend fetches
/// `/config` once at build time (`force-static`), which would freeze these
/// numbers at deploy. Capacity is dynamic, so it gets its own endpoint that the
/// browser polls.
#[derive(serde::Serialize)]
struct CapacityResponse {
    /// The pair to display: usage of the cap that would refuse the next spawn.
    /// Resolved server-side (see [`gl_core::Capacity::binding`]) so a client
    /// never has to know that a `Failed` row holds a slot without holding RAM,
    /// and never shows a number that contradicts `is_full`.
    used: u32,
    total: u32,
    /// Whether either cap is currently met. Precomputed so the frontend does
    /// not have to re-derive the "which caps count as full" rule and drift from
    /// the server's own definition.
    is_full: bool,
    /// The raw per-cap counts, kept for operators and debugging. Clients should
    /// prefer `used`/`total`; these two pairs disagree whenever a `Failed` row
    /// is holding a slot, which is exactly the confusion `used` exists to
    /// prevent.
    active: u32,
    max_active: u32,
    provisioned: u32,
    max_provisioned: u32,
}

#[derive(serde::Serialize)]
struct ErrorResponse {
    error: String,
    code: String,
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

enum AppError {
    NotFound(String),
    Invalid(String),
    ServiceUnavailable(String),
    /// A cap was hit. Renders 503 with a `Retry-After` header and a body that
    /// names which limit was exceeded (server-full vs. busy).
    CapacityFull {
        message: String,
        code: String,
        retry_after_secs: u64,
    },
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // CapacityFull needs an extra Retry-After header, so handle it up front.
        if let AppError::CapacityFull {
            message,
            code,
            retry_after_secs,
        } = self
        {
            let mut response = (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: message,
                    code,
                }),
            )
                .into_response();
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                HeaderValue::from(retry_after_secs),
            );
            return response;
        }

        let (status, message, code) = match self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, "not_found"),
            AppError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg, "invalid"),
            AppError::ServiceUnavailable(msg) => {
                (StatusCode::SERVICE_UNAVAILABLE, msg, "service_unavailable")
            }
            AppError::CapacityFull { .. } => unreachable!("handled above"),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg, "internal_error"),
        };
        (
            status,
            Json(ErrorResponse {
                error: message,
                code: code.into(),
            }),
        )
            .into_response()
    }
}

impl AppError {
    /// Map a gl-core error onto its HTTP-facing form.
    ///
    /// `capacity_retry_after_secs` becomes the `Retry-After` header on the 503 a
    /// capacity cap produces. Callers pass [`Config::sweep_interval_secs`],
    /// because the sweep is the only thing that frees a slot: the API exposes no
    /// despawn endpoint, so capacity is reclaimed when an instance ages past
    /// `life_in_days` *and* the sweeper next runs. Deriving the hint from config
    /// rather than a constant also means it follows the operator if they shorten
    /// the interval.
    ///
    /// [`Config::sweep_interval_secs`]: gl_core::Config::sweep_interval_secs
    fn from_core(e: gl_core::Error, capacity_retry_after_secs: u64) -> Self {
        match e {
            gl_core::Error::NotFound => AppError::NotFound("not found".into()),
            gl_core::Error::Invalid => AppError::Invalid("invalid".into()),
            gl_core::Error::PortExhausted => {
                AppError::ServiceUnavailable("port range exhausted".into())
            }
            gl_core::Error::CapacityFull { kind } => {
                // Distinguish disk-bound (server full) from RAM-bound (busy).
                // Matching the enum keeps this exhaustive: a new cap cannot be
                // added in gl-core without the compiler demanding a code here.
                let (message, code) = match kind {
                    CapacityKind::Provisioned => (
                        "server is full; no capacity for new instances",
                        "server_full",
                    ),
                    CapacityKind::Active => {
                        ("server is busy; too many running instances", "server_busy")
                    }
                };
                AppError::CapacityFull {
                    message: message.to_string(),
                    code: code.to_string(),
                    retry_after_secs: capacity_retry_after_secs,
                }
            }
            other => AppError::Internal(other.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn spawn_goopy(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, AppError> {
    let slug = tokio::task::spawn_blocking({
        let state = Arc::clone(&state);
        move || state.manager.spawn()
    })
    .await
    .map_err(|e| AppError::Internal(format!("task join error: {e}")))?
    .map_err(|e| AppError::from_core(e, state.cfg.sweep_interval_secs))?;

    Ok((
        StatusCode::CREATED,
        Json(SpawnResponse {
            slug,
            status: "Spawning".into(),
        }),
    ))
}

async fn get_goopy(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let domain = state.cfg.domain.clone();

    let goopy = tokio::task::spawn_blocking({
        let state = Arc::clone(&state);
        move || state.manager.get(&slug)
    })
    .await
    .map_err(|e| AppError::Internal(format!("task join error: {e}")))?
    .map_err(|e| AppError::from_core(e, state.cfg.sweep_interval_secs))?;

    let goopy = goopy.ok_or_else(|| AppError::NotFound("not found".into()))?;

    let expires_at = goopy.created_at + Duration::days(goopy.life_in_days as i64);
    let is_expired = Utc::now() >= expires_at;

    let url = if domain == "localhost" {
        format!("http://localhost:{}", goopy.port)
    } else {
        format!("https://{}.{}", goopy.slug, domain)
    };

    Ok(Json(GoopyResponse {
        slug: goopy.slug,
        status: goopy.status.to_string(),
        url,
        created_at: goopy.created_at.to_rfc3339(),
        expires_at: expires_at.to_rfc3339(),
        is_expired,
    }))
}

/// `GET /goopies/:slug/alive` — nginx's `auth_request` expiry gate.
///
/// This endpoint speaks the `auth_request` protocol, not REST: **200 allows
/// the request through, 403 denies it**, and the site's `error_page 403 =
/// @expired` turns the denial into the redirect a visitor sees.
///
/// Those are very nearly the only two codes available. nginx's
/// `ngx_http_auth_request_module` forwards 401 and 403 verbatim, treats any
/// 2xx as "allow", and collapses **everything else into a 500** — logging
/// only `auth request unexpected status`. A 410 here does not produce a 410
/// at the edge; it produces a blank 500 page, which is how this endpoint
/// previously broke the expiry redirect without anyone noticing.
///
/// So: do not return a status from here that is not 200 or 403 without
/// working through what nginx will actually do with it.
///
/// # Cacheability
///
/// nginx caches this subrequest (see `proxy_cache` in the generated site), and
/// **gl-serv decides per response whether it may**, rather than the site
/// applying a blanket TTL. A live instance is cacheable for a few seconds; a
/// denial never is.
///
/// That asymmetry is the point. Once #96 lands, this endpoint doubles as the
/// wake trigger for a suspended instance, and a cached denial would strand it —
/// nginx would keep answering from cache and the wake would never fire. Because
/// only the `200` arm is cacheable, a suspended or expired instance always
/// reaches gl-serv, and #96 inherits correct behaviour without having to revisit
/// the cache.
async fn alive_check(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Result<Response, AppError> {
    let goopy = tokio::task::spawn_blocking({
        let state = Arc::clone(&state);
        move || state.manager.get(&slug)
    })
    .await
    .map_err(|e| AppError::Internal(format!("task join error: {e}")))?
    .map_err(|e| AppError::from_core(e, state.cfg.sweep_interval_secs))?;

    let Some(goopy) = goopy else {
        return Ok(deny());
    };

    let expires_at = goopy.created_at + Duration::days(goopy.life_in_days as i64);
    let alive = goopy.status == gl_core::Status::Done && Utc::now() < expires_at;

    if alive {
        Ok(allow(state.cfg.ratelimit.alive_cache_secs))
    } else {
        Ok(deny())
    }
}

/// `200` with a short `max-age`, letting nginx serve the next few seconds of
/// subrequests for this slug from cache.
fn allow(cache_secs: u64) -> Response {
    let mut response = StatusCode::OK.into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_str(&format!("max-age={cache_secs}"))
            .expect("max-age from a u64 is always a valid header value"),
    );
    response
}

/// `403` with `no-store`, which is what keeps the cache wake-safe: a denial is
/// never remembered, so a suspended or expired instance always reaches gl-serv.
/// See the note on [`alive_check`].
fn deny() -> Response {
    let mut response = StatusCode::FORBIDDEN.into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

/// `GET /capacity` — current usage of both instance caps.
///
/// Advisory only: it is read without a lock and can go stale between the poll
/// and a click, and two clients can race for the last slot. The 503 from
/// `POST /goopies` stays the authority; this endpoint exists so the UI can warn
/// *before* the click rather than only after it.
async fn get_capacity(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, AppError> {
    let capacity = tokio::task::spawn_blocking({
        let state = Arc::clone(&state);
        move || state.manager.capacity()
    })
    .await
    .map_err(|e| AppError::Internal(format!("task join error: {e}")))?
    .map_err(|e| AppError::from_core(e, state.cfg.sweep_interval_secs))?;

    let (used, total) = capacity.binding();

    Ok(Json(CapacityResponse {
        used,
        total,
        is_full: capacity.is_full(),
        active: capacity.active,
        max_active: capacity.max_active,
        provisioned: capacity.provisioned,
        max_provisioned: capacity.max_provisioned,
    }))
}

/// `GET /version` — the commit this process was built from.
///
/// Answers "is the thing I merged the thing that is running?" without an ssh
/// session and a guess at file mtimes. Two callers: the frontend footer, and
/// `deploy/push-binary.sh`, which compares this against the sha it just built
/// so the post-deploy check is an identity check rather than a liveness check.
///
/// `no-store` because the whole point is that it reflects the process that is
/// answering right now; a cached copy is the stale-version problem again,
/// moved one hop out.
///
/// Takes no state: every value is a compile-time constant of the binary.
async fn get_version() -> impl IntoResponse {
    let mut response = Json(VersionResponse {
        sha: gl_core::build_info::short_git_sha(),
        sha_full: gl_core::build_info::GIT_SHA,
        built_at: gl_core::build_info::BUILT_AT,
        version: env!("CARGO_PKG_VERSION"),
    })
    .into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(ConfigResponse {
        life_in_days: state.cfg.life_in_days,
        storage_quota_mb: state.cfg.allocator.quota_mb,
        domain: state.cfg.domain.clone(),
    })
}

// ---------------------------------------------------------------------------
// Rate limiting helpers
// ---------------------------------------------------------------------------

/// A throttled request's `429` response.
///
/// `tower_governor` already computes the wait time and a set of rate-limit
/// headers (e.g. `x-ratelimit-after`), but its default body is plain text, so
/// this re-renders it as the same `ErrorResponse` JSON shape used by
/// [`AppError`] while preserving those headers.
struct RateLimitedResponse {
    /// Seconds until the client may retry, as reported by the governor.
    wait_time: u64,
    /// Extra rate-limit headers computed by the governor, if any.
    extra_headers: Option<axum::http::HeaderMap>,
}

impl IntoResponse for RateLimitedResponse {
    fn into_response(self) -> Response {
        let mut resp = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: format!("rate limit exceeded; retry in {}s", self.wait_time),
                code: "too_many_requests".into(),
            }),
        )
            .into_response();

        let headers = resp.headers_mut();
        if let Ok(value) = HeaderValue::from_str(&self.wait_time.to_string()) {
            headers.insert(axum::http::header::RETRY_AFTER, value);
        }
        if let Some(extra) = self.extra_headers {
            for (name, value) in &extra {
                headers.insert(name.clone(), value.clone());
            }
        }

        resp
    }
}

/// Convert a `tower_governor` error into a JSON API response.
///
/// The common case (`TooManyRequests`) becomes a [`RateLimitedResponse`]: a
/// `429` with a JSON body and a `Retry-After` header, matching the error shape
/// used elsewhere in the API.
///
/// Any other variant (e.g. `UnableToExtractKey` when no client IP can be
/// resolved, or an internal governor error) indicates a server-side problem
/// rather than client abuse, so it maps to `500` via [`AppError::Internal`]. In
/// production nginx always sets `X-Real-IP`, so `UnableToExtractKey` should
/// never occur.
fn rate_limit_error_handler(err: tower_governor::GovernorError) -> Response<Body> {
    match err {
        tower_governor::GovernorError::TooManyRequests { wait_time, headers } => {
            RateLimitedResponse {
                wait_time,
                extra_headers: headers,
            }
            .into_response()
        }
        other => {
            tracing::error!(error = ?other, "rate-limit middleware failed");
            AppError::Internal("internal rate-limit error".into()).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Router builder (shared by main and tests)
// ---------------------------------------------------------------------------

/// How often each governor evicts per-IP entries that are no longer rate
/// limiting anything.
const GOVERNOR_CLEANUP_INTERVAL_SECS: u64 = 60;

/// Spawn the background task that periodically evicts a governor's stale
/// per-IP entries.
///
/// `retain` is expected to call `retain_recent()` on the governor's limiter.
/// It is taken as a closure rather than the limiter itself so that the
/// limiter's concrete type — which mentions `governor` types that
/// `tower_governor` does not re-export — stays inferred at the call site.
fn spawn_governor_cleanup<F>(retain: F, label: &'static str)
where
    F: Fn() + Send + 'static,
{
    let interval_duration = StdDuration::from_secs(GOVERNOR_CLEANUP_INTERVAL_SECS);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(interval_duration);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The first tick fires immediately; skip it so cleanup runs after one
        // full interval rather than at startup, when there is nothing to evict.
        interval.tick().await;
        loop {
            interval.tick().await;
            retain();
            tracing::debug!(governor = label, "evicted stale rate-limit entries");
        }
    });
}

/// Build the application router.
///
/// Two separate `GovernorLayer`s are applied:
/// - A **tight** limit (`provision_burst` / `provision_period_secs`) covers
///   only `POST /goopies` (the expensive provisioning path).
/// - A **loose** limit (`read_burst` / `read_period_secs`) covers all read
///   endpoints.
///
/// Both use `SmartIpKeyExtractor`, which resolves the client IP from
/// `X-Real-IP` (set by nginx), falling back to `X-Forwarded-For` and then the
/// TCP peer address.
///
/// Each governor keeps one in-memory entry per distinct client IP, which is
/// never reclaimed on its own — on a public, unauthenticated API that grows
/// without bound. So this also spawns one background task per governor that
/// calls `retain_recent()` every [`GOVERNOR_CLEANUP_INTERVAL_SECS`], dropping
/// entries whose rate-limit state has fully replenished. The tasks are tied to
/// the governors created here rather than to `main`, so tests exercise the same
/// wiring; they must therefore be called from within a Tokio runtime.
///
/// # Panics
///
/// Panics if a rate-limit value is zero. [`gl_core::Config::from_file`] rejects
/// those before this is reached, so this is unreachable for any config loaded
/// from disk.
fn build_router(
    state: Arc<AppState>,
    cors: CorsLayer,
    rl: &gl_core::config::RateLimitConfig,
) -> Router {
    // Tight limit for the provisioning endpoint.
    let provision_governor = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .burst_size(rl.provision_burst)
        .period(StdDuration::from_secs(rl.provision_period_secs))
        .finish()
        .expect("provision rate-limit values are validated by Config::from_file");

    {
        let limiter = provision_governor.limiter().clone();
        spawn_governor_cleanup(move || limiter.retain_recent(), "provision");
    }

    let provision_layer =
        GovernorLayer::new(provision_governor).error_handler(rate_limit_error_handler);

    // Loose limit for read endpoints.
    let read_governor = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .burst_size(rl.read_burst)
        .period(StdDuration::from_secs(rl.read_period_secs))
        .finish()
        .expect("read rate-limit values are validated by Config::from_file");

    {
        let limiter = read_governor.limiter().clone();
        spawn_governor_cleanup(move || limiter.retain_recent(), "read");
    }

    let read_layer = GovernorLayer::new(read_governor).error_handler(rate_limit_error_handler);

    // Separate limit for the nginx liveness subrequest.
    //
    // This is not a user-facing read, so it cannot share the read budget: nginx
    // fires it once per HTTP request to an instance, meaning one page view
    // costs one token per subresource. Exhausting it does not degrade
    // gracefully either — auth_request renders a 429 as a 500 — so the page
    // simply does not load.
    let alive_governor = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .burst_size(rl.alive_burst)
        .period(StdDuration::from_secs(rl.alive_period_secs))
        .finish()
        .expect("alive rate-limit values are validated by Config::from_file");

    {
        let limiter = alive_governor.limiter().clone();
        spawn_governor_cleanup(move || limiter.retain_recent(), "alive");
    }

    let alive_layer = GovernorLayer::new(alive_governor).error_handler(rate_limit_error_handler);

    let spawn_routes = Router::new()
        .route("/goopies", post(spawn_goopy))
        .layer(provision_layer)
        .with_state(Arc::clone(&state));

    let read_routes = Router::new()
        .route("/goopies/{slug}", get(get_goopy))
        .route("/config", get(get_config))
        .route("/capacity", get(get_capacity))
        .route("/version", get(get_version))
        .layer(read_layer)
        .with_state(Arc::clone(&state));

    let alive_routes = Router::new()
        .route("/goopies/{slug}/alive", get(alive_check))
        .layer(alive_layer)
        .with_state(Arc::clone(&state));

    Router::new()
        .merge(spawn_routes)
        .merge(read_routes)
        .merge(alive_routes)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

/// Serve `app` on `listener`.
///
/// The router is wrapped in `into_make_service_with_connect_info` so every
/// request carries its TCP peer address. `SmartIpKeyExtractor` needs that as
/// its last-resort fallback: without it, a request that carries neither
/// `X-Real-IP` nor `X-Forwarded-For` has no key at all, and the rate-limit
/// layer fails the request with a 500 instead of limiting it. nginx always sets
/// `X-Real-IP` in production, but anything talking to gl-serv directly — a
/// local frontend during development, a health check — would otherwise get a
/// 500 from every read endpoint.
///
/// Shared with the tests so they exercise the same wiring rather than a
/// look-alike of it.
async fn serve(listener: tokio::net::TcpListener, app: Router) -> std::io::Result<()> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

/// Sweep expired and `Failed` instances every `interval_duration`, forever.
///
/// The first sweep runs **at startup**, not one interval later. The sweep is
/// the only thing that frees a capacity slot, so deferring it by a full
/// interval meant every restart pushed it further away: with #112 deploying on
/// each merge to trunk, an active day could postpone it indefinitely, and a fix
/// to the teardown would not take effect until a day after it shipped (#117).
///
/// Sweeping at startup costs the startup path nothing. This runs on a detached
/// task and hands the blocking work to `spawn_blocking`, so it cannot delay the
/// listener bind — and therefore cannot turn a deploy's `systemctl is-active`
/// check into a false negative.
///
/// The loop waits for each sweep before the next tick, because a truthful count
/// is exactly the thing that has to be waited for. That makes a wedged teardown
/// expensive: `SysRunner` waits on its privileged children with no timeout
/// (#156), so one hung `systemctl` stops every later sweep for the life of the
/// process. Until that timeout exists, an overrunning sweep at least says so —
/// see `warn_if_sweep_overruns`.
///
/// The outcome of a sweep is logged by `GoopyManager::sweep` itself and
/// deliberately not repeated here; this function logs only the failures that
/// `sweep` cannot log for itself.
async fn run_sweeper(manager: Arc<dyn ManagerService>, interval_duration: std::time::Duration) {
    // `Config::from_file` already rejects a zero interval, so this only fires
    // for a `Config` built in code. Kept because the invariant belongs where
    // `tokio::time::interval` would otherwise panic on it.
    assert!(
        !interval_duration.is_zero(),
        "sweep_interval_secs must be > 0 — Config::from_file enforces this \
         for configs read from disk"
    );

    let mut interval = tokio::time::interval(interval_duration);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // The first tick completes immediately; every later one waits.
        interval.tick().await;
        let manager = Arc::clone(&manager);
        let sweep = tokio::task::spawn_blocking(move || manager.sweep());
        match warn_if_sweep_overruns(sweep, interval_duration).await {
            // The `(swept, failed)` line belongs to `GoopyManager::sweep`.
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::error!(error = %e, "sweep failed"),
            Err(e) => tracing::error!(error = %e, "sweep task panicked"),
        }
    }
}

/// Await `sweep`, logging a warning if it has not finished within `budget`.
///
/// A sweep that outruns its own interval is the visible symptom of a teardown
/// wedged on a privileged command (#156). The wait is not cut short — the count
/// is only true once the teardown settles — but the stall stops being silent,
/// which is the same failure mode #117 was filed about.
async fn warn_if_sweep_overruns<T>(
    sweep: tokio::task::JoinHandle<T>,
    budget: std::time::Duration,
) -> Result<T, tokio::task::JoinError> {
    tokio::pin!(sweep);

    tokio::select! {
        result = &mut sweep => return result,
        _ = tokio::time::sleep(budget) => {
            tracing::warn!(
                budget_secs = budget.as_secs(),
                "sweep is still running a full interval after it started; \
                 a teardown is likely wedged on a privileged command (#156)",
            );
        }
    }

    sweep.await
}

#[tokio::main]
async fn main() {
    let span_events = match std::env::var("RUST_LOG_SPANS").as_deref() {
        Ok("0") | Ok("false") | Ok("") | Err(_) => tracing_subscriber::fmt::format::FmtSpan::NONE,
        Ok(_) => tracing_subscriber::fmt::format::FmtSpan::FULL,
    };

    tracing_subscriber::fmt()
        .with_span_events(span_events)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // The version is set at runtime rather than via `#[command(version)]`,
    // which takes only a `&'static str`: the commit stamp lives in gl-core's
    // build environment, not this crate's.
    let cli = Cli::from_arg_matches(
        &Cli::command()
            .version(gl_core::build_info::describe(env!("CARGO_PKG_VERSION")))
            .get_matches(),
    )
    .unwrap_or_else(|e| e.exit());

    // Before anything is opened, bound or spawned: --check-config reads the
    // file and the filesystem and nothing else, so it can run against a live
    // host while the previous binary is still serving.
    if cli.check_config {
        match check_config(&cli.config) {
            Ok((cfg, summary)) => {
                // Printed before the host checks run, so a failure arrives
                // next to the values it was judged against.
                println!("{summary}");
                if let Err(e) = check_host_paths(&cfg) {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
                return;
            }
            Err(e) => {
                // `Error::Config` already renders its own "config error:"
                // prefix, so the message is printed bare.
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }

    let cfg = gl_core::Config::from_file(&cli.config).unwrap_or_else(|e| {
        tracing::error!("config error: {e}");
        std::process::exit(1);
    });

    let provisioner = cfg.build_provisioner(cfg.dev_mode, Arc::new(RealSysRunner));

    let registry = SqliteRegistry::new(&cfg.registry.path).unwrap_or_else(|e| {
        tracing::error!("failed to open SQLite registry: {e}");
        std::process::exit(1);
    });

    let manager: Arc<dyn ManagerService> = Arc::new(GoopyManager::new(
        cfg.build_manager_config(),
        registry,
        provisioner,
    ));

    let cors_origin = cfg.cors_origin.parse::<HeaderValue>().unwrap_or_else(|e| {
        tracing::error!("invalid cors_origin in config: {e}");
        std::process::exit(1);
    });

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list([cors_origin]))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(tower_http::cors::Any);

    let bind_address = cfg.bind_address.clone();
    let sweep_interval_secs = cfg.sweep_interval_secs;
    let ratelimit_cfg = cfg.ratelimit.clone();

    let state = Arc::new(AppState { manager, cfg });

    // Spawn the periodic sweep background task. `run_sweeper` asserts the
    // interval is non-zero.
    tokio::spawn(run_sweeper(
        Arc::clone(&state.manager),
        std::time::Duration::from_secs(sweep_interval_secs),
    ));

    let app = build_router(state, cors, &ratelimit_cfg);

    let listener = tokio::net::TcpListener::bind(&bind_address)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("failed to bind to {bind_address}: {e}");
            std::process::exit(1);
        });

    tracing::info!("listening on {bind_address}");
    serve(listener, app).await.unwrap_or_else(|e| {
        tracing::error!("server error: {e}");
        std::process::exit(1);
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::Request;
    use chrono::Duration;
    use gl_core::goopy_provisioner::GoopyProvisioner;
    use gl_core::goopy_registry::GoopyRegistry;
    use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
    use gl_core::{Goopy, GoopyManager, GoopyManagerConfig, ProvisionerKind, Status};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use tower::ServiceExt;

    // ── Test provisioner ──────────────────────────────────────────────────

    struct NoopProvisioner;

    impl GoopyProvisioner for NoopProvisioner {
        fn provision(&self, _: &Goopy) -> Result<(), gl_core::Error> {
            Ok(())
        }
        fn deprovision(&self, _: &Goopy) -> Result<(), gl_core::Error> {
            Ok(())
        }
        fn kind(&self) -> ProvisionerKind {
            ProvisionerKind::Hello
        }
        fn service_version(&self) -> &str {
            "9.9.9-mock"
        }
    }

    // ── Test helpers ──────────────────────────────────────────────────────

    fn test_cfg(domain: &str) -> gl_core::Config {
        gl_core::Config {
            base_dir: PathBuf::from("/tmp/goopy-test"),
            domain: domain.to_string(),
            life_in_days: 7,
            port_range_start: 9000,
            port_range_end: 9100,
            dev_mode: true,
            cors_origin: "https://example.com".to_string(),
            bind_address: "127.0.0.1:0".to_string(),
            api_address: None,
            sweep_interval_secs: 86400,
            event_retention_days: 30,
            max_active: 100,
            max_provisioned: 100,
            registry: gl_core::config::RegistryConfig {
                path: PathBuf::from(":memory:"),
            },
            allocator: gl_core::config::AllocatorConfig {
                kind: gl_core::AllocatorKind::PlainDir,
                pool: String::new(),
                quota_mb: 0,
            },
            provisioner: gl_core::config::ProvisionerConfig::Hello,
            ratelimit: gl_core::config::RateLimitConfig::default(),
        }
    }

    /// Build a test router using the given registry (pass pre-seeded registries
    /// for tests that need existing goopies).
    fn make_router(domain: &str, registry: SqliteRegistry) -> Router {
        make_router_with(
            domain,
            registry,
            gl_core::config::RateLimitConfig::default(),
            100,
            100,
        )
    }

    /// Build a test router with explicit rate-limit settings.
    fn make_router_with_rl(
        domain: &str,
        registry: SqliteRegistry,
        rl: gl_core::config::RateLimitConfig,
    ) -> Router {
        make_router_with(domain, registry, rl, 100, 100)
    }

    /// Like [`make_router`] but with explicit capacity caps, for cap tests.
    fn make_router_with_caps(
        domain: &str,
        registry: SqliteRegistry,
        max_active: u32,
        max_provisioned: u32,
    ) -> Router {
        make_router_with(
            domain,
            registry,
            gl_core::config::RateLimitConfig::default(),
            max_active,
            max_provisioned,
        )
    }

    /// Shared builder behind the three helpers above.
    fn make_router_with(
        domain: &str,
        registry: SqliteRegistry,
        rl: gl_core::config::RateLimitConfig,
        max_active: u32,
        max_provisioned: u32,
    ) -> Router {
        let cfg = test_cfg(domain);
        let manager: Arc<dyn ManagerService> = Arc::new(GoopyManager::new(
            GoopyManagerConfig {
                base_dir: cfg.base_dir.clone(),
                domain: cfg.domain.clone(),
                life_in_days: cfg.life_in_days,
                port_range_start: cfg.port_range_start,
                port_range_end: cfg.port_range_end,
                max_active,
                max_provisioned,
                event_retention_days: 30,
            },
            registry,
            NoopProvisioner,
        ));
        let cors_origin = cfg.cors_origin.parse::<HeaderValue>().unwrap();
        let cors = CorsLayer::new()
            .allow_origin(AllowOrigin::list([cors_origin]))
            .allow_methods([Method::GET, Method::POST])
            .allow_headers(tower_http::cors::Any);
        let state = Arc::new(AppState { manager, cfg });
        build_router(state, cors, &rl)
    }

    /// The `Retry-After` a capacity 503 should carry: the sweep interval, since
    /// the sweep is the only thing that frees a slot. Read back off the same
    /// config the router is built from so the two cannot drift.
    fn expected_retry_after(domain: &str) -> String {
        test_cfg(domain).sweep_interval_secs.to_string()
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn seed_goopy(
        registry: &SqliteRegistry,
        slug: &str,
        life_in_days: i32,
        days_ago: i64,
        port: u32,
        status: Status,
    ) -> Goopy {
        let goopy = Goopy {
            slug: slug.to_string(),
            life_in_days,
            created_at: Utc::now() - Duration::days(days_ago),
            working_dir: PathBuf::from(format!("/tmp/goopy-test/{slug}")),
            port,
            status,
            provisioner_kind: ProvisionerKind::Hello,
            service_version: "0.1.0".to_string(),
        };
        registry.save(&goopy).unwrap();
        registry.acquire_port(slug, port, port + 1).unwrap();
        goopy
    }

    // ── spawn_goopy ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_returns_201_with_slug_and_status() {
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .method("POST")
                    .uri("/goopies")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert!(body["slug"].is_string(), "slug should be present");
        assert_eq!(body["status"], "Spawning");
    }

    #[tokio::test]
    async fn spawn_returns_503_when_ports_exhausted() {
        // Port range start == end means no ports available.
        let mut cfg = test_cfg("goopy.life");
        cfg.port_range_start = 9000;
        cfg.port_range_end = 9000; // empty range → PortExhausted on first acquire

        let manager: Arc<dyn ManagerService> = Arc::new(GoopyManager::new(
            GoopyManagerConfig {
                base_dir: PathBuf::from("/tmp/goopy-test"),
                domain: cfg.domain.clone(),
                life_in_days: cfg.life_in_days,
                port_range_start: cfg.port_range_start,
                port_range_end: cfg.port_range_end,
                max_active: 100,
                max_provisioned: 100,
                event_retention_days: 30,
            },
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
            NoopProvisioner,
        ));
        let cors = CorsLayer::new()
            .allow_methods([Method::GET, Method::POST])
            .allow_headers(tower_http::cors::Any);
        let state = Arc::new(AppState { manager, cfg });
        let app = build_router(state, cors, &gl_core::config::RateLimitConfig::default());

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .method("POST")
                    .uri("/goopies")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["code"], "service_unavailable");
    }

    #[tokio::test]
    async fn spawn_returns_503_with_retry_after_when_provisioned_cap_hit() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // One Failed goopy fills the (provisioned = 1) cap; Failed still counts.
        seed_goopy(&registry, "full-server-slug", 7, 0, 9050, Status::Failed);
        let app = make_router_with_caps("goopy.life", registry, 100, 1);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .method("POST")
                    .uri("/goopies")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some(expected_retry_after("goopy.life").as_str()),
            "capacity-full 503 must advertise the sweep interval, the cadence at \
             which a slot can actually free up"
        );
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["code"], "server_full");
    }

    #[tokio::test]
    async fn spawn_returns_503_server_busy_when_active_cap_hit() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // One resident (Done) goopy fills the (active = 1) cap.
        seed_goopy(&registry, "busy-server-slug", 7, 0, 9051, Status::Done);
        let app = make_router_with_caps("goopy.life", registry, 1, 100);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .method("POST")
                    .uri("/goopies")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some(expected_retry_after("goopy.life").as_str())
        );
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["code"], "server_busy");
    }

    // ── get_goopy ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_goopy_returns_200_with_subdomain_url() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "happy-little-slug", 7, 0, 9001, Status::Done);
        let app = make_router("goopy.life", registry);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/goopies/happy-little-slug")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["slug"], "happy-little-slug");
        assert_eq!(body["url"], "https://happy-little-slug.goopy.life");
    }

    #[tokio::test]
    async fn get_goopy_localhost_domain_uses_http_port_url() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "local-test-slug", 7, 0, 9042, Status::Done);
        let app = make_router("localhost", registry);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/goopies/local-test-slug")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["url"], "http://localhost:9042");
    }

    #[tokio::test]
    async fn get_goopy_expires_at_uses_instance_life_in_days() {
        // Config says 7 days, but the goopy was saved with life_in_days = 3.
        // expires_at must reflect the per-instance value, not the config.
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        let goopy = seed_goopy(&registry, "short-lived-slug", 3, 0, 9002, Status::Done);
        let app = make_router("goopy.life", registry);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/goopies/short-lived-slug")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;

        let expected_expires_at = (goopy.created_at + Duration::days(3)).to_rfc3339();
        assert_eq!(body["expires_at"], expected_expires_at);
    }

    #[tokio::test]
    async fn get_goopy_is_expired_false_for_live_instance() {
        // Created now with a 7-day life: is_expired must be false.
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "live-slug", 7, 0, 9010, Status::Done);
        let app = make_router("goopy.life", registry);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/goopies/live-slug")
                    .header("x-real-ip", "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["is_expired"], false);
    }

    #[tokio::test]
    async fn get_goopy_is_expired_true_for_expired_instance() {
        // Created 10 days ago with a 7-day life: is_expired must be true.
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "old-slug", 7, 10, 9011, Status::Done);
        let app = make_router("goopy.life", registry);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/goopies/old-slug")
                    .header("x-real-ip", "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["is_expired"], true);
    }

    #[tokio::test]
    async fn get_goopy_unknown_slug_returns_404_with_code() {
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/goopies/no-such-slug")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["code"], "not_found");
    }

    // ── alive_check ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn alive_check_returns_200_for_alive_goopy() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // Created now, lives 7 days → not expired, status Done
        seed_goopy(&registry, "alive-slug", 7, 0, 9003, Status::Done);
        let app = make_router("goopy.life", registry);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/goopies/alive-slug/alive")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn alive_check_returns_403_for_expired_goopy() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // Created 10 days ago, lives 7 → expired
        seed_goopy(&registry, "expired-slug", 7, 10, 9004, Status::Done);
        let app = make_router("goopy.life", registry);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/goopies/expired-slug/alive")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn alive_check_returns_403_for_non_done_status() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // Still spawning → not alive even if within lifetime
        seed_goopy(&registry, "spawning-slug", 7, 0, 9005, Status::Spawning);
        let app = make_router("goopy.life", registry);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/goopies/spawning-slug/alive")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn alive_check_returns_403_for_unknown_slug() {
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/goopies/no-such/alive")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // ── get_config ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_config_returns_correct_fields() {
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["domain"], "goopy.life");
        assert_eq!(body["life_in_days"], 7);
        assert_eq!(body["storage_quota_mb"], 0); // PlainDir has no quota
    }

    /// The backend's commit must never travel on `/config`. That body is
    /// fetched once at Vercel build time and frozen into a static page, and
    /// backend-only merges do not rebuild the frontend at all — so a sha here
    /// would be served as fact long after it stopped being true. This asserts
    /// the decision rather than trusting it to stay remembered.
    #[tokio::test]
    async fn get_config_does_not_carry_the_backend_version() {
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = body_json(resp.into_body()).await;
        for field in ["sha", "sha_full", "built_at", "version"] {
            assert!(
                body.get(field).is_none(),
                "/config must not carry {field}: it is frozen at frontend build time",
            );
        }
    }

    // ── get_version ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_version_reports_the_build_stamp() {
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        // Compared against the constants rather than a literal: the test binary
        // is stamped by whatever built it, which under `cargo test` is
        // "unknown" and under a deploy is a real commit. Both must serve.
        assert_eq!(body["sha_full"], gl_core::build_info::GIT_SHA);
        assert_eq!(body["sha"], gl_core::build_info::short_git_sha());
        assert_eq!(body["built_at"], gl_core::build_info::BUILT_AT);
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    }

    /// A build made by neither deploy path says so. The endpoint exists to
    /// answer "which commit is serving"; inventing one when there is none is
    /// the failure it was built to remove.
    #[tokio::test]
    async fn get_version_reports_unknown_for_an_unstamped_build() {
        // `cargo test` sets neither GL_GIT_SHA nor GL_BUILT_AT, so this test
        // binary *is* an unstamped build — unless it was built by a deploy, in
        // which case there is a real sha to report and nothing to assert.
        if gl_core::build_info::GIT_SHA != gl_core::build_info::UNKNOWN {
            return;
        }

        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = body_json(resp.into_body()).await;
        assert_eq!(body["sha"], "unknown");
        assert_eq!(body["sha_full"], "unknown");
    }

    /// The answer is about the process replying right now, so a cached copy is
    /// the stale-version problem moved one hop out.
    #[tokio::test]
    async fn get_version_is_never_cached() {
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            resp.headers()
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
        );
    }

    /// `/version` is a read, not a provisioning request: the deploy polls it
    /// and every visitor's footer fetches it, so it must sit on the loose read
    /// limiter. With `provision_burst = 1`, a run of reads that would exhaust
    /// the provisioning budget must all pass.
    #[tokio::test]
    async fn get_version_uses_the_loose_read_rate_limit() {
        let rl = gl_core::config::RateLimitConfig {
            provision_burst: 1,
            provision_period_secs: 60,
            read_burst: 100,
            read_period_secs: 1,
            alive_burst: 600,
            alive_period_secs: 1,
            alive_cache_secs: 5,
        };
        let app = make_router_with_rl(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
            rl,
        );

        for attempt in 0..5 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .header("x-real-ip", "203.0.113.11")
                        .uri("/version")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "read #{attempt} must not be throttled by the provisioning limit",
            );
        }
    }

    // ── get_capacity ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_capacity_returns_zero_usage_and_configured_caps() {
        let app = make_router_with_caps(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
            3,
            5,
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/capacity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["active"], 0);
        assert_eq!(body["max_active"], 3);
        assert_eq!(body["provisioned"], 0);
        assert_eq!(body["max_provisioned"], 5);
        assert_eq!(body["is_full"], false);
        // The active cap has less headroom (3 vs 5), so it is the one shown.
        assert_eq!(body["used"], 0);
        assert_eq!(body["total"], 3);
    }

    #[tokio::test]
    async fn get_capacity_counts_failed_as_provisioned_but_not_active() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "cap-done", 7, 0, 9401, Status::Done);
        seed_goopy(&registry, "cap-failed", 7, 0, 9402, Status::Failed);
        let app = make_router_with_caps("goopy.life", registry, 10, 10);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/capacity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        // The Failed row still holds a registry slot but no longer holds RAM.
        assert_eq!(body["active"], 1);
        assert_eq!(body["provisioned"], 2);
        assert_eq!(body["is_full"], false);
        // Equal caps, so the provisioned pair binds — the one a visitor is
        // actually blocked by.
        assert_eq!(body["used"], 2);
        assert_eq!(body["total"], 10);
    }

    #[tokio::test]
    async fn get_capacity_reports_is_full_when_only_one_cap_is_met() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // A Failed row meets a provisioned cap of 1 while leaving active
        // headroom — is_full must still be true, matching what spawn enforces.
        seed_goopy(&registry, "cap-failed", 7, 0, 9403, Status::Failed);
        let app = make_router_with_caps("goopy.life", registry, 10, 1);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/capacity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["active"], 0);
        assert_eq!(body["provisioned"], 1);
        assert_eq!(body["is_full"], true);
        // The displayed pair must show a full server, not the active count's
        // misleading "0 / 10".
        assert_eq!(body["used"], 1);
        assert_eq!(body["total"], 1);
    }

    /// `/capacity` is polled by every visitor, so it must sit on the loose read
    /// limiter, not the tight provisioning one. With `provision_burst = 1`, a
    /// run of reads that would exhaust the provisioning budget must all pass.
    #[tokio::test]
    async fn get_capacity_uses_the_loose_read_rate_limit() {
        let rl = gl_core::config::RateLimitConfig {
            provision_burst: 1,
            provision_period_secs: 60,
            read_burst: 100,
            read_period_secs: 1,
            alive_burst: 600,
            alive_period_secs: 1,
            alive_cache_secs: 5,
        };
        let app = make_router_with_rl(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
            rl,
        );

        for attempt in 0..5 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .header("x-real-ip", "203.0.113.9")
                        .uri("/capacity")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "read #{attempt} must not be throttled by the provisioning limit",
            );
        }
    }

    /// A live instance may be cached, briefly.
    #[tokio::test]
    async fn alive_check_allows_caching_a_live_instance() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "live-slug", 7, 0, 9020, Status::Done);
        let app = make_router("goopy.life", registry);

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .uri("/goopies/live-slug/alive")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("max-age=5"),
        );
    }

    /// A denial must never be cached.
    ///
    /// This is the property that keeps the `proxy_cache` in the generated site
    /// wake-safe for #96: if nginx could remember a 403, a suspended instance
    /// would never reach gl-serv again and the wake would never fire. Asserted
    /// for all three denial paths, since they are three separate returns.
    #[tokio::test]
    async fn alive_check_never_allows_caching_a_denial() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // Created 10 days ago, lives 7 → expired.
        seed_goopy(&registry, "expired-slug", 7, 10, 9021, Status::Done);
        seed_goopy(&registry, "spawning-slug", 7, 0, 9022, Status::Spawning);
        let app = make_router("goopy.life", registry);

        for slug in ["expired-slug", "spawning-slug", "no-such-slug"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .header("x-real-ip", "127.0.0.1")
                        .uri(format!("/goopies/{slug}/alive"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{slug}");
            assert_eq!(
                resp.headers()
                    .get(axum::http::header::CACHE_CONTROL)
                    .and_then(|v| v.to_str().ok()),
                Some("no-store"),
                "{slug}: a cached denial would strand a suspended instance once \
                 #96 makes this the wake trigger",
            );
        }
    }

    /// The liveness check must not be throttled by the read budget.
    ///
    /// nginx fires one `auth_request` subrequest per HTTP request to an
    /// instance, so a single page costs one token per subresource. With both
    /// on the same bucket, a read burst of 3 would blank a page with 4 assets
    /// — and not with a 429, but with a 500 per asset, because that is what
    /// `auth_request` renders a non-2xx/401/403 as.
    #[tokio::test]
    async fn alive_check_is_not_throttled_by_the_read_budget() {
        let rl = gl_core::config::RateLimitConfig {
            provision_burst: 1,
            provision_period_secs: 60,
            // Deliberately tiny: if the two budgets were still shared, the
            // fourth liveness check below would be refused.
            read_burst: 3,
            read_period_secs: 60,
            alive_burst: 50,
            alive_period_secs: 1,
            alive_cache_secs: 5,
        };
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "busy-slug", 7, 0, 9010, Status::Done);
        let app = make_router_with_rl("goopy.life", registry, rl);

        for attempt in 0..40 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .header("x-real-ip", "203.0.113.42")
                        .uri("/goopies/busy-slug/alive")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "liveness check #{attempt} was throttled; one page load spends \
                 dozens of these and every refusal becomes a 500 at the proxy",
            );
        }
    }

    /// The two budgets are independent in both directions: exhausting the
    /// liveness bucket must not spend the read one.
    #[tokio::test]
    async fn exhausting_the_alive_budget_leaves_reads_working() {
        let rl = gl_core::config::RateLimitConfig {
            provision_burst: 1,
            provision_period_secs: 60,
            read_burst: 10,
            read_period_secs: 60,
            alive_burst: 2,
            alive_period_secs: 60,
            alive_cache_secs: 5,
        };
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "busy-slug", 7, 0, 9011, Status::Done);
        let app = make_router_with_rl("goopy.life", registry, rl);

        let mut throttled = false;
        for _ in 0..6 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .header("x-real-ip", "203.0.113.43")
                        .uri("/goopies/busy-slug/alive")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                throttled = true;
            }
        }
        assert!(
            throttled,
            "alive_burst = 2 should throttle within 6 requests"
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "203.0.113.43")
                    .uri("/goopies/busy-slug")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a flood of liveness checks must not spend the read budget",
        );
    }

    /// A request with no `X-Real-IP` (and no `X-Forwarded-For`) must still be
    /// served: the rate limiter falls back to the TCP peer address, which is
    /// only present because [`serve`] attaches connect info. Exercised over a
    /// real socket, since `oneshot` bypasses the make-service entirely — the
    /// one layer this is testing.
    #[tokio::test]
    async fn read_without_forwarding_headers_falls_back_to_the_peer_address() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { serve(listener, app).await });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /config HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        let status_line = response.lines().next().unwrap_or_default();
        assert!(
            status_line.starts_with("HTTP/1.1 200"),
            "unheadered read should be rate-limited by peer IP, not rejected; got: {response}",
        );
    }

    // ── CORS ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn cors_allowed_origin_sets_acao_header() {
        // test_cfg sets cors_origin = "https://example.com"
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .method("GET")
                    .uri("/config")
                    .header("Origin", "https://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("https://example.com"),
        );
    }

    #[tokio::test]
    async fn cors_disallowed_origin_omits_acao_header() {
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .header("x-real-ip", "127.0.0.1")
                    .method("GET")
                    .uri("/config")
                    .header("Origin", "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get("access-control-allow-origin").is_none(),
            "disallowed origin must not receive ACAO header",
        );
    }

    // ── ManagerService::sweep ─────────────────────────────────────────────

    /// Verifies that `ManagerService::sweep()` is callable via the trait object
    /// and returns zero swept instances when the registry is empty.
    #[test]
    fn manager_service_sweep_empty_registry_returns_zero() {
        let cfg = test_cfg("goopy.life");
        let manager: Arc<dyn ManagerService> = Arc::new(GoopyManager::new(
            GoopyManagerConfig {
                base_dir: cfg.base_dir.clone(),
                domain: cfg.domain.clone(),
                life_in_days: cfg.life_in_days,
                port_range_start: cfg.port_range_start,
                port_range_end: cfg.port_range_end,
                max_active: 100,
                max_provisioned: 100,
                event_retention_days: 30,
            },
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
            NoopProvisioner,
        ));

        let (swept, errors) = manager.sweep().expect("sweep should not fail");
        assert_eq!(swept, 0);
        assert!(errors.is_empty());
    }

    /// Verifies that `ManagerService::sweep()` despawns an expired instance and
    /// leaves a non-expired instance untouched when called via the trait object.
    #[test]
    fn manager_service_sweep_despawns_expired_instance() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        // Expired: created 10 days ago, lives 7 days.
        seed_goopy(&registry, "sweep-expired", 7, 10, 9010, Status::Done);
        // Alive: created now, lives 7 days.
        seed_goopy(&registry, "sweep-alive", 7, 0, 9011, Status::Done);

        let cfg = test_cfg("goopy.life");
        let manager: Arc<dyn ManagerService> = Arc::new(GoopyManager::new(
            GoopyManagerConfig {
                base_dir: cfg.base_dir.clone(),
                domain: cfg.domain.clone(),
                life_in_days: cfg.life_in_days,
                port_range_start: cfg.port_range_start,
                port_range_end: cfg.port_range_end,
                max_active: 100,
                max_provisioned: 100,
                event_retention_days: 30,
            },
            registry,
            NoopProvisioner,
        ));

        let (swept, errors) = manager.sweep().expect("sweep should not fail");
        assert_eq!(swept, 1);
        assert!(errors.is_empty());

        // The alive instance must still be reachable.
        assert!(manager.get("sweep-alive").unwrap().is_some());
        // The expired instance must have been removed — despawn runs on a
        // background thread, so poll until done (mirroring the gl-core test).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while manager.get("sweep-expired").unwrap().is_some() {
            assert!(std::time::Instant::now() < deadline, "despawn timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// A restart must not postpone the sweep by a full interval: the sweeper's
    /// first pass runs at startup. Configured here with a day-long interval, so
    /// the old behaviour (skip the first tick) would hang this test.
    #[tokio::test]
    async fn sweeper_sweeps_once_at_startup() {
        let (manager, mut rx) = CountingManager::with_outcome(SweepOutcome::Ok);

        let sweeper = tokio::spawn(run_sweeper(manager, std::time::Duration::from_secs(86_400)));

        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("the sweeper must sweep at startup, not one interval later")
            .expect("the sweeper should still be running");

        sweeper.abort();
    }

    /// A sweeper that swept once and then stopped would be the same silent
    /// failure #117 is about, one interval later. Time is paused, so this pins
    /// the interval itself rather than a real wall-clock wait.
    #[tokio::test(start_paused = true)]
    async fn sweeper_keeps_sweeping_at_the_configured_interval() {
        let interval = std::time::Duration::from_secs(3_600);
        let (manager, mut rx) = CountingManager::with_outcome(SweepOutcome::Ok);

        let sweeper = tokio::spawn(run_sweeper(manager, interval));

        // Startup tick.
        rx.recv().await.expect("the sweeper must sweep at startup");

        for pass in 1..=3 {
            tokio::time::advance(interval).await;
            rx.recv()
                .await
                .unwrap_or_else(|| panic!("the sweeper must sweep again on tick {pass}"));
        }

        sweeper.abort();
    }

    /// The loop must survive whatever a single sweep does to it. A `sweep` that
    /// returns `Err`, or one that panics inside `spawn_blocking`, is logged and
    /// the next tick still comes — otherwise one bad pass silently ends
    /// reclamation for the life of the process.
    #[tokio::test(start_paused = true)]
    async fn a_failing_or_panicking_sweep_does_not_stop_the_sweeper() {
        let interval = std::time::Duration::from_secs(3_600);

        for outcome in [SweepOutcome::Err, SweepOutcome::Panic] {
            let (manager, mut rx) = CountingManager::with_outcome(outcome);
            let sweeper = tokio::spawn(run_sweeper(manager, interval));

            rx.recv().await.expect("the sweeper must sweep at startup");

            tokio::time::advance(interval).await;
            rx.recv()
                .await
                .unwrap_or_else(|| panic!("a {outcome:?} sweep must not stop the loop"));

            assert!(!sweeper.is_finished(), "the sweeper must still be running");
            sweeper.abort();
        }
    }

    /// What a single `sweep` call does when the sweeper drives it.
    #[derive(Clone, Copy, Debug)]
    enum SweepOutcome {
        Ok,
        Err,
        Panic,
    }

    /// Reports every `sweep` call on a channel before applying `outcome`, so a
    /// test can count passes without waiting on wall-clock time.
    struct CountingManager {
        swept: tokio::sync::mpsc::UnboundedSender<()>,
        outcome: SweepOutcome,
    }

    impl CountingManager {
        /// Returns the manager already behind the trait object `run_sweeper`
        /// takes, plus the receiving end of its sweep counter.
        fn with_outcome(
            outcome: SweepOutcome,
        ) -> (
            Arc<dyn ManagerService>,
            tokio::sync::mpsc::UnboundedReceiver<()>,
        ) {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let manager: Arc<dyn ManagerService> = Arc::new(Self { swept: tx, outcome });
            (manager, rx)
        }
    }

    impl ManagerService for CountingManager {
        fn spawn(&self) -> Result<String, gl_core::Error> {
            unimplemented!("the sweeper never spawns")
        }
        fn get(&self, _slug: &str) -> Result<Option<gl_core::Goopy>, gl_core::Error> {
            unimplemented!("the sweeper never reads a single instance")
        }
        fn sweep(&self) -> Result<(u32, Vec<gl_core::Error>), gl_core::Error> {
            let _ = self.swept.send(());
            match self.outcome {
                SweepOutcome::Ok => Ok((0, Vec::new())),
                SweepOutcome::Err => Err(gl_core::Error::Subprocess("sweep blew up".into())),
                SweepOutcome::Panic => panic!("sweep panicked"),
            }
        }
        fn capacity(&self) -> Result<gl_core::Capacity, gl_core::Error> {
            unimplemented!("the sweeper never reads capacity")
        }
    }

    // ── rate limiting ─────────────────────────────────────────────────────

    /// A tight provision limit (burst = 1) should reject the second back-to-back
    /// spawn from the same IP with 429 and a `Retry-After` header.
    #[tokio::test]
    async fn provision_rate_limit_returns_429_with_retry_after() {
        let rl = gl_core::config::RateLimitConfig {
            provision_burst: 1,
            provision_period_secs: 60,
            read_burst: 100,
            read_period_secs: 1,
            alive_burst: 600,
            alive_period_secs: 1,
            alive_cache_secs: 5,
        };
        let app = make_router_with_rl(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
            rl,
        );

        let make_req = || {
            Request::builder()
                .method("POST")
                .uri("/goopies")
                .header("x-real-ip", "203.0.113.7")
                .body(Body::empty())
                .unwrap()
        };

        // First request from this IP consumes the single burst token.
        let first = app.clone().oneshot(make_req()).await.unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);

        // Second request from the same IP is throttled.
        let second = app.clone().oneshot(make_req()).await.unwrap();
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            second.headers().contains_key("retry-after"),
            "429 response must carry a Retry-After header",
        );
        assert_eq!(
            second.headers().get("content-type").unwrap(),
            "application/json",
        );
        let body = body_json(second.into_body()).await;
        assert_eq!(body["code"], "too_many_requests");
    }

    /// The limit is keyed on the real client IP taken from `X-Real-IP`, so a
    /// request from a different IP is not throttled by another IP's usage.
    #[tokio::test]
    async fn provision_rate_limit_is_per_real_client_ip() {
        let rl = gl_core::config::RateLimitConfig {
            provision_burst: 1,
            provision_period_secs: 60,
            read_burst: 100,
            read_period_secs: 1,
            alive_burst: 600,
            alive_period_secs: 1,
            alive_cache_secs: 5,
        };
        let app = make_router_with_rl(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
            rl,
        );

        let make_req = |ip: &str| {
            Request::builder()
                .method("POST")
                .uri("/goopies")
                .header("x-real-ip", ip)
                .body(Body::empty())
                .unwrap()
        };

        // Exhaust the burst for the first IP.
        let first = app.clone().oneshot(make_req("198.51.100.1")).await.unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);
        let throttled = app.clone().oneshot(make_req("198.51.100.1")).await.unwrap();
        assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);

        // A different IP still has its own fresh burst.
        let other = app.clone().oneshot(make_req("198.51.100.2")).await.unwrap();
        assert_eq!(other.status(), StatusCode::CREATED);
    }

    /// The liveness limit is keyed on the real client IP too.
    ///
    /// This is #142's second cause: `proxy_set_header` does not inherit across
    /// nginx locations, so the `/goopy-alive-check` subrequest forwarded no
    /// client IP and `SmartIpKeyExtractor` fell back to nginx's own peer
    /// address on `127.0.0.1`. Every visitor of every instance on the host
    /// shared one bucket, and one busy page starved all of them. The template
    /// now forwards the headers (see `alive_check_subrequest_forwards_the_client_ip`
    /// in `nginx.rs`); this guards the other half — that the `alive` governor
    /// actually buckets on them.
    #[tokio::test]
    async fn alive_rate_limit_is_per_real_client_ip() {
        let rl = gl_core::config::RateLimitConfig {
            provision_burst: 100,
            provision_period_secs: 1,
            read_burst: 100,
            read_period_secs: 1,
            alive_burst: 1,
            alive_period_secs: 60,
            alive_cache_secs: 5,
        };
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "shared-slug", 7, 0, 9012, Status::Done);
        let app = make_router_with_rl("goopy.life", registry, rl);

        let make_req = |ip: &str| {
            Request::builder()
                .uri("/goopies/shared-slug/alive")
                .header("x-real-ip", ip)
                .body(Body::empty())
                .unwrap()
        };

        // Exhaust the burst for the first visitor.
        let first = app.clone().oneshot(make_req("198.51.100.1")).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let throttled = app.clone().oneshot(make_req("198.51.100.1")).await.unwrap();
        assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);

        // A second visitor of the same instance has an untouched bucket.
        let other = app.clone().oneshot(make_req("198.51.100.2")).await.unwrap();
        assert_eq!(
            other.status(),
            StatusCode::OK,
            "one visitor exhausting the liveness budget must not blank the \
             instance for everyone else on the host",
        );
    }

    // -----------------------------------------------------------------------
    // --check-config
    // -----------------------------------------------------------------------

    const VALID_CONFIG: &str = r#"
base_dir = "/tmp/goopy"
domain = "goopy.life"
life_in_days = 7
port_range_start = 9000
port_range_end = 9100
dev_mode = true
cors_origin = "https://goopy.life"
bind_address = "127.0.0.1:8080"
[registry]
path = "/tmp/goopy-check.db"
[allocator]
kind = "PlainDir"
[provisioner]
kind = "Hello"
"#;

    fn write_config(toml: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        f.write_all(toml.as_bytes()).expect("write config");
        f.flush().expect("flush config");
        f
    }

    #[test]
    fn check_config_accepts_a_valid_config_and_summarises_it() {
        let f = write_config(VALID_CONFIG);
        let (_cfg, summary) = check_config(f.path()).expect("a valid config must check out");

        assert!(summary.contains("is valid"), "summary was: {summary}");
        // The values that decide what the process becomes; an operator reads
        // these to confirm it is the environment they meant to deploy. Every
        // one of them is also a value the gate now validates — a summary that
        // showed a field nothing checks is the trap this PR was fixing.
        for expected in [
            "goopy.life",
            "127.0.0.1:8080",
            "https://goopy.life",
            "Hello",
            "PlainDir",
            "/tmp/goopy-check.db",
        ] {
            assert!(
                summary.contains(expected),
                "summary should mention {expected}, was: {summary}",
            );
        }
    }

    #[test]
    fn check_config_rejects_a_config_missing_a_required_field() {
        // Exactly the shape that took the API down: a file that predates a
        // newly required field still parses as TOML but not as a Config.
        let without_provisioner = VALID_CONFIG.replace("[provisioner]\nkind = \"Hello\"\n", "");
        let f = write_config(&without_provisioner);

        let err = check_config(f.path()).expect_err("a config missing `provisioner` must fail");
        let message = err.to_string();
        assert!(
            message.contains("provisioner"),
            "the error must name the missing field, was: {message}",
        );
    }

    #[test]
    fn check_config_rejects_a_missing_file() {
        let err = check_config(Path::new("/nonexistent/goopy-life/config.toml"))
            .expect_err("a config that is not there must fail");
        assert!(err.to_string().contains("could not read"), "was: {err}",);
    }

    #[test]
    fn check_config_creates_no_registry_file() {
        // The gate runs beside a live gl-serv. Opening the registry here would
        // be a second writer against the running service's database.
        let dir = tempfile::tempdir().expect("tempdir");
        let registry_path = dir.path().join("registry.db");
        let toml = VALID_CONFIG.replace(
            "/tmp/goopy-check.db",
            registry_path.to_str().expect("utf-8 path"),
        );
        let f = write_config(&toml);

        check_config(f.path()).expect("a valid config must check out");

        assert!(
            !registry_path.exists(),
            "--check-config must not open the registry",
        );
    }

    #[test]
    fn check_config_names_the_pool_a_zfs_config_will_use() {
        // `Zfs` alone is the under-reporting the summary exists to avoid: both
        // deploy configs use it, and an operator checking a prod config by hand
        // has nothing else to confirm it is pointed at the pool they meant.
        let toml = VALID_CONFIG.replace(
            "[allocator]\nkind = \"PlainDir\"",
            "[allocator]\nkind = \"Zfs\"\npool = \"zpool_ghost\"\nquota_mb = 512",
        );
        let f = write_config(&toml);
        let (_cfg, summary) = check_config(f.path()).expect("a Zfs config must check out");

        for expected in ["zpool_ghost", "512"] {
            assert!(
                summary.contains(expected),
                "summary should mention {expected}, was: {summary}",
            );
        }
    }

    #[test]
    fn check_config_omits_pool_and_quota_for_a_plaindir_config() {
        // The other half: both keys are ignored under PlainDir, so reporting
        // them would be the opposite error -- a value that looks like it took
        // effect and did not.
        let f = write_config(&VALID_CONFIG.replace(
            "[allocator]\nkind = \"PlainDir\"",
            "[allocator]\nkind = \"PlainDir\"\npool = \"zpool_ignored\"\nquota_mb = 512",
        ));
        let (_cfg, summary) = check_config(f.path()).expect("a PlainDir config must check out");

        assert!(
            !summary.contains("zpool_ignored"),
            "PlainDir ignores pool, so the summary must not imply otherwise: {summary}",
        );
    }

    #[test]
    fn check_config_rejects_an_unusable_cors_origin() {
        // gl-core cannot catch this one: `HeaderValue` is not in its
        // dependency tree. Left to startup it is an exit(1) after the deploy
        // has already swapped the config in.
        let toml = VALID_CONFIG.replace(
            r#"cors_origin = "https://goopy.life""#,
            "cors_origin = \"https://goopy.life\\n\"",
        );
        let f = write_config(&toml);

        let err = check_config(f.path()).expect_err("a newline is not a header value");
        assert!(err.to_string().contains("cors_origin"), "was: {err}");
    }

    #[test]
    fn check_config_rejects_a_config_that_would_crash_loop_the_service() {
        // The regression this whole pair of checks exists for: both of these
        // parse as TOML and as a `Config`'s shape, and both abort `main()`
        // *after* push-binary.sh has installed the binary and swapped the
        // config -- i.e. after the outage has started.
        for (from, to) in [
            (
                r#"bind_address = "127.0.0.1:8080""#,
                r#"bind_address = "0.0.0.0""#,
            ),
            (
                "dev_mode = true",
                "dev_mode = true\nsweep_interval_secs = 0",
            ),
        ] {
            let f = write_config(&VALID_CONFIG.replace(from, to));
            let err = check_config(f.path())
                .expect_err("a config gl-serv cannot start on must not pass the gate");
            assert!(
                err.to_string().contains("bind_address")
                    || err.to_string().contains("sweep_interval_secs"),
                "the error must name the offending key, was: {err}",
            );
        }
    }

    // -----------------------------------------------------------------------
    // --check-config: host paths
    // -----------------------------------------------------------------------

    /// A Ghost config pointed at `source_dir`, with `node_bin` as its node.
    fn ghost_config(source_dir: &std::path::Path, node_bin: &std::path::Path) -> gl_core::Config {
        let toml = VALID_CONFIG.replace(
            "[provisioner]\nkind = \"Hello\"",
            &format!(
                "[provisioner]\nkind = \"Ghost\"\nversion = \"6.63.0\"\n\
                 source_dir = {:?}\nnode_bin = {:?}",
                source_dir, node_bin,
            ),
        );
        let f = write_config(&toml);
        let (cfg, _) = check_config(f.path()).expect("a Ghost config parses");
        cfg
    }

    /// A directory holding every entry the provisioner symlinks, plus an
    /// executable standing in for node.
    fn prepared_ghost_install() -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let source_dir = dir.path().join("ghost-6.63.0");
        std::fs::create_dir(&source_dir).expect("create source_dir");
        for entry in gl_core::goopy_provisioner::ghost_provisioner::SHARED_ENTRIES {
            std::fs::write(source_dir.join(entry), b"").expect("create shared entry");
        }
        let node_bin = dir.path().join("node");
        std::fs::write(&node_bin, b"#!/bin/sh\n").expect("create node");
        std::fs::set_permissions(&node_bin, std::fs::Permissions::from_mode(0o755))
            .expect("chmod node");
        (dir, source_dir)
    }

    #[test]
    fn check_host_paths_accepts_a_prepared_ghost_install() {
        let (dir, source_dir) = prepared_ghost_install();
        let cfg = ghost_config(&source_dir, &dir.path().join("node"));

        check_host_paths(&cfg).expect("a prepared install must pass");
    }

    #[test]
    fn check_host_paths_rejects_a_source_dir_that_is_not_there() {
        // The failure CI structurally cannot see: the config parses, the
        // service starts, `systemctl is-active` is green -- and every
        // POST /goopies fails at provision time.
        let (dir, _) = prepared_ghost_install();
        let cfg = ghost_config(&dir.path().join("ghost-6.63.1"), &dir.path().join("node"));

        let err = check_host_paths(&cfg).expect_err("a typo'd source_dir must fail");
        assert!(err.to_string().contains("source_dir"), "was: {err}");
    }

    #[test]
    fn check_host_paths_rejects_a_source_dir_that_is_not_a_ghost_install() {
        // `is_dir` alone would pass this: the directory exists, it just is not
        // the thing the provisioner links instances into.
        let (dir, source_dir) = prepared_ghost_install();
        std::fs::remove_file(source_dir.join("package.json")).expect("remove package.json");

        let cfg = ghost_config(&source_dir, &dir.path().join("node"));
        let err = check_host_paths(&cfg).expect_err("an unprepared install must fail");
        assert!(err.to_string().contains("package.json"), "was: {err}");
    }

    #[test]
    fn check_host_paths_rejects_a_node_bin_that_is_not_executable() {
        let (dir, source_dir) = prepared_ghost_install();
        let not_node = dir.path().join("node.txt");
        std::fs::write(&not_node, b"").expect("create non-executable");

        let cfg = ghost_config(&source_dir, &not_node);
        let err = check_host_paths(&cfg).expect_err("a non-executable node_bin must fail");
        assert!(err.to_string().contains("node_bin"), "was: {err}");
    }

    #[test]
    fn check_host_paths_ignores_a_hello_config() {
        // Hello has no host-side paths at all, so the check must be a no-op
        // rather than something that has to be kept in step with it.
        let f = write_config(VALID_CONFIG);
        let (cfg, _) = check_config(f.path()).expect("a valid config must check out");

        check_host_paths(&cfg).expect("Hello configures no host paths");
    }

    // -----------------------------------------------------------------------
    // --check-config against the configs this repository commits
    // -----------------------------------------------------------------------

    /// The gl-serv-side counterpart to gl-core's `tests/committed_configs.rs`.
    ///
    /// That test covers everything `Config::from_file` validates. This one
    /// exists for the single leg it cannot reach — `cors_origin`, which is
    /// checked against `HeaderValue` and so lives on this side of the
    /// dependency boundary. Without it, a committed config with an unusable
    /// origin would still reach a droplet before anything objected.
    ///
    /// `check_host_paths` is deliberately *not* called here: its paths exist
    /// only on a droplet, so asserting them in CI would fail every build.
    #[test]
    fn every_committed_config_passes_the_gate() {
        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the repository root is two levels above gl-serv");

        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(repo_root.join("deploy/config"))
            .expect("deploy/config should be readable — did the directory move?")
            .map(|entry| entry.expect("readable directory entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        files.sort();
        // An empty deploy/config would make this vacuously green, which is the
        // exact silence it exists to break.
        assert!(
            !files.is_empty(),
            "no .toml files found in deploy/config — this test would pass by default"
        );
        files.push(repo_root.join("backend/config.local.toml"));

        for path in files {
            if let Err(e) = check_config(&path) {
                panic!("{} does not pass --check-config: {e}", path.display());
            }
        }
    }
}
