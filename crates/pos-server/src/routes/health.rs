use crate::error::ApiResult;
use axum::extract::State;
use axum::Json;
use serde_json::json;
use std::sync::Arc;

use crate::AppState;

/// GET /healthz — readiness probe. Verifies DB connectivity.
pub async fn healthz(State(state): State<Arc<AppState>>) -> ApiResult<Json<serde_json::Value>> {
    sqlx::query("SELECT 1").execute(&state.pool).await?;
    Ok(Json(json!({ "status": "ok" })))
}
