//! Machine-readable inventories for stable runtime contracts.

use anyhow::{bail, Context, Result};
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
pub fn runtime_status_enum_inventory() -> Result<Value> {
    Ok(json!({
        "version": 1,
        "source_of_truth": "typed Rust enums deriving schemars::JsonSchema",
        "enums": [
            enum_contract::<AgentStatus>("AgentStatus", "src/types.rs")?,
            enum_contract::<WorkItemState>("WorkItemState", "src/domain/work_item.rs")?,
            enum_contract::<WorkItemPlanStatus>("WorkItemPlanStatus", "src/domain/work_item.rs")?,
            enum_contract::<WorkItemReadiness>("WorkItemReadiness", "src/domain/work_item.rs")?,
            enum_contract::<TaskStatus>("TaskStatus", "src/types.rs")?,
            enum_contract::<WaitConditionStatus>("WaitConditionStatus", "src/types.rs")?,
            enum_contract::<TimerStatus>("TimerStatus", "src/types.rs")?,
            enum_contract::<QueueEntryStatus>("QueueEntryStatus", "src/types.rs")?,
            enum_contract::<ToolResultStatus>("ToolResultStatus", "src/tool/spec.rs")?,
        ],
    }))
}

fn enum_contract<T: JsonSchema>(name: &str, source: &str) -> Result<Value> {
    Ok(json!({
        "name": name,
        "source": source,
        "serialized_values": serialized_enum_values::<T>(name)?,
    }))
}

fn serialized_enum_values<T: JsonSchema>(name: &str) -> Result<Vec<String>> {
    let schema = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<T>();
    let schema = serde_json::to_value(schema)
        .with_context(|| format!("failed to serialize schema for {name}"))?;
    schema_enum_values(&schema, name)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .with_context(|| format!("status contract {name} contains a non-string variant"))
        })
        .collect()
}

fn schema_enum_values<'a>(schema: &'a Value, name: &str) -> Result<Vec<&'a Value>> {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return Ok(values.iter().collect());
    }

    let variants = schema
        .get("oneOf")
        .and_then(Value::as_array)
        .with_context(|| format!("status contract {name} is not a string enum schema"))?;
    let mut values = Vec::new();
    for variant_schema in variants {
        if let Some(variant_values) = variant_schema.get("enum").and_then(Value::as_array) {
            values.extend(variant_values);
        } else if let Some(value) = variant_schema.get("const") {
            values.push(value);
        } else {
            bail!("status contract {name} contains a non-enum variant schema");
        }
    }
    Ok(values)
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
}
