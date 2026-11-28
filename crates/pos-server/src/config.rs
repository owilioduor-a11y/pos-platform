use anyhow::{bail, Context, Result};

/// Typed environment configuration loaded once at startup.
#[derive(Clone)]
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
    /// Secret used to tokenize each per-request Daraja callback URL. Daraja
    /// callbacks are unsigned, so the token is the callback authenticator.
    pub daraja_callback_secret: String,

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

fn redacted(value: &str) -> &'static str {
    if value.is_empty() {
        "<unset>"
    } else {
        "<redacted>"
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("app_env", &self.app_env)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database_url", &redacted(&self.database_url))
            .field("jwt_secret", &redacted(&self.jwt_secret))
            .field("jwt_expiry_seconds", &self.jwt_expiry_seconds)
            .field("daraja_env", &self.daraja_env)
            .field("daraja_consumer_key", &redacted(&self.daraja_consumer_key))
            .field(
                "daraja_consumer_secret",
                &redacted(&self.daraja_consumer_secret),
            )
            .field("daraja_passkey", &redacted(&self.daraja_passkey))
            .field("daraja_shortcode", &redacted(&self.daraja_shortcode))
            .field("daraja_callback_url", &redacted(&self.daraja_callback_url))
            .field(
                "daraja_callback_secret",
                &redacted(&self.daraja_callback_secret),
            )
            .field("acquirer_provider", &self.acquirer_provider)
            .field("acquirer_api_url", &redacted(&self.acquirer_api_url))
            .field("acquirer_api_key", &redacted(&self.acquirer_api_key))
            .field(
                "acquirer_webhook_secret",
                &redacted(&self.acquirer_webhook_secret),
            )
            .field("etims_enabled", &self.etims_enabled)
            .field("etims_api_url", &redacted(&self.etims_api_url))
            .field("etims_tin", &redacted(&self.etims_tin))
            .field("etims_bhf_id", &redacted(&self.etims_bhf_id))
            .finish()
    }
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
            daraja_callback_secret: env("DARAJA_CALLBACK_SECRET", ""),
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

    /// Fail closed on settings that would otherwise process real payments
    /// through mocks or issue unusable/misconfigured tokens.
    pub fn validate(&self) -> Result<()> {
        if self.port == 0 {
            bail!("SERVER_PORT must be greater than zero");
        }
        if self.jwt_secret.is_empty() {
            bail!("JWT_SECRET must not be empty");
        }
        if self.jwt_expiry_seconds <= 0 {
            bail!("JWT_EXPIRY_SECONDS must be positive");
        }
        if self.jwt_expiry_seconds > 30 * 24 * 3600 {
            bail!("JWT_EXPIRY_SECONDS must not exceed 30 days");
        }

        if self.is_production() {
            if self.jwt_secret.len() < 32 {
                bail!("JWT_SECRET must be at least 32 chars in production");
            }
            if self.acquirer_provider.eq_ignore_ascii_case("mock")
                || self.acquirer_api_url.is_empty()
                || self.acquirer_api_key.is_empty()
            {
                bail!("production requires a configured card acquirer; mock mode is disabled");
            }
            if self.acquirer_webhook_secret.is_empty() {
                bail!("production requires KENYA_ACQUIRER_WEBHOOK_SECRET");
            }

            let mpesa_configured = [
                &self.daraja_consumer_key,
                &self.daraja_consumer_secret,
                &self.daraja_passkey,
                &self.daraja_shortcode,
            ]
            .iter()
            .any(|value| !value.is_empty());
            if mpesa_configured {
                if !self.daraja_env.eq_ignore_ascii_case("production") {
                    bail!("DARAJA_ENV must be production when M-Pesa credentials are configured");
                }
                if !self.daraja_callback_url.starts_with("https://") {
                    bail!("DARAJA_CALLBACK_URL must be an https URL in production");
                }
                if self.daraja_callback_secret.is_empty() {
                    bail!("production requires DARAJA_CALLBACK_SECRET");
                }
                if self.daraja_callback_secret.len() < 32 {
                    bail!("DARAJA_CALLBACK_SECRET must be at least 32 chars in production");
                }
            }
        }
        Ok(())
    }

    pub fn is_production(&self) -> bool {
        self.app_env.eq_ignore_ascii_case("production")
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
            database_url: "postgres://user:db_password@localhost/pos".into(),
            jwt_secret: "dev-secret".into(),
            jwt_expiry_seconds: 86400,
            daraja_env: "sandbox".into(),
            daraja_consumer_key: "mpesa_key".into(),
            daraja_consumer_secret: "mpesa_secret".into(),
            daraja_passkey: "mpesa_passkey".into(),
            daraja_shortcode: "174379".into(),
            daraja_callback_url: "http://localhost/callback".into(),
            daraja_callback_secret: "dev-callback-secret".into(),
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

    fn production_config() -> Config {
        let mut config = test_config();
        config.app_env = "production".into();
        config.jwt_secret = "x".repeat(32);
        config.acquirer_provider = "partner".into();
        config.acquirer_api_url = "https://acquirer.example".into();
        config.acquirer_api_key = "acquirer_key".into();
        config.acquirer_webhook_secret = "webhook_secret".into();
        config.daraja_env = "production".into();
        config.daraja_callback_url = "https://pos.example/callback".into();
        config.daraja_callback_secret = "x".repeat(32);
        config
    }

    #[test]
    fn development_allows_mock_acquirer() {
        assert!(test_config().validate().is_ok());
    }

    #[test]
    fn production_rejects_mock_acquirer() {
        let mut config = production_config();
        config.acquirer_provider = "mock".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn production_rejects_sandbox_mpesa() {
        let mut config = production_config();
        config.daraja_env = "sandbox".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn production_requires_a_strong_daraja_callback_secret() {
        let mut config = production_config();
        config.daraja_callback_secret.clear();
        assert!(config.validate().is_err());

        let mut config = production_config();
        config.daraja_callback_secret = "short".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn production_rejects_short_jwt_secret() {
        let mut config = production_config();
        config.jwt_secret = "short".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let rendered = format!("{:?}", test_config());
        assert!(!rendered.contains("db_password"));
        assert!(!rendered.contains("dev-secret"));
        assert!(!rendered.contains("mpesa_secret"));
        assert!(!rendered.contains("dev-callback-secret"));
        assert!(!rendered.contains("acquirer_key"));
        assert!(rendered.contains("<redacted>"));
    }
}
