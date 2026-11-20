use super::*;
use crate::config::{built_in_provider_registry_with_settings, provider_config_view, AppConfig};
use std::collections::HashMap;

#[test]
fn provider_headers_debug_and_provider_views_never_expose_values() {
    let _env = crate::test_env::lock_env();
    let secret = "random-secret-like-header-value";
    let mut runtime = built_in_provider_registry_with_settings(&HashMap::new())
        .unwrap()
        .remove(&ProviderId::openai())
        .unwrap();
    runtime.headers = ProviderHeaders::new([("x-example".into(), secret.into())]).unwrap();
    let home = tempfile::tempdir().unwrap();
    let config =
        AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf())).unwrap();
    let view = provider_config_view(&config, &runtime);
    assert_eq!(view.header_names, vec!["x-example"]);
    assert!(!serde_json::to_string(&view).unwrap().contains(secret));
    assert!(!format!("{runtime:?}").contains(secret));
    assert!(!format!("{:?}", runtime.headers).contains(secret));
}
