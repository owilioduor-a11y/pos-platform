pub mod card_acquirer;
pub mod daraja;

use crate::config::Config;
use pos_domain::payment::{PaymentGateway, PaymentReceipt, PaymentRequest};
use pos_domain::{DomainError, DomainResult, PaymentMethod};

/// All configured payment providers for the deployment region.
/// `None` means the provider isn't configured — charges to it fail
/// cleanly instead of panicking.
pub struct GatewaySet {
    pub mpesa: Option<daraja::DarajaClient>,
    pub card: Option<card_acquirer::CardAcquirerClient>,
}

impl GatewaySet {
    pub fn from_config(config: &Config) -> Self {
        let mpesa = if config.daraja_consumer_key.is_empty()
            || config.daraja_consumer_secret.is_empty()
            || config.daraja_passkey.is_empty()
            || config.daraja_shortcode.is_empty()
        {
            None
        } else {
            Some(daraja::DarajaClient::new(config))
        };

        let card = Some(card_acquirer::CardAcquirerClient::new(config));

        Self { mpesa, card }
    }

    /// Pick the gateway for a payment method. Cash has no gateway.
    pub fn for_method(&self, method: PaymentMethod) -> Option<&dyn PaymentGateway> {
        match method {
            PaymentMethod::Mpesa => self.mpesa.as_ref().map(|g| g as &dyn PaymentGateway),
            PaymentMethod::CardTap => self.card.as_ref().map(|g| g as &dyn PaymentGateway),
            PaymentMethod::Cash => None,
        }
    }

    /// Route a charge to the correct provider.
    pub async fn charge(&self, req: &PaymentRequest) -> DomainResult<PaymentReceipt> {
        let gateway = self.for_method(req.method).ok_or_else(|| {
            DomainError::PaymentDeclined(format!("no gateway for {:?}", req.method))
        })?;
        gateway.charge(req).await
    }

    pub fn provider_for(&self, method: PaymentMethod) -> &'static str {
        self.for_method(method)
            .map(PaymentGateway::provider_name)
            .unwrap_or("cash")
    }
}
