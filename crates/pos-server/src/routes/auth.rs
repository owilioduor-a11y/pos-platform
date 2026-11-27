use crate::auth::{hash_password, issue_token, verify_password, AuthUser};
use crate::error::{ApiError, ApiResult};
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub business_name: String,
    /// "micro" or "retail".
    pub mode: String,
    pub currency_code: Option<String>,
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub merchant_id: String,
    pub user_id: String,
    pub role: String,
}

/// POST /api/v1/auth/register — create merchant + owner account.
pub async fn register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<Json<AuthResponse>> {
    let pool = state.pool.clone();
    let config = &state.config;
    let mode = match req.mode.to_ascii_lowercase().as_str() {
        "micro" => "micro",
        "retail" => "retail",
        other => {
            return Err(ApiError::BadRequest(format!(
                "mode must be 'micro' or 'retail', got '{other}'"
            )))
        }
    };
    if req.business_name.trim().is_empty() {
        return Err(ApiError::BadRequest("business_name required".into()));
    }
    if req.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    if req.password.len() > 128 {
        return Err(ApiError::BadRequest(
            "password must be at most 128 characters".into(),
        ));
    }
    let email = req.email.trim().to_ascii_lowercase();
    if email.is_empty()
        || email.len() > 254
        || email.matches('@').count() != 1
        || email.starts_with('@')
        || email.ends_with('@')
    {
        return Err(ApiError::BadRequest("valid email required".into()));
    }
    let currency = req
        .currency_code
        .clone()
        .unwrap_or_else(|| "KES".into())
        .to_ascii_uppercase();
    if currency.len() != 3 {
        return Err(ApiError::BadRequest(
            "currency_code must be ISO 4217".into(),
        ));
    }

    let merchant_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();
    let password_hash = hash_password(&req.password)?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO merchants (id, business_name, mode, currency_code) VALUES ($1, $2, $3, $4)",
    )
    .bind(merchant_id)
    .bind(req.business_name.trim())
    .bind(mode)
    .bind(&currency)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO users (id, merchant_id, email, password_hash, role) VALUES ($1, $2, $3, $4, 'owner')",
    )
    .bind(user_id)
    .bind(merchant_id)
    .bind(&email)
    .bind(&password_hash)
    .execute(&mut *tx)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::Conflict("email already registered".into())
        }
        other => ApiError::Internal(other.into()),
    })?;
    tx.commit().await?;

    let token = issue_token(config, user_id, merchant_id, "owner")?;
    Ok(Json(AuthResponse {
        token,
        merchant_id: merchant_id.to_string(),
        user_id: user_id.to_string(),
        role: "owner".into(),
    }))
}

/// POST /api/v1/auth/login — exchange credentials for a JWT.
pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Json<AuthResponse>> {
    if req.password.len() > 128 {
        return Err(ApiError::Unauthorized);
    }
    let pool = state.pool.clone();
    let config = &state.config;
    let email = req.email.trim().to_ascii_lowercase();
    let row = sqlx::query_as::<_, (Uuid, Uuid, String, String)>(
        "SELECT id, merchant_id, password_hash, role FROM users WHERE email = $1",
    )
    .bind(&email)
    .fetch_optional(&pool)
    .await?
    .ok_or(ApiError::Unauthorized)?;

    let (user_id, merchant_id, password_hash, role) = row;
    if !verify_password(&password_hash, &req.password) {
        return Err(ApiError::Unauthorized);
    }

    let token = issue_token(config, user_id, merchant_id, &role)?;
    Ok(Json(AuthResponse {
        token,
        merchant_id: merchant_id.to_string(),
        user_id: user_id.to_string(),
        role,
    }))
}

/// GET /api/v1/auth/me — current principal (connectivity check for apps).
pub async fn me(user: AuthUser) -> Json<serde_json::Value> {
    Json(json!({
        "user_id": user.user_id,
        "merchant_id": user.merchant_id,
        "role": user.role,
    }))
}
