//! Host-layer eligibility tests for standalone native web search dispatch:
//! an unparseable or unresolvable model route, a route without a native
//! search capability, or a capability of a non-dispatchable kind must fail
//! as `provider_unavailable` before any upstream request is attempted
//! (the fixture providers point at a closed port, so a wrongly attempted
//! request would surface as `network_failed` instead).

use std::sync::Arc;

use crate::config::{AppConfig, ProviderId, RuntimeModelCatalog};
use crate::provider::{build_provider_from_config, ProviderNativeWebSearchKind};
use crate::runtime::bootstrap::ProviderReconfigurator;
use crate::runtime::RuntimeHandle;
use holon_provider::config::ProviderBuiltinWebSearchConfig;

fn dispatch_fixture() -> (tempfile::TempDir, RuntimeHandle) {
    let home = tempfile::tempdir().unwrap();
    let mut config =
        AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf())).unwrap();
    // Route any misguidedly attempted request at a closed port.
    for id in [ProviderId::openai(), ProviderId::anthropic()] {
        let provider = config.providers.get_mut(&id).unwrap();
        provider.base_url = "http://127.0.0.1:1".into();
        provider.credential = Some("native-search-dispatch-test-key".into());
    }
    // The OpenAI route deliberately carries no native search capability.
    config
        .providers
        .get_mut(&ProviderId::openai())
        .unwrap()
        .builtin_web_search = None;
    // The Anthropic route declares a native search kind that has no
    // standalone dispatch in this Holon version.
    config
        .providers
        .get_mut(&ProviderId::anthropic())
        .unwrap()
        .builtin_web_search = Some(ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::Anthropic,
        advertised_tool_type: "web_search".into(),
        backend_kind: "anthropic_web_search".into(),
    });

    let provider = build_provider_from_config(&config).unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        home.path().join("data"),
        home.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider,
        "default".into(),
        crate::runtime::tests::support::context_config(),
    )
    .unwrap();
    let mut snapshot = (*runtime.inner.config_snapshot.load_full()).clone();
    snapshot.user_home_dir = None;
    snapshot.model_catalog = RuntimeModelCatalog::from_config(&config);
    snapshot.provider_reconfig = Some(ProviderReconfigurator { config });
    runtime.inner.config_snapshot.store(Arc::new(snapshot));
    (home, runtime)
}

#[tokio::test]
async fn native_web_search_dispatch_rejects_ineligible_routes_without_requests() {
    let (_home, runtime) = dispatch_fixture();
    let cases = [
        (
            "unparseable route",
            "route without separators",
            "cannot be parsed",
        ),
        (
            "unknown provider route",
            "missing-search-provider@default/gpt-5.4",
            "cannot be resolved",
        ),
        (
            "route without native search capability",
            "openai@default/gpt-5.4",
            "does not declare builtin web search",
        ),
        (
            "capability kind without standalone dispatch",
            "anthropic@default/claude-sonnet-4-6",
            "no standalone dispatch",
        ),
    ];
    for (label, model_route, expected_fragment) in cases {
        let error = runtime
            .dispatch_native_web_search("openai-search", model_route, "query", 5)
            .await
            .err()
            .unwrap_or_else(|| panic!("{label} must be rejected"));
        assert_eq!(error.kind, "provider_unavailable", "{label}: {error:?}");
        assert!(
            error.message.contains(expected_fragment),
            "{label}: unexpected message {}",
            error.message
        );
    }
}

#[tokio::test]
async fn native_web_search_dispatch_requires_host_provider_configuration() {
    let (_home, runtime) = dispatch_fixture();
    let mut snapshot = (*runtime.inner.config_snapshot.load_full()).clone();
    snapshot.provider_reconfig = None;
    runtime.inner.config_snapshot.store(Arc::new(snapshot));

    let error = runtime
        .dispatch_native_web_search("openai-search", "openai@default/gpt-5.4", "query", 5)
        .await
        .err()
        .expect("missing host provider configuration must be rejected");
    assert_eq!(error.kind, "provider_unavailable");
    assert!(error
        .message
        .contains("host provider configuration is unavailable"));
}
