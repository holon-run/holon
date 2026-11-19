use super::*;

#[test]
fn provider_headers_reserve_transport_owned_headers_but_allow_user_agent() {
    for name in RESERVED_HEADERS {
        assert!(ProviderHeaders::new([(name.to_uppercase(), "not-disclosed".into())]).is_err());
    }
    assert!(ProviderHeaders::new([("User-Agent".into(), "custom-client".into())]).is_ok());
}
