use anyhow::{Context, Result};

/// Typed environment configuration loaded once at startup.
#[derive(Debug, Clone)]
pub struct Config {
    pub app_env: String,
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub jwt_secret: String,
    pub jwt_expiry_seconds: i64,

    // Daraja (M-Pesa)
    pub daraja_env: String,
    pub daraja_consumer_key: String,
    pub daraja_consumer_secret: String,
    pub daraja_passkey: String,
    pub daraja_shortcode: String,
    pub daraja_callback_url: String,

    // Card acquirer (Kenya partner / global PSP)
    pub acquirer_provider: String,
    pub acquirer_api_url: String,
    pub acquirer_api_key: String,
    pub acquirer_webhook_secret: String,

    // KRA eTIMS (Phase 2)
    pub etims_enabled: bool,
    pub etims_api_url: String,
    pub etims_tin: String,
    pub etims_bhf_id: String,
}

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_required(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let port = env("SERVER_PORT", "8080")
            .parse::<u16>()
            .context("SERVER_PORT must be a valid port")?;
        let jwt_expiry_seconds = env("JWT_EXPIRY_SECONDS", "86400")
            .parse::<i64>()
            .context("JWT_EXPIRY_SECONDS must be an integer")?;
        Ok(Self {
            app_env: env("APP_ENV", "development"),
            host: env("SERVER_HOST", "0.0.0.0"),
            port,
            database_url: env_required("DATABASE_URL")?,
            jwt_secret: env_required("JWT_SECRET")?,
            jwt_expiry_seconds,
            daraja_env: env("DARAJA_ENV", "sandbox"),
            daraja_consumer_key: env("DARAJA_CONSUMER_KEY", ""),
            daraja_consumer_secret: env("DARAJA_CONSUMER_SECRET", ""),
            daraja_passkey: env("DARAJA_PASSKEY", ""),
            daraja_shortcode: env("DARAJA_SHORTCODE", ""),
            daraja_callback_url: env("DARAJA_CALLBACK_URL", ""),
            acquirer_provider: env("KENYA_ACQUIRER_PROVIDER", "mock"),
            acquirer_api_url: env("KENYA_ACQUIRER_API_URL", ""),
            acquirer_api_key: env("KENYA_ACQUIRER_API_KEY", ""),
            acquirer_webhook_secret: env("KENYA_ACQUIRER_WEBHOOK_SECRET", ""),
            etims_enabled: env("ETIMS_ENABLED", "false") == "true",
            etims_api_url: env("ETIMS_API_URL", ""),
            etims_tin: env("ETIMS_TIN", ""),
            etims_bhf_id: env("ETIMS_BHF_ID", ""),
        })
    }

    pub fn is_production(&self) -> bool {
        self.app_env.eq_ignore_ascii_case("production")
    }
}
