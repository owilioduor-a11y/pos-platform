use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// Authenticated principal injected into protected route handlers.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub merchant_id: Uuid,
    pub role: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    merchant_id: String,
    role: String,
    exp: usize,
    iat: usize,
}

pub fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("argon2 hash failed: {e}")))
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

pub fn issue_token(
    config: &Config,
    user_id: Uuid,
    merchant_id: Uuid,
    role: &str,
) -> ApiResult<String> {
    let now = chrono_like_now();
    let claims = Claims {
        sub: user_id.to_string(),
        merchant_id: merchant_id.to_string(),
        role: role.to_string(),
        iat: now as usize,
        exp: (now + config.jwt_expiry_seconds) as usize,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(|e| ApiError::Internal(e.into()))
}

fn chrono_like_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn decode_token(config: &Config, token: &str) -> ApiResult<AuthUser> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &validation,
    )
    .map_err(|_| ApiError::Unauthorized)?;
    let c = data.claims;
    Ok(AuthUser {
        user_id: c.sub.parse().map_err(|_| ApiError::Unauthorized)?,
        merchant_id: c.merchant_id.parse().map_err(|_| ApiError::Unauthorized)?,
        role: c.role,
    })
}

/// Axum middleware: validates `Authorization: Bearer <jwt>` and inserts
/// `AuthUser` into request extensions for protected routes.
pub async fn require_auth(
    axum::extract::State(config): axum::extract::State<Arc<Config>>,
    mut req: axum::extract::Request,
    next: Next,
) -> ApiResult<Response> {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    let token = header
        .strip_prefix("Bearer ")
        .ok_or(ApiError::Unauthorized)?;
    let user = decode_token(&config, token)?;
    req.extensions_mut().insert(user);
    Ok(next.run(req).await)
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthUser>()
            .cloned()
            .ok_or(ApiError::Unauthorized)
    }
}
