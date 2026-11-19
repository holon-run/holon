use holon_tools::builtins::memory_search::definition;
use serde_json::json;

#[test]
fn schema_exposes_optional_agent_ids_scope() {
    let definition = definition().unwrap();
    let schema = &definition.spec.input_schema;
    let agent_ids = schema["properties"]["agent_ids"]
        .as_object()
        .expect("agent_ids property");
    // schemars renders Option<Vec<String>> as a nullable array type.
    let type_values = agent_ids["type"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![agent_ids["type"].as_str().unwrap_or_default()]);
    assert!(type_values.contains(&"array"));
    assert!(type_values.contains(&"null"));
    assert_eq!(agent_ids["items"]["type"], "string");
    let required = schema["required"].as_array().expect("required list");
    assert!(!required.contains(&json!("agent_ids")));
}

#[test]
fn schema_keeps_existing_query_contract() {
    let definition = definition().unwrap();
    let schema = &definition.spec.input_schema;
    assert_eq!(schema["properties"]["query"]["type"], "string");
    let required = schema["required"].as_array().expect("required list");
    assert!(required.contains(&json!("query")));
    assert!(!required.contains(&json!("limit")));
    assert!(!required.contains(&json!("include_all_workspaces")));
}
