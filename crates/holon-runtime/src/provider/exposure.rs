use holon_domain::model_values::{provider_definition, ProviderSurface};

use crate::config::{AppConfig, ProviderRuntimeConfig};

/// Where a materialized provider entry belongs in the configuration and
/// display surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderExposure {
    /// First-class provider, shown in the default list.
    Primary,
    /// Internal protocol sub-endpoint the user configured explicitly; shown in
    /// the advanced section instead of the default list.
    Advanced,
}

impl ProviderExposure {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Advanced => "advanced",
        }
    }
}

/// Single projection deciding whether a provider entry is exposed.
///
/// Every consumer (runtime config surface, `config providers list`, onboarding)
/// reads this function instead of re-deriving visibility, so the declared
/// [`ProviderSurface`] stays the only source of truth.
///
/// `None` means the entry is routable but not shown until the user configures
/// it explicitly.
pub(crate) fn provider_exposure(
    config: &AppConfig,
    provider: &ProviderRuntimeConfig,
) -> Option<ProviderExposure> {
    match provider_definition(provider.id.as_str()).map(|definition| definition.surface) {
        // Providers without a built-in definition are user-declared.
        None => Some(ProviderExposure::Primary),
        Some(ProviderSurface::Primary) => Some(ProviderExposure::Primary),
        Some(ProviderSurface::SubEndpoint { .. }) => {
            explicitly_configured(config, provider).then_some(ProviderExposure::Advanced)
        }
    }
}

/// A sub-endpoint is exposed only when the user configured it, either through
/// the inheriting `providers.<parent>.endpoints.<endpoint>` form or the legacy
/// flat `providers.<sub-id>` form.
fn explicitly_configured(config: &AppConfig, provider: &ProviderRuntimeConfig) -> bool {
    let stored = &config.stored_config.providers;
    stored.contains_key(&provider.id)
        || stored
            .get(&provider.route_provider)
            .is_some_and(|parent| parent.endpoints.contains_key(&provider.route_endpoint))
}

#[cfg(all(test, holon_lib_test_shard = "c"))]
mod tests {
    use super::{provider_exposure, ProviderExposure};
    use crate::config::{AppConfig, ProviderEndpointId, ProviderId, ProviderRuntimeConfig};
    use tempfile::tempdir;

    fn built_in_config() -> AppConfig {
        let home = tempdir().expect("temp home");
        AppConfig::load_with_home(Some(home.path().to_path_buf())).expect("built-in config")
    }

    fn provider<'a>(config: &'a AppConfig, id: &str) -> &'a ProviderRuntimeConfig {
        config
            .providers
            .get(&ProviderId::parse(id).expect("valid provider id"))
            .expect("built-in provider entry")
    }

    #[test]
    fn primary_providers_stay_exposed() {
        let config = built_in_config();
        assert_eq!(
            provider_exposure(&config, provider(&config, "tencent-tokenhub")),
            Some(ProviderExposure::Primary)
        );
        assert_eq!(
            provider_exposure(&config, provider(&config, "github-copilot")),
            Some(ProviderExposure::Primary)
        );
    }

    #[test]
    fn unconfigured_protocol_sub_endpoints_are_not_exposed() {
        let config = built_in_config();
        for id in [
            "github-copilot-chat-completions",
            "opencode-go-messages",
            "tencent-tokenhub-messages",
            "tencent-tokenhub-responses",
        ] {
            assert_eq!(
                provider_exposure(&config, provider(&config, id)),
                None,
                "{id} must stay hidden until the user configures it"
            );
        }
    }

    #[test]
    fn parent_endpoint_entry_exposes_the_sub_endpoint_as_advanced() {
        let mut config = built_in_config();
        config
            .stored_config
            .providers
            .entry(ProviderId::parse("tencent-tokenhub").expect("valid provider id"))
            .or_default()
            .endpoints
            .insert(
                ProviderEndpointId::parse("messages").expect("valid endpoint"),
                Default::default(),
            );
        assert_eq!(
            provider_exposure(&config, provider(&config, "tencent-tokenhub-messages")),
            Some(ProviderExposure::Advanced)
        );
    }

    #[test]
    fn legacy_flat_entry_exposes_the_sub_endpoint_as_advanced() {
        let mut config = built_in_config();
        config.stored_config.providers.insert(
            ProviderId::parse("tencent-tokenhub-responses").expect("valid provider id"),
            Default::default(),
        );
        assert_eq!(
            provider_exposure(&config, provider(&config, "tencent-tokenhub-responses")),
            Some(ProviderExposure::Advanced)
        );
    }
}
