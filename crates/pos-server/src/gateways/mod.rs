pub mod card_acquirer;
pub mod daraja;

use crate::config::Config;
use pos_domain::payment::{PaymentGateway, PaymentMethod};

/// Multi-region payment provider router. Selects the gateway for a
/// payment method / merchant region without leaking provider details
/// into routes or domain code.
pub struct Gateways {
    pub daraja: Option<daraja::DarajaClient>,
    pub acquirer: Option<card_acquirer::CardAcquirerClient>,
}

impl Gateways {
    pub fn from_config(config: &Config) -> Self {
        let daraja = if config.daraja_consumer_key.is_empty()
            || config.daraja_consumer_secret.is_empty()
        {
            None
        } else {
            Some(daraja::DarajaClient::new(config))
        };

        let acquirer = if config.kenya_acquirer_api_url.is_empty()
            && config.kenya_acquirer_provider != "mock"
        {
            None
        } else {
            Some(card_acquirer::CardAcquirerClient::new(config))
        };

        Self { daraja, acquirer }
    }

    /// Pick the gateway for a payment method. `None` for cash (no processor).
    pub fn for_method(&self, method: &PaymentMethod) -> Option<&dyn PaymentGateway> {
        match method {
            PaymentMethod::Mpesa => self.daraja.as_ref().map(|g| g as &dyn PaymentGateway),
            PaymentMethod::CardTap => self
                .acquirer
                .as_ref()
                .map(|g| g as &dyn PaymentGateway),
            PaymentMethod::Cash => None,
        }
    }
}
