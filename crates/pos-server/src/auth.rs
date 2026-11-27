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
    validation.set_required_spec_claims(&["exp", "sub"]);
    validation.validate_exp = true;
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &validation,
    )
    .map_err(|_| ApiError::Unauthorized)?;
    let c = data.claims;
    if c.sub.trim().is_empty() || c.merchant_id.trim().is_empty() || c.role.trim().is_empty() {
        return Err(ApiError::Unauthorized);
    }
    if !matches!(c.role.as_str(), "owner" | "manager" | "cashier") {
        return Err(ApiError::Unauthorized);
    }
    let issued_at = i64::try_from(c.iat).unwrap_or(i64::MAX);
    let expires_at = i64::try_from(c.exp).unwrap_or(i64::MAX);
    let now = chrono_like_now();
    if expires_at <= issued_at || issued_at > now + 60 {
        return Err(ApiError::Unauthorized);
    }
    Ok(AuthUser {
        user_id: c.sub.parse().map_err(|_| ApiError::Unauthorized)?,
        merchant_id: c.merchant_id.parse().map_err(|_| ApiError::Unauthorized)?,
        role: c.role,
    })
}

fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
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
    let token = bearer_token(header).ok_or(ApiError::Unauthorized)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            app_env: "development".into(),
            host: "127.0.0.1".into(),
            port: 8080,
            database_url: "postgres://localhost/pos".into(),
            jwt_secret: "test-secret-at-least-32-chars-long".into(),
            jwt_expiry_seconds: 3600,
            daraja_env: "sandbox".into(),
            daraja_consumer_key: "".into(),
            daraja_consumer_secret: "".into(),
            daraja_passkey: "".into(),
            daraja_shortcode: "".into(),
            daraja_callback_url: "".into(),
            acquirer_provider: "mock".into(),
            acquirer_api_url: "".into(),
            acquirer_api_key: "".into(),
            acquirer_webhook_secret: "".into(),
            etims_enabled: false,
            etims_api_url: "".into(),
            etims_tin: "".into(),
            etims_bhf_id: "".into(),
        }
    }

    #[test]
    fn bearer_scheme_is_case_insensitive() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer abc"), Some("abc"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer   "), None);
    }

    #[test]
    fn issued_token_decodes_to_the_same_principal() {
        let config = test_config();
        let user_id = Uuid::now_v7();
        let merchant_id = Uuid::now_v7();
        let token = issue_token(&config, user_id, merchant_id, "cashier").unwrap();
        let decoded = decode_token(&config, &token).unwrap();
        assert_eq!(decoded.user_id, user_id);
        assert_eq!(decoded.merchant_id, merchant_id);
        assert_eq!(decoded.role, "cashier");
    }

    #[test]
    fn tampered_and_expired_tokens_are_rejected() {
        let config = test_config();
        let token = issue_token(&config, Uuid::now_v7(), Uuid::now_v7(), "owner").unwrap();
        assert!(decode_token(&config, &format!("{token}x")).is_err());

        let mut expired = test_config();
        expired.jwt_expiry_seconds = -1;
        let token = issue_token(&expired, Uuid::now_v7(), Uuid::now_v7(), "owner").unwrap();
        assert!(decode_token(&expired, &token).is_err());
    }

    #[test]
    fn unknown_roles_are_rejected() {
        let config = test_config();
        let token = issue_token(&config, Uuid::now_v7(), Uuid::now_v7(), "superuser").unwrap();
        assert!(decode_token(&config, &token).is_err());
    }
}
