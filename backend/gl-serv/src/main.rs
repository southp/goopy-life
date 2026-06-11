use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use clap::Parser;
use gl_core::goopy_provisioner::hello_provisioner::HelloProvisioner;
use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
use gl_core::{GoopyManager, PlainDirAllocator, StorageAllocator, ZfsAllocator};
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

type AppManager = GoopyManager<SqliteRegistry, HelloProvisioner>;

struct AppState {
    manager: Mutex<AppManager>,
    domain: String,
    life_in_days: i32,
    storage_quota_mb: u64,
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
        let (status, message) = match self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::ServiceUnavailable(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };
        (status, Json(ErrorResponse { error: message })).into_response()
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
        move || {
            let mut manager = state.manager.lock().unwrap();
            manager.spawn()
        }
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
    let domain = state.domain.clone();
    let life_in_days = state.life_in_days;

    let goopy = tokio::task::spawn_blocking({
        let state = Arc::clone(&state);
        move || {
            let manager = state.manager.lock().unwrap();
            manager.get(&slug)
        }
    })
    .await
    .map_err(|e| AppError::Internal(format!("task join error: {e}")))??;

    let goopy = goopy.ok_or_else(|| AppError::NotFound("not found".into()))?;

    let expires_at = goopy.created_at + Duration::days(life_in_days as i64);

    let url = format!("https://{}.{}", goopy.slug, domain);

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
    let life_in_days = state.life_in_days;

    let goopy = tokio::task::spawn_blocking({
        let state = Arc::clone(&state);
        move || {
            let manager = state.manager.lock().unwrap();
            manager.get(&slug)
        }
    })
    .await
    .map_err(|e| AppError::Internal(format!("task join error: {e}")))??;

    let Some(goopy) = goopy else {
        return Ok(StatusCode::GONE.into_response());
    };

    let expires_at = goopy.created_at + Duration::days(life_in_days as i64);
    let alive = goopy.status == gl_core::Status::Done && Utc::now() < expires_at;

    if alive {
        Ok(StatusCode::OK.into_response())
    } else {
        Ok(StatusCode::GONE.into_response())
    }
}

async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(ConfigResponse {
        life_in_days: state.life_in_days,
        storage_quota_mb: state.storage_quota_mb,
        domain: state.domain.clone(),
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

    // Build storage allocator
    let storage: Box<dyn StorageAllocator> = if cfg.dev_mode {
        Box::new(PlainDirAllocator)
    } else {
        Box::new(ZfsAllocator::new(
            cfg.allocator.pool.clone(),
            cfg.allocator.quota_mb,
        ))
    };

    let provisioner = HelloProvisioner::new(cfg.domain.clone(), cfg.dev_mode, storage);

    let registry = SqliteRegistry::new(&cfg.registry.path).expect("failed to open SQLite registry");

    let manager = GoopyManager::new(
        cfg.base_dir.clone(),
        cfg.domain.clone(),
        cfg.ssl_email.clone(),
        cfg.life_in_days,
        cfg.port_range_start,
        cfg.port_range_end,
        registry,
        provisioner,
    );

    let state = Arc::new(AppState {
        manager: Mutex::new(manager),
        domain: cfg.domain.clone(),
        life_in_days: cfg.life_in_days,
        storage_quota_mb: cfg.allocator.quota_mb,
    });

    // CORS
    let cors = CorsLayer::new()
        .allow_origin(
            cfg.cors_origin
                .parse::<HeaderValue>()
                .expect("invalid cors_origin value"),
        )
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(tower_http::cors::Any);

    let app = Router::new()
        .route("/goopies", post(spawn_goopy))
        .route("/goopies/{slug}", get(get_goopy))
        .route("/goopies/{slug}/alive", get(alive_check))
        .route("/config", get(get_config))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_address)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("failed to bind to {}: {e}", cfg.bind_address);
            std::process::exit(1);
        });

    tracing::info!("listening on {}", cfg.bind_address);
    axum::serve(listener, app).await.unwrap();
}
