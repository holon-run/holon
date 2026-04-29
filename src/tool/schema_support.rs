use anyhow::{anyhow, Result};
use schemars::{r#gen::SchemaSettings, schema::RootSchema, JsonSchema};
use serde_json::Value;
use std::any::TypeId;

use crate::tool::tools::spawn_agent::SpawnAgentArgs;

pub(crate) fn tool_input_schema<T: JsonSchema + 'static>() -> Result<Value> {
    let mut schema = serde_json::to_value(root_schema_for::<T>().schema)
        .map_err(|error| anyhow!("tool schema should serialize: {error}"))?;
    normalize_object_defaults(&mut schema);
    prune_schema_metadata(&mut schema);
    if TypeId::of::<T>() == TypeId::of::<SpawnAgentArgs>() {
        enforce_public_named_spawn_contract(&mut schema);
    }
    Ok(schema)
}

fn enforce_public_named_spawn_contract(schema: &mut Value) {
    let Some(schema_object) = schema.as_object_mut() else {
        return;
    };

    let contract = serde_json::json!({
        "allOf": [
            {
                "if": {
                    "properties": {
                        "preset": {
                            "const": "public_named"
                        }
                    },
                    "required": ["preset"]
                },
                "then": {
                    "required": ["agent_id"],
                    "properties": {
                        "workspace_mode": {
                            "enum": ["inherit"]
                        }
                    }
                }
            },
            {
                "if": {
                    "not": {
                        "properties": {
                            "preset": {
                                "const": "public_named"
                            }
                        },
                        "required": ["preset"]
                    }
                },
                "then": {
                    "not": {
                        "required": ["agent_id"]
                    }
                }
            }
        ]
    });

    if let Some(existing_all_of) = schema_object.get_mut("allOf").and_then(Value::as_array_mut) {
        if let Some(contract_variants) = contract.get("allOf").and_then(Value::as_array) {
            existing_all_of.extend(contract_variants.iter().cloned());
        }
        return;
    }

    schema_object.insert("allOf".to_string(), contract["allOf"].clone());
}

#[cfg(test)]
pub(crate) fn validate_source_tool_schema(schema: &Value) -> Result<()> {
    validate_source_tool_schema_inner(schema, "$")
}

fn root_schema_for<T: JsonSchema>() -> RootSchema {
    SchemaSettings::draft07()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.option_add_null_type = false;
        })
        .into_generator()
        .into_root_schema_for::<T>()
}

fn prune_schema_metadata(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };

    object.remove("$schema");
    object.remove("title");
    object.remove("definitions");
    object.remove("$defs");

    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for child in properties.values_mut() {
            prune_schema_metadata(child);
        }
    }
    if let Some(items) = object.get_mut("items") {
        prune_schema_metadata(items);
    }
    if let Some(any_of) = object.get_mut("anyOf").and_then(Value::as_array_mut) {
        for variant in any_of {
            prune_schema_metadata(variant);
        }
    }
    if let Some(one_of) = object.get_mut("oneOf").and_then(Value::as_array_mut) {
        for variant in one_of {
            prune_schema_metadata(variant);
        }
    }
}

fn normalize_object_defaults(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };

    if schema_type_names(object.get("type"))
        .iter()
        .any(|schema_type| *schema_type == "object")
    {
        object
            .entry("properties".to_string())
            .or_insert_with(|| Value::Object(Default::default()));
        object
            .entry("required".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
    }

    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for child in properties.values_mut() {
            normalize_object_defaults(child);
        }
    }
    if let Some(items) = object.get_mut("items") {
        normalize_object_defaults(items);
    }
    if let Some(any_of) = object.get_mut("anyOf").and_then(Value::as_array_mut) {
        for variant in any_of {
            normalize_object_defaults(variant);
        }
    }
    if let Some(one_of) = object.get_mut("oneOf").and_then(Value::as_array_mut) {
        for variant in one_of {
            normalize_object_defaults(variant);
        }
    }
}

#[cfg(test)]
fn validate_source_tool_schema_inner(schema: &Value, path: &str) -> Result<()> {
    let Some(object) = schema.as_object() else {
        return Ok(());
    };

    let schema_types = schema_type_names(object.get("type"));
    if schema_types
        .iter()
        .any(|schema_type| *schema_type == "object")
    {
        let properties = object
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("{path} object schema must expose properties"))?;
        if let Some(required) = object.get("required") {
            let required = required
                .as_array()
                .ok_or_else(|| anyhow!("{path} required must be an array"))?;
            for name in required {
                let Some(name) = name.as_str() else {
                    return Err(anyhow!("{path} required entries must be strings"));
                };
                if !properties.contains_key(name) {
                    return Err(anyhow!(
                        "{path} required entry `{name}` is missing from properties"
                    ));
                }
            }
        }
        for (name, child) in properties {
            validate_source_tool_schema_inner(child, &format!("{path}.properties.{name}"))?;
        }
    }

    if schema_types
        .iter()
        .any(|schema_type| *schema_type == "array")
    {
        let items = object
            .get("items")
            .ok_or_else(|| anyhow!("{path} array schema must define items"))?;
        validate_source_tool_schema_inner(items, &format!("{path}.items"))?;
    }

    if let Some(any_of) = object.get("anyOf").and_then(Value::as_array) {
        for (index, variant) in any_of.iter().enumerate() {
            validate_source_tool_schema_inner(variant, &format!("{path}.anyOf[{index}]"))?;
        }
    }
    if let Some(one_of) = object.get("oneOf").and_then(Value::as_array) {
        for (index, variant) in one_of.iter().enumerate() {
            validate_source_tool_schema_inner(variant, &format!("{path}.oneOf[{index}]"))?;
        }
    }

    Ok(())
}

fn schema_type_names(value: Option<&Value>) -> Vec<&str> {
    match value {
        Some(Value::String(schema_type)) => vec![schema_type.as_str()],
        Some(Value::Array(types)) => types.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}
