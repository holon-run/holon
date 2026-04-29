use serde_json::{json, Value};

use crate::{
    auth::load_codex_cli_credential,
    config::{AppConfig, CredentialSource, ModelRef},
};

use super::{build_candidate, classify_provider_error, retry::provider_retry_policy_json};

pub fn provider_doctor(config: &AppConfig) -> Value {
    let providers = config
        .provider_chain()
        .into_iter()
        .map(|model_ref| {
            let availability = provider_availability(config, &model_ref);
            let provider_cfg = config
                .providers
                .get(&model_ref.provider)
                .map(|provider| {
                    json!({
                        "base_url": provider.base_url,
                        "transport": provider.transport.as_str(),
                        "auth": {
                            "source": provider.auth.source.as_str(),
                            "kind": provider.auth.kind.as_str(),
                            "env": provider.auth.env,
                            "profile": provider.auth.profile,
                            "external": provider.auth.external,
                            "credential_configured": provider.has_configured_credential(),
                        },
                    })
                })
                .unwrap_or_else(|| json!({"error": "provider_not_configured"}));
            json!({
                "model": model_ref.as_string(),
                "provider": model_ref.provider.as_str(),
                "settings": provider_cfg,
                "availability": availability,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "default_model": config.default_model.as_string(),
        "fallback_models": config.fallback_models.iter().map(ModelRef::as_string).collect::<Vec<_>>(),
        "disable_provider_fallback": config.provider_fallback_disabled(),
        "runtime_max_output_tokens": config.runtime_max_output_tokens,
        "retry_policy": provider_retry_policy_json(),
        "providers": providers,
    })
}

fn provider_availability(config: &AppConfig, model_ref: &ModelRef) -> Value {
    let mut availability = match build_candidate(config, model_ref) {
        Ok(candidate) => json!({
            "available": true,
            "prompt_capabilities": candidate.provider.prompt_capabilities(),
        }),
        Err(error) => {
            let classification = classify_provider_error(&error);
            let mut availability = json!({
                "available": false,
                "error": error.to_string(),
                "failure_kind": classification.kind.as_str(),
                "disposition": classification.disposition.as_str(),
            });
            if classification.kind == super::retry::ProviderFailureKind::UnsupportedTransport {
                availability["transport_contract"] = json!("streaming_required");
            }
            availability
        }
    };

    if let Some(provider) = config.providers.get(&model_ref.provider) {
        if provider.auth.source != CredentialSource::ExternalCli {
            return availability;
        }
        let Some(codex_home) = provider.codex_home.as_ref() else {
            return availability;
        };
        let credential_result = load_codex_cli_credential(codex_home);
        if let Ok(credential) = credential_result.as_ref() {
            availability["credential"] = json!({
                "source": credential.source,
                "account_id": credential.account_id,
                "expires_at": credential.expires_at,
            });
        }
        if let Err(error) = credential_result {
            availability["credential_error"] = json!(error.to_string());
        }
    }

    availability
}
