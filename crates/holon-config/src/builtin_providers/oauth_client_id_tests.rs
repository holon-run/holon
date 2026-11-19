use super::*;

fn assert_copilot_oauth_auth(auth: &ProviderAuthConfig, profile: &str, client_id: &str) {
    assert_eq!(auth.source, CredentialSource::AuthProfile);
    assert_eq!(auth.kind, CredentialKind::OAuth);
    assert_eq!(auth.profile.as_deref(), Some(profile));
    assert_eq!(auth.client_id.as_deref(), Some(client_id));
}

#[test]
fn fresh_github_copilot_client_id_preserves_builtin_oauth_auth() {
    let mut config = HolonConfigFile::default();
    set_provider_config_key(
        &mut config,
        "providers.github-copilot.auth.client_id",
        "Iv1.root-client",
    )
    .unwrap();
    let id = ProviderId::parse("github-copilot").unwrap();
    assert_copilot_oauth_auth(
        &config.providers[&id].auth,
        GITHUB_COPILOT_CREDENTIAL_PROFILE,
        "Iv1.root-client",
    );
    let registry =
        resolve_provider_registry(&config, &HashMap::new(), &CredentialStoreFile::default())
            .unwrap();
    assert_copilot_oauth_auth(
        &registry[&id].auth,
        GITHUB_COPILOT_CREDENTIAL_PROFILE,
        "Iv1.root-client",
    );
}

#[test]
fn github_copilot_endpoint_client_id_inherits_oauth_auth() {
    for (endpoint, profile) in [
        ("default", GITHUB_COPILOT_CREDENTIAL_PROFILE),
        ("coding", GITHUB_COPILOT_CREDENTIAL_PROFILE),
        ("default", "copilot-work"),
        ("coding", "copilot-work"),
    ] {
        let mut config = HolonConfigFile::default();
        if profile != GITHUB_COPILOT_CREDENTIAL_PROFILE {
            set_provider_config_key(
                &mut config,
                "providers.github-copilot.auth.profile",
                profile,
            )
            .unwrap();
        }
        set_provider_config_key(
            &mut config,
            &format!("providers.github-copilot.endpoints.{endpoint}.auth.client_id"),
            "Iv1.endpoint-client",
        )
        .unwrap();
        let id = ProviderId::parse("github-copilot").unwrap();
        let endpoint_id = ProviderEndpointId::parse(endpoint).unwrap();
        assert_copilot_oauth_auth(
            config.providers[&id].endpoints[&endpoint_id]
                .auth
                .as_ref()
                .unwrap(),
            profile,
            "Iv1.endpoint-client",
        );
        config.providers.get_mut(&id).unwrap().plans.insert(
            "selected".into(),
            ProviderPlanConfigFile {
                endpoint: Some(endpoint_id),
            },
        );
        let registry =
            resolve_provider_registry(&config, &HashMap::new(), &CredentialStoreFile::default())
                .unwrap();
        assert_copilot_oauth_auth(
            &registry[&ProviderId::parse("github-copilot-selected").unwrap()].auth,
            profile,
            "Iv1.endpoint-client",
        );
    }
}

#[test]
fn named_endpoint_client_id_inherits_default_endpoint_auth() {
    let mut config = HolonConfigFile::default();
    set_provider_config_key(
        &mut config,
        "providers.github-copilot.auth.client_id",
        "Iv1.root-client",
    )
    .unwrap();
    let id = ProviderId::parse("github-copilot").unwrap();
    let provider = config.providers.get_mut(&id).unwrap();
    let mut default_auth = provider.auth.clone();
    default_auth.profile = Some("copilot-work".into());
    provider
        .endpoints
        .entry(ProviderEndpointId::default_endpoint())
        .or_default()
        .auth = Some(default_auth);
    set_provider_config_key(
        &mut config,
        "providers.github-copilot.endpoints.coding.auth.client_id",
        "Iv1.coding-client",
    )
    .unwrap();
    let endpoint_id = ProviderEndpointId::parse("coding").unwrap();
    assert_copilot_oauth_auth(
        config.providers[&id].endpoints[&endpoint_id]
            .auth
            .as_ref()
            .unwrap(),
        "copilot-work",
        "Iv1.coding-client",
    );
}

#[test]
fn endpoint_client_id_preserves_existing_auth() {
    let mut config = HolonConfigFile::default();
    set_provider_config_key(
        &mut config,
        "providers.github-copilot.auth.client_id",
        "Iv1.root-client",
    )
    .unwrap();
    let id = ProviderId::parse("github-copilot").unwrap();
    let endpoint_id = ProviderEndpointId::parse("coding").unwrap();
    let provider = config.providers.get_mut(&id).unwrap();
    let mut endpoint_auth = provider.auth.clone();
    endpoint_auth.profile = Some("copilot-personal".into());
    provider
        .endpoints
        .entry(endpoint_id.clone())
        .or_default()
        .auth = Some(endpoint_auth);
    set_provider_config_key(
        &mut config,
        "providers.github-copilot.endpoints.coding.auth.client_id",
        "Iv1.updated-client",
    )
    .unwrap();
    assert_copilot_oauth_auth(
        config.providers[&id].endpoints[&endpoint_id]
            .auth
            .as_ref()
            .unwrap(),
        "copilot-personal",
        "Iv1.updated-client",
    );
}
