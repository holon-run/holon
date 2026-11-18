use super::*;

#[tokio::test]
async fn blocks_loopback_by_default() {
    let config = WebFetchConfig::default();
    let url = Url::parse("http://127.0.0.1:3000/").unwrap();
    let error = validate_fetch_url(&url, &config).await.unwrap_err();
    let tool_error = crate::test_tool_error(&error);
    assert_eq!(tool_error.kind, "network_denied");
}

#[tokio::test]
async fn allowlisted_loopback_is_allowed_for_dev_hosts() {
    let mut config = WebFetchConfig::default();
    config.allowed_hosts = vec!["127.0.0.1:3000".into()];
    let url = Url::parse("http://127.0.0.1:3000/").unwrap();
    let decision = validate_fetch_url(&url, &config).await.unwrap();
    assert!(decision.allowed_by_host_rule);
}

#[tokio::test]
async fn allowlisted_ipv6_loopback_with_port_matches() {
    let mut config = WebFetchConfig::default();
    config.allowed_hosts = vec!["[::1]:5173".into()];
    let url = Url::parse("http://[::1]:5173/").unwrap();
    let decision = validate_fetch_url(&url, &config).await.unwrap();
    assert!(decision.allowed_by_host_rule);
}

#[tokio::test]
async fn metadata_endpoint_stays_blocked_when_host_is_allowlisted() {
    let mut config = WebFetchConfig::default();
    config.allowed_hosts = vec!["169.254.169.254".into()];
    let url = Url::parse("http://169.254.169.254/latest/meta-data").unwrap();
    let error = validate_fetch_url(&url, &config).await.unwrap_err();
    let tool_error = crate::test_tool_error(&error);
    assert_eq!(tool_error.kind, "network_denied");
    assert!(tool_error.message.contains("metadata"));
}
