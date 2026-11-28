mod auth;
mod config;
mod error;
mod gateways;
mod routes;

use axum::middleware;
use axum::routing::{get, patch, post};
use axum::Router;
use config::Config;
use gateways::GatewaySet;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::Arc;

/// Shared application state injected into every handler.
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub gateways: GatewaySet,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,pos_server=debug,sqlx=warn".into()),
        )
        .init();

    let config = Arc::new(Config::from_env()?);
    config.validate()?;

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    tracing::info!("database migrations applied");

    let bind_addr = format!("{}:{}", config.host, config.port);
    let state = Arc::new(AppState {
        pool,
        config: config.clone(),
        gateways: GatewaySet::from_config(&config),
    });

    let app = build_router(state, config);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("pos-server listening on {bind_addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn build_router(state: Arc<AppState>, config: Arc<Config>) -> Router {
    // Public endpoints: health, auth registration/login, provider webhooks
    // (webhooks authenticate via signature/payload, not JWT).
    let public = Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/auth/register", post(routes::auth::register))
        .route("/auth/login", post(routes::auth::login))
        .route("/webhooks/daraja", post(routes::payments::daraja_webhook))
        .route(
            "/webhooks/acquirer",
            post(routes::payments::acquirer_webhook),
        );

    // JWT-protected merchant endpoints.
    let protected = Router::new()
        .route("/auth/me", get(routes::auth::me))
        .route(
            "/catalog/products",
            get(routes::catalog::list_products).post(routes::catalog::upsert_product),
        )
        .route(
            "/catalog/products/{id}/stock",
            patch(routes::catalog::update_stock),
        )
        .route("/sync/sales", post(routes::sync::sync_sales))
        .route("/sync/events", post(routes::sync::sync_events))
        .route("/payments/charge", post(routes::payments::charge))
        .route(
            "/payments/mpesa/stkpush",
            post(routes::payments::mpesa_stkpush),
        )
        .route(
            "/payments/mpesa/status/{checkout_request_id}",
            get(routes::payments::mpesa_status),
        )
        .layer(middleware::from_fn_with_state(config, auth::require_auth));

    Router::new()
        .nest("/api/v1", public.merge(protected))
        .with_state(state)
}

async fn shutdown_signal() {
    // A failure to register a handler is logged, not panicked on: losing one
    // signal source must never bring the server down or disable the other.
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl+C handler");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
