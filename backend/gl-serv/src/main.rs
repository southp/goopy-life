use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use clap::Parser;
use gl_core::goopy_provisioner::hello_provisioner::HelloProvisioner;
use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
use gl_core::{GoopyManager, RealSysRunner};
use tower_http::cors::CorsLayer;

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
// Shared state
// ---------------------------------------------------------------------------

struct AppState {
    manager: Arc<GoopyManager<SqliteRegistry, HelloProvisioner>>,
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
            AppError::ServiceUnavailable(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg, "service_unavailable"),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg, "internal_error"),
        };
        (status, Json(ErrorResponse { error: message, code: code.into() })).into_response()
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
    let (slug, _port, _job_id) = tokio::task::spawn_blocking({
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

    // Build storage allocator from config
    let storage = cfg.allocator.build();

    let provisioner = HelloProvisioner::new(
        cfg.domain.clone(),
        cfg.dev_mode,
        storage,
        Arc::new(RealSysRunner),
    );

    let registry = SqliteRegistry::new(&cfg.registry.path).unwrap_or_else(|e| {
        tracing::error!("failed to open SQLite registry: {e}");
        std::process::exit(1);
    });

    let manager = Arc::new(GoopyManager::new(
        cfg.base_dir.clone(),
        cfg.domain.clone(),
        cfg.ssl_email.clone(),
        cfg.life_in_days,
        cfg.port_range_start,
        cfg.port_range_end,
        registry,
        provisioner,
    ));

    let cors_origin = cfg.cors_origin.parse::<HeaderValue>().unwrap_or_else(|e| {
        tracing::error!("invalid cors_origin in config: {e}");
        std::process::exit(1);
    });

    let cors = CorsLayer::new()
        .allow_origin(cors_origin)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(tower_http::cors::Any);

    let bind_address = cfg.bind_address.clone();

    let state = Arc::new(AppState { manager, cfg });

    let app = Router::new()
        .route("/goopies", post(spawn_goopy))
        .route("/goopies/{slug}", get(get_goopy))
        .route("/goopies/{slug}/alive", get(alive_check))
        .route("/config", get(get_config))
        .layer(cors)
        .with_state(state);

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
