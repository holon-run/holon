use super::CallContext;
use std::collections::BTreeMap;

#[test]
fn call_context_debug_redacts_header_values() {
    let mut headers = BTreeMap::new();
    headers.insert(
        "Authorization".to_string(),
        "Bearer do-not-log-this-token".to_string(),
    );
    let context = CallContext {
        headers,
        ..Default::default()
    };

    let debug = format!("{context:?}");
    assert!(debug.contains("Authorization"));
    assert!(!debug.contains("do-not-log-this-token"));
}
