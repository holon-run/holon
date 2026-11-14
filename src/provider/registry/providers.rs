#[cfg(test)]
pub(crate) use holon_domain::model_values::ProviderCatalogRegistration;

pub(crate) use holon_domain::model_values::{
    provider_definition, provider_definitions, ModelDiscoveryAuth, ModelDiscoveryDecoder,
    ModelDiscoveryDefinition, ModelDiscoveryRoute, ProviderCatalogPolicy,
    ProviderContextManagement, ProviderDefinition, ProviderMaterializer, ProviderWebSearch,
};

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderTransportKind;

    #[test]
    fn minimax_subscription_credentials_are_region_and_plan_scoped() {
        for (legacy, provider, base_url, credential_env) in [
            (
                "minimax-m-plan",
                "minimax",
                "https://api.minimax.io/anthropic",
                "MINIMAX_M_PLAN_API_KEY",
            ),
            (
                "minimax-cn-m-plan",
                "minimax-cn",
                "https://api.minimax.cn/anthropic",
                "MINIMAX_CN_M_PLAN_API_KEY",
            ),
        ] {
            let definition = provider_definition(legacy).unwrap();
            assert_eq!(definition.route_provider, provider);
            assert_eq!(definition.route_endpoint, "m-plan");
            assert_eq!(definition.default_base_url, base_url);
            assert_eq!(definition.credential_envs, &[credential_env]);
            assert_eq!(
                definition.transport,
                ProviderTransportKind::AnthropicMessages
            );
            assert_eq!(definition.catalog_policy, ProviderCatalogPolicy::StaticOnly);
        }
        let default = provider_definition("minimax").unwrap();
        assert_eq!(default.route_endpoint, "default");
        assert_eq!(default.credential_envs, &["MINIMAX_API_KEY"]);
        let china_default = provider_definition("minimax-cn").unwrap();
        assert_eq!(china_default.route_provider, "minimax-cn");
        assert_eq!(china_default.route_endpoint, "default");
        assert_eq!(
            china_default.default_base_url,
            "https://api.minimax.cn/anthropic"
        );
        assert_eq!(china_default.credential_envs, &["MINIMAX_CN_API_KEY"]);
        assert_eq!(
            china_default.transport,
            ProviderTransportKind::AnthropicMessages
        );
        assert_eq!(
            china_default.catalog_policy,
            ProviderCatalogPolicy::StaticOnly
        );
    }
}
