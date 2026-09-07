use anyhow::{anyhow, Result};
use schemars::{generate::SchemaSettings, JsonSchema, Schema};
use serde_json::Value;

pub(crate) fn tool_input_schema<T: JsonSchema>() -> Result<Value> {
    normalized_schema::<T>()
}

pub(crate) fn tool_result_schema<T: JsonSchema>() -> Result<Value> {
    normalized_schema::<T>()
}

fn normalized_schema<T: JsonSchema>() -> Result<Value> {
    let mut schema = serde_json::to_value(root_schema_for::<T>())
        .map_err(|error| anyhow!("tool schema should serialize: {error}"))?;
    normalize_object_defaults(&mut schema);
    normalize_numeric_bound_literals(&mut schema);
    prune_schema_metadata(&mut schema);
    Ok(schema)
}

#[cfg(test)]
pub(crate) fn validate_source_tool_schema(schema: &Value) -> Result<()> {
    validate_source_tool_schema_inner(schema, "$")
}

fn root_schema_for<T: JsonSchema>() -> Schema {
    SchemaSettings::draft07()
        .with(|settings| {
            settings.inline_subschemas = true;
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

fn normalize_numeric_bound_literals(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };

    for key in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
    ] {
        if let Some(value) = object.get_mut(key) {
            normalize_number_literal(value);
        }
    }

    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for child in properties.values_mut() {
            normalize_numeric_bound_literals(child);
        }
    }
    if let Some(items) = object.get_mut("items") {
        normalize_numeric_bound_literals(items);
    }
    if let Some(any_of) = object.get_mut("anyOf").and_then(Value::as_array_mut) {
        for variant in any_of {
            normalize_numeric_bound_literals(variant);
        }
    }
    if let Some(one_of) = object.get_mut("oneOf").and_then(Value::as_array_mut) {
        for variant in one_of {
            normalize_numeric_bound_literals(variant);
        }
    }
}

fn normalize_number_literal(value: &mut Value) {
    let Some(number) = value.as_f64() else {
        return;
    };
    if let Some(normalized) = serde_json::Number::from_f64(number) {
        *value = Value::Number(normalized);
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
