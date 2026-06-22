use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use clap::Parser;
use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
use gl_core::{GoopyManager, RealSysRunner};
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
}

impl<R, P> ManagerService for GoopyManager<R, P>
where
    R: gl_core::goopy_registry::GoopyRegistry + Send + Sync + 'static,
    P: gl_core::goopy_provisioner::GoopyProvisioner + Send + Sync + 'static,
{
    fn spawn(&self) -> Result<String, gl_core::Error> {
        GoopyManager::spawn(self).map(|(slug, _, _)| slug)
    }

    fn get(&self, slug: &str) -> Result<Option<gl_core::Goopy>, gl_core::Error> {
        GoopyManager::get(self, slug)
    }

    fn sweep(&self) -> Result<(u32, Vec<gl_core::Error>), gl_core::Error> {
        GoopyManager::sweep(self)
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
}

#[derive(serde::Serialize)]
struct ConfigResponse {
    life_in_days: i32,
    storage_quota_mb: u64,
    domain: String,
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
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message, code) = match self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, "not_found"),
            AppError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg, "invalid"),
            AppError::ServiceUnavailable(msg) => {
                (StatusCode::SERVICE_UNAVAILABLE, msg, "service_unavailable")
            }
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

impl From<gl_core::Error> for AppError {
    fn from(e: gl_core::Error) -> Self {
        match e {
            gl_core::Error::NotFound => AppError::NotFound("not found".into()),
            gl_core::Error::Invalid => AppError::Invalid("invalid".into()),
            gl_core::Error::PortExhausted => {
                AppError::ServiceUnavailable("port range exhausted".into())
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
    .map_err(|e| AppError::Internal(format!("task join error: {e}")))??;

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
    .map_err(|e| AppError::Internal(format!("task join error: {e}")))??;

    let goopy = goopy.ok_or_else(|| AppError::NotFound("not found".into()))?;

    let expires_at = goopy.created_at + Duration::days(goopy.life_in_days as i64);

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
    .map_err(|e| AppError::Internal(format!("task join error: {e}")))??;

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

async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(ConfigResponse {
        life_in_days: state.cfg.life_in_days,
        storage_quota_mb: state.cfg.allocator.quota_mb,
        domain: state.cfg.domain.clone(),
    })
}

// ---------------------------------------------------------------------------
// Router builder (shared by main and tests)
// ---------------------------------------------------------------------------

fn build_router(state: Arc<AppState>, cors: CorsLayer) -> Router {
    Router::new()
        .route("/goopies", post(spawn_goopy))
        .route("/goopies/{slug}", get(get_goopy))
        .route("/goopies/{slug}/alive", get(alive_check))
        .route("/config", get(get_config))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
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

    let app = build_router(state, cors);

    let listener = tokio::net::TcpListener::bind(&bind_address)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("failed to bind to {bind_address}: {e}");
            std::process::exit(1);
        });

    tracing::info!("listening on {bind_address}");
    axum::serve(listener, app).await.unwrap_or_else(|e| {
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
            ssl_email: "test@example.com".to_string(),
            life_in_days: 7,
            provisioner_kind: ProvisionerKind::Hello,
            port_range_start: 9000,
            port_range_end: 9100,
            dev_mode: true,
            cors_origin: "https://example.com".to_string(),
            bind_address: "127.0.0.1:0".to_string(),
            api_port: 3000,
            sweep_interval_secs: 86400,
            registry: gl_core::config::RegistryConfig {
                path: PathBuf::from(":memory:"),
            },
            allocator: gl_core::config::AllocatorConfig {
                kind: gl_core::AllocatorKind::PlainDir,
                pool: String::new(),
                quota_mb: 0,
            },
        }
    }

    /// Build a test router using the given registry (pass pre-seeded registries
    /// for tests that need existing goopies).
    fn make_router(domain: &str, registry: SqliteRegistry) -> Router {
        let cfg = test_cfg(domain);
        let manager: Arc<dyn ManagerService> = Arc::new(GoopyManager::new(
            GoopyManagerConfig {
                base_dir: cfg.base_dir.clone(),
                domain: cfg.domain.clone(),
                ssl_email: cfg.ssl_email.clone(),
                life_in_days: cfg.life_in_days,
                port_range_start: cfg.port_range_start,
                port_range_end: cfg.port_range_end,
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
        build_router(state, cors)
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
                ssl_email: cfg.ssl_email.clone(),
                life_in_days: cfg.life_in_days,
                port_range_start: cfg.port_range_start,
                port_range_end: cfg.port_range_end,
            },
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
            NoopProvisioner,
        ));
        let cors = CorsLayer::new()
            .allow_methods([Method::GET, Method::POST])
            .allow_headers(tower_http::cors::Any);
        let state = Arc::new(AppState { manager, cfg });
        let app = build_router(state, cors);

        let resp = app
            .oneshot(
                Request::builder()
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

    // ── get_goopy ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_goopy_returns_200_with_subdomain_url() {
        let registry = SqliteRegistry::new(Path::new(":memory:")).unwrap();
        seed_goopy(&registry, "happy-little-slug", 7, 0, 9001, Status::Done);
        let app = make_router("goopy.life", registry);

        let resp = app
            .oneshot(
                Request::builder()
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
    async fn get_goopy_unknown_slug_returns_404_with_code() {
        let app = make_router(
            "goopy.life",
            SqliteRegistry::new(Path::new(":memory:")).unwrap(),
        );

        let resp = app
            .oneshot(
                Request::builder()
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
                ssl_email: cfg.ssl_email.clone(),
                life_in_days: cfg.life_in_days,
                port_range_start: cfg.port_range_start,
                port_range_end: cfg.port_range_end,
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
                ssl_email: cfg.ssl_email.clone(),
                life_in_days: cfg.life_in_days,
                port_range_start: cfg.port_range_start,
                port_range_end: cfg.port_range_end,
            },
            registry,
            NoopProvisioner,
        ));

        let (swept, errors) = manager.sweep().expect("sweep should not fail");
        assert_eq!(swept, 1);
        assert!(errors.is_empty());

        // The alive instance must still be reachable.
        assert!(manager.get("sweep-alive").unwrap().is_some());
        // The expired instance must have been removed.
        assert!(manager.get("sweep-expired").unwrap().is_none());
    }
}
