use super::*;

#[test]
fn test_spec_builder() {
    let tool_spec = spec(
        "TestTool",
        "A test tool",
        serde_json::json!({"type": "object"}),
    );

    assert_eq!(tool_spec.name, "TestTool");
    assert_eq!(tool_spec.description, "A test tool");
}
