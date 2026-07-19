//! Machine-readable inventories for stable runtime contracts.

use schemars::{generate::SchemaSettings, JsonSchema};
use serde_json::{json, Value};

use crate::{
    tool::spec::ToolResultStatus,
    types::{
        AgentStatus, QueueEntryStatus, TaskStatus, TimerStatus, WaitConditionStatus,
        WorkItemPlanStatus, WorkItemReadiness, WorkItemState,
    },
};

/// Generate the checked-in inventory of stable serialized runtime status enums.
///
/// The Rust enum definitions and their serde attributes remain the source of
/// truth. The generated inventory is snapshot-tested so variant additions,
/// removals, and serialized-name changes require an intentional refresh.
pub fn runtime_status_enum_inventory() -> Value {
    json!({
        "version": 1,
        "source_of_truth": "typed Rust enums deriving schemars::JsonSchema",
        "enums": [
            enum_contract::<AgentStatus>("AgentStatus", "src/types.rs"),
            enum_contract::<WorkItemState>("WorkItemState", "src/domain/work_item.rs"),
            enum_contract::<WorkItemPlanStatus>("WorkItemPlanStatus", "src/domain/work_item.rs"),
            enum_contract::<WorkItemReadiness>("WorkItemReadiness", "src/domain/work_item.rs"),
            enum_contract::<TaskStatus>("TaskStatus", "src/types.rs"),
            enum_contract::<WaitConditionStatus>("WaitConditionStatus", "src/types.rs"),
            enum_contract::<TimerStatus>("TimerStatus", "src/types.rs"),
            enum_contract::<QueueEntryStatus>("QueueEntryStatus", "src/types.rs"),
            enum_contract::<ToolResultStatus>("ToolResultStatus", "src/tool/spec.rs"),
        ],
    })
}

fn enum_contract<T: JsonSchema>(name: &str, source: &str) -> Value {
    json!({
        "name": name,
        "source": source,
        "serialized_values": serialized_enum_values::<T>(name),
    })
}

fn serialized_enum_values<T: JsonSchema>(name: &str) -> Vec<String> {
    let schema = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<T>();
    let schema = serde_json::to_value(schema)
        .unwrap_or_else(|error| panic!("failed to serialize schema for {name}: {error}"));
    schema_enum_values(&schema, name)
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("status contract {name} contains a non-string variant"))
                .to_string()
        })
        .collect()
}

fn schema_enum_values<'a>(schema: &'a Value, name: &str) -> Vec<&'a Value> {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values.iter().collect();
    }

    schema
        .get("oneOf")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("status contract {name} is not a string enum schema"))
        .iter()
        .flat_map(|variant_schema| {
            if let Some(values) = variant_schema.get("enum").and_then(Value::as_array) {
                return values.iter().collect::<Vec<_>>();
            }
            if let Some(value) = variant_schema.get("const") {
                return vec![value];
            }
            panic!("status contract {name} contains a non-enum variant schema")
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
            schema_enum_values(&schema, "QueueEntryStatus"),
            vec![&json!("queued"), &json!("processed"), &json!("interrupted")]
        );
    }
}
