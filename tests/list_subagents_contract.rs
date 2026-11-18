use holon_tools::{
    builtins::{list_subagents, BuiltinTool},
    dispatch::ToolRegistry,
    names,
};
use serde_json::json;

#[test]
fn list_subagents_is_the_only_model_facing_spelling_with_bounded_schema() {
    let registry = ToolRegistry::new(std::path::PathBuf::new());
    let specs = registry.tool_specs().unwrap();
    let spec = specs
        .iter()
        .find(|s| s.name == names::LIST_SUBAGENTS)
        .unwrap();
    assert!(!specs.iter().any(|s| s.name == names::LIST_SUBAGENT_CLEANUP));
    assert_eq!(
        spec.input_schema["properties"]["limit"]["minimum"].as_f64(),
        Some(1.0)
    );
    assert_eq!(
        spec.input_schema["properties"]["limit"]["maximum"].as_f64(),
        Some(100.0)
    );
    assert_eq!(spec.input_schema["properties"]["limit"]["default"], 20);
    assert_eq!(spec.input_schema["additionalProperties"], false);
    assert!(spec.input_schema["properties"]["cursor"].is_object());
    assert_eq!(
        spec.input_schema["properties"]["include_cleanup"]["default"],
        false
    );
    assert_eq!(
        registry.family_for_tool(names::LIST_SUBAGENTS).unwrap(),
        registry
            .family_for_tool(names::LIST_SUBAGENT_CLEANUP)
            .unwrap(),
    );
    assert!(registry
        .family_for_tool(names::LIST_SUBAGENTS)
        .unwrap()
        .is_some());
}

#[test]
fn hidden_legacy_dispatch_uses_the_same_query_and_preserves_raw_call_names() {
    assert_eq!(
        BuiltinTool::from_name(names::LIST_SUBAGENTS),
        Some(BuiltinTool::ListSubagents),
    );
    assert_eq!(
        BuiltinTool::from_name(names::LIST_SUBAGENT_CLEANUP),
        Some(BuiltinTool::ListSubagents),
    );
    let old_call = holon_tools::ToolCall {
        id: "historical-call".into(),
        name: names::LIST_SUBAGENT_CLEANUP.into(),
        input: json!({}),
    };
    let read: holon_tools::ToolCall =
        serde_json::from_value(serde_json::to_value(&old_call).unwrap()).unwrap();
    assert_eq!(read.name, "ListSubagentCleanup");
    let args: list_subagents::ListSubagentsArgs = serde_json::from_value(json!({})).unwrap();
    assert_eq!(args.limit, 20);
    assert!(args.cursor.is_none());
    assert!(!args.include_cleanup);
    assert!(serde_json::from_value::<list_subagents::ListSubagentsArgs>(
        json!({"supervisor_agent_id": "foreign"})
    )
    .is_err());
}
