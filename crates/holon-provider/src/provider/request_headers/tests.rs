use super::*;

#[test]
fn provider_session_identity_is_stable_opaque_and_namespaced() {
    let session = ProviderSessionId::from_runtime_identity("installation", "agent", 1);
    assert_eq!(
        session,
        ProviderSessionId::from_runtime_identity("installation", "agent", 1)
    );
    assert_eq!(session.as_str().len(), 73);
    assert!(reqwest::header::HeaderValue::from_str(session.as_str()).is_ok());
    for other in [
        ProviderSessionId::from_runtime_identity("other", "agent", 1),
        ProviderSessionId::from_runtime_identity("installation", "other", 1),
        ProviderSessionId::from_runtime_identity("installation", "agent", 2),
        ProviderSessionId::from_runtime_identity("installationa", "gent", 1),
    ] {
        assert_ne!(session, other);
    }
    assert_ne!(
        ProviderSessionId::from_runtime_identity("ab", "c", 1),
        ProviderSessionId::from_runtime_identity("a", "bc", 1)
    );
    assert!(!session.as_str().contains("installation"));
    assert!(!session.as_str().contains("agent"));
}
