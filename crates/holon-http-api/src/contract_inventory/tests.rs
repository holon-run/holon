use super::schema_enum_values;
use serde_json::json;

#[test]
fn extracts_enum_values_from_documented_variant_schema() {
    let schema = json!({
        "oneOf": [
            {"type": "string", "enum": ["queued", "processed"]},
            {"type": "string", "const": "interrupted", "description": "Replay on recovery."}
        ]
    });

    assert_eq!(
        schema_enum_values(&schema, "QueueEntryStatus").expect("extract enum values"),
        vec![&json!("queued"), &json!("processed"), &json!("interrupted")]
    );
}

#[test]
fn rejects_non_enum_variant_schema() {
    let schema = json!({"oneOf": [{"type": "object"}]});

    let error = schema_enum_values(&schema, "QueueEntryStatus")
        .expect_err("reject non-enum variant schema");

    assert_eq!(
        error.to_string(),
        "status contract QueueEntryStatus contains a non-enum variant schema"
    );
}
