use super::*;

fn assert_oauth_auth(auth: &ProviderAuthConfig, profile: &str, client_id: &str) {
    assert_eq!(auth.source, CredentialSource::AuthProfile);
    assert_eq!(auth.kind, CredentialKind::OAuth);
    assert_eq!(auth.profile.as_deref(), Some(profile));
    assert_eq!(auth.client_id.as_deref(), Some(client_id));
}

fn custom_oauth_config() -> HolonConfigFile {
    let mut config = HolonConfigFile::default();
    config.providers.insert(
        ProviderId::parse("example-oauth").unwrap(),
        ProviderConfigFile {
            base_url: "https://example.invalid/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                profile: Some("example-oauth".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    config
}

#[test]
fn named_endpoint_client_id_inherits_default_endpoint_auth() {
    let mut config = custom_oauth_config();
    set_provider_config_key(
        &mut config,
        "providers.example-oauth.auth.client_id",
        "Iv1.root-client",
    )
    .unwrap();
    let id = ProviderId::parse("example-oauth").unwrap();
    let provider = config.providers.get_mut(&id).unwrap();
    let mut default_auth = provider.auth.clone();
    default_auth.profile = Some("work-oauth".into());
    provider
        .endpoints
        .entry(ProviderEndpointId::default_endpoint())
        .or_default()
        .auth = Some(default_auth);
    set_provider_config_key(
        &mut config,
        "providers.example-oauth.endpoints.coding.auth.client_id",
        "Iv1.coding-client",
    )
    .unwrap();
    let endpoint_id = ProviderEndpointId::parse("coding").unwrap();
    assert_oauth_auth(
        config.providers[&id].endpoints[&endpoint_id]
            .auth
            .as_ref()
            .unwrap(),
        "work-oauth",
        "Iv1.coding-client",
    );
}

#[test]
fn endpoint_client_id_preserves_existing_auth() {
    let mut config = custom_oauth_config();
    set_provider_config_key(
        &mut config,
        "providers.example-oauth.auth.client_id",
        "Iv1.root-client",
    )
    .unwrap();
    let id = ProviderId::parse("example-oauth").unwrap();
    let endpoint_id = ProviderEndpointId::parse("coding").unwrap();
    let provider = config.providers.get_mut(&id).unwrap();
    let mut endpoint_auth = provider.auth.clone();
    endpoint_auth.profile = Some("personal-oauth".into());
    provider
        .endpoints
        .entry(endpoint_id.clone())
        .or_default()
        .auth = Some(endpoint_auth);
    set_provider_config_key(
        &mut config,
        "providers.example-oauth.endpoints.coding.auth.client_id",
        "Iv1.updated-client",
    )
    .unwrap();
    assert_oauth_auth(
        config.providers[&id].endpoints[&endpoint_id]
            .auth
            .as_ref()
            .unwrap(),
        "personal-oauth",
        "Iv1.updated-client",
    );
}
