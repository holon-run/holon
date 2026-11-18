use super::*;
use serde_json::json;

#[test]
fn web_fetch_provider_is_optional_and_advertised() {
    let spec = definition().unwrap().spec;
    assert!(spec.input_schema["properties"].get("provider").is_some());
    let args: WebFetchArgs = serde_json::from_value(json!({"url": "https://example.com"})).unwrap();
    assert!(args.provider.is_none());
    let args: WebFetchArgs = serde_json::from_value(json!({
        "url": "https://example.com", "provider": "deepseek"
    }))
    .unwrap();
    assert_eq!(args.provider.as_deref(), Some("deepseek"));
}
