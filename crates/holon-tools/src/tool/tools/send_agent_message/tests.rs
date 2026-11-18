use super::*;

#[test]
fn reply_context_selector_is_optional_without_exposing_routing_identities() {
    let input = serde_json::json!({"agent_id": "peer", "message": "answer"});
    let args: SendAgentMessageArgs = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(args.reply_context_id, None);
    let mut selected = input.clone();
    selected["reply_context_id"] = serde_json::json!("admitted-message");
    let args: SendAgentMessageArgs = serde_json::from_value(selected).unwrap();
    assert_eq!(args.reply_context_id.as_deref(), Some("admitted-message"));
    for forbidden in [
        "sender_agent_id",
        "correlation_id",
        "authority_class",
        "routing_activation_id",
    ] {
        let mut spoofed = input.clone();
        spoofed[forbidden] = serde_json::json!("forged");
        assert!(serde_json::from_value::<SendAgentMessageArgs>(spoofed).is_err());
    }
    let spec = definition().unwrap().spec;
    assert!(spec.input_schema["properties"]
        .get("reply_context_id")
        .is_some());
}
