use std::time::Duration;

use anyhow::{anyhow, Result};

use super::{AppConfig, ModelRouteRef, ProviderId, ProviderRuntimeConfig, ProviderTransportKind};

pub const DEFAULT_X_SEARCH_MODEL: &str = "grok-4-1-fast";
pub const DEFAULT_X_SEARCH_TIMEOUT_SECONDS: u64 = 60;

#[derive(Debug, Clone)]
pub struct XSearchRuntimeConfig {
    pub provider: ProviderRuntimeConfig,
    pub model: String,
    pub timeout: Duration,
}

impl XSearchRuntimeConfig {
    pub fn from_app_config(config: &AppConfig) -> Result<Option<Self>> {
        if config.stored_config.x_search.enabled == Some(false) {
            return Ok(None);
        }
        let xai_id = ProviderId::parse(ProviderId::XAI)?;
        let Some(provider) = config.providers.get(&xai_id) else {
            return Ok(None);
        };
        if !provider.has_configured_credential() {
            return Ok(None);
        }
        if provider.transport != ProviderTransportKind::OpenAiResponses {
            return Err(anyhow!(
                "x_search requires the xai provider to use openai_responses transport"
            ));
        }

        let model = match config.stored_config.x_search.model.as_deref() {
            Some(model) => {
                let route = ModelRouteRef::parse_compatible(model)?;
                if route.provider != xai_id {
                    return Err(anyhow!("x_search.model must reference the xai provider"));
                }
                if route.endpoint != provider.route_endpoint {
                    return Err(anyhow!(
                        "x_search.model endpoint does not match the active xai provider endpoint"
                    ));
                }
                route.model
            }
            None => DEFAULT_X_SEARCH_MODEL.to_string(),
        };
        let timeout_seconds = config
            .stored_config
            .x_search
            .timeout_seconds
            .unwrap_or(DEFAULT_X_SEARCH_TIMEOUT_SECONDS);
        if timeout_seconds == 0 {
            return Err(anyhow!("x_search.timeout_seconds must be positive"));
        }

        Ok(Some(Self {
            provider: provider.clone(),
            model,
            timeout: Duration::from_secs(timeout_seconds),
        }))
    }
}
