use super::*;

#[test]
fn oidc_configuration_requires_https() {
    let config = OidcProviderConfig {
        issuer_url: "http://issuer.example".into(),
        client_id: "holon".into(),
        client_secret_env: None,
        redirect_uri: None,
    };
    assert!(config.validate().is_err());
}

#[test]
fn localhost_redirect_must_use_http() {
    let mut config = OidcProviderConfig {
        issuer_url: "https://issuer.example".into(),
        client_id: "client".into(),
        client_secret_env: None,
        redirect_uri: Some("ftp://localhost/callback".into()),
    };
    assert!(config.validate().is_err());

    config.redirect_uri = Some("http://localhost/callback".into());
    assert!(config.validate().is_ok());
}

#[test]
fn loopback_ip_redirect_may_use_http() {
    let mut config = OidcProviderConfig {
        issuer_url: "https://issuer.example".into(),
        client_id: "client".into(),
        client_secret_env: None,
        redirect_uri: Some("http://127.0.0.1:7879/callback".into()),
    };
    assert!(config.validate().is_ok());

    config.redirect_uri = Some("http://[::1]:7879/callback".into());
    assert!(config.validate().is_ok());

    config.redirect_uri = Some("http://192.168.1.5:7879/callback".into());
    assert!(config.validate().is_err());

    config.redirect_uri = Some("ftp://127.0.0.1:7879/callback".into());
    assert!(config.validate().is_err());
}

#[test]
fn local_auth_is_default_and_valid() {
    assert_eq!(AuthConfig::default().mode, AuthenticationMode::Local);
    assert!(AuthConfig::default().validate().is_ok());
}
