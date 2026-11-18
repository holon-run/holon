use holon_tools::builtins::use_workspace::definition;

#[test]
fn use_workspace_schema_exposes_path_and_workspace_id() {
    let spec = definition().unwrap().spec;
    let properties = spec.input_schema["properties"].as_object().unwrap();
    assert!(properties["path"].is_object());
    assert!(properties["workspace_id"].is_object());
    assert!(!properties.contains_key("access_mode"));
}
