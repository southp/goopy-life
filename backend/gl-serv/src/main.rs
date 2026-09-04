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
use clap::Parser;
use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
use gl_core::{CapacityKind, GoopyManager, RealSysRunner};
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
#[command(version = "0.1")]
#[command(about = "Goopy.Life API server")]
struct Cli {
    /// Path to the config file
    #[arg(long, default_value = "/opt/goopy-life/config.toml")]
    config: std::path::PathBuf,
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
        return Ok(StatusCode::GONE.into_response());
    };

    let expires_at = goopy.created_at + Duration::days(goopy.life_in_days as i64);
    let alive = goopy.status == gl_core::Status::Done && Utc::now() < expires_at;

    if alive {
        Ok(StatusCode::OK.into_response())
    } else {
        Ok(StatusCode::GONE.into_response())
    }
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

    let spawn_routes = Router::new()
        .route("/goopies", post(spawn_goopy))
        .layer(provision_layer)
        .with_state(Arc::clone(&state));

    let read_routes = Router::new()
        .route("/goopies/{slug}", get(get_goopy))
        .route("/goopies/{slug}/alive", get(alive_check))
        .route("/config", get(get_config))
        .route("/capacity", get(get_capacity))
        .layer(read_layer)
        .with_state(Arc::clone(&state));

    Router::new()
        .merge(spawn_routes)
        .merge(read_routes)
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

    let cli = Cli::parse();

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

    // Spawn the periodic sweep background task.
    {
        let manager = Arc::clone(&state.manager);
        let interval_duration = std::time::Duration::from_secs(sweep_interval_secs);
        assert!(
            !interval_duration.is_zero(),
            "sweep_interval_secs must be > 0 in config.toml"
        );
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(interval_duration);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The first tick fires immediately; skip it so the sweep runs after
            // one full interval has elapsed rather than at startup.
            interval.tick().await;
            loop {
                interval.tick().await;
                let manager = Arc::clone(&manager);
                match tokio::task::spawn_blocking(move || manager.sweep()).await {
                    Ok(Ok((swept, errors))) => {
                        if !errors.is_empty() {
                            tracing::warn!(
                                swept,
                                error_count = errors.len(),
                                "sweep completed with errors"
                            );
                        } else {
                            tracing::info!(swept, "sweep completed");
                        }
                    }
                    Ok(Err(e)) => tracing::error!(error = %e, "sweep failed"),
                    Err(e) => tracing::error!(error = %e, "sweep task panicked"),
                }
            }
        });
    }

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
            sweep_interval_secs: 86400,
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
            provisioner: gl_core::config::ProvisionerConfig {
                kind: ProvisionerKind::Hello,
            },
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
    async fn alive_check_returns_410_for_expired_goopy() {
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

        assert_eq!(resp.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn alive_check_returns_410_for_non_done_status() {
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

        assert_eq!(resp.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn alive_check_returns_410_for_unknown_slug() {
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

        assert_eq!(resp.status(), StatusCode::GONE);
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
}
