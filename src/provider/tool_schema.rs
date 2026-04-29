use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolSchemaContract {
    Relaxed,
    #[cfg_attr(not(test), allow(dead_code))]
    Strict,
}

pub(crate) fn emitted_tool_json_schema(
    schema: &Value,
    contract: ToolSchemaContract,
) -> Result<Value> {
    let mut schema = schema.clone();
    strip_openai_incompatible_top_level_composition(&mut schema);
    ensure_object_strictness(&mut schema)?;
    if matches!(contract, ToolSchemaContract::Strict) {
        strengthen_strict_tool_schema(&mut schema, false)?;
    }
    Ok(schema)
}

#[cfg(test)]
pub(crate) fn validate_emitted_tool_schema(
    schema: &Value,
    contract: ToolSchemaContract,
) -> Result<()> {
    validate_emitted_tool_schema_inner(schema, "$", contract)
}

fn ensure_object_strictness(schema: &mut Value) -> Result<()> {
    let Some(object) = schema.as_object_mut() else {
        return Ok(());
    };
    if schema_type_names(object.get("type"))
        .iter()
        .any(|schema_type| *schema_type == "object")
    {
        object
            .entry("properties".to_string())
            .or_insert_with(|| Value::Object(Default::default()));
        if !object.contains_key("additionalProperties") {
            object.insert("additionalProperties".to_string(), Value::Bool(false));
        }
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for child in properties.values_mut() {
            ensure_object_strictness(child)?;
        }
    }
    if let Some(items) = object.get_mut("items") {
        ensure_object_strictness(items)?;
    }
    if let Some(any_of) = object.get_mut("anyOf").and_then(Value::as_array_mut) {
        for child in any_of {
            ensure_object_strictness(child)?;
        }
    }
    if let Some(one_of) = object.get_mut("oneOf").and_then(Value::as_array_mut) {
        for child in one_of {
            ensure_object_strictness(child)?;
        }
    }
    Ok(())
}

fn strip_openai_incompatible_top_level_composition(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };

    for key in ["allOf", "anyOf", "oneOf", "enum", "not"] {
        object.remove(key);
    }
}

fn strengthen_strict_tool_schema(schema: &mut Value, nullable: bool) -> Result<()> {
    let Some(object) = schema.as_object_mut() else {
        return Ok(());
    };

    let has_object_type = schema_type_names(object.get("type"))
        .into_iter()
        .any(|schema_type| schema_type == "object");
    let has_array_type = schema_type_names(object.get("type"))
        .into_iter()
        .any(|schema_type| schema_type == "array");
    let original_required = object.get("required").cloned();

    if has_object_type {
        object.insert("additionalProperties".to_string(), Value::Bool(false));
        if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
            let original_required =
                required_property_names(properties, original_required.as_ref())?;
            let property_names = properties.keys().cloned().collect::<Vec<_>>();
            for (name, child) in properties.iter_mut() {
                let child_nullable = !original_required.contains(name);
                strengthen_strict_tool_schema(child, child_nullable)?;
            }
            object.insert(
                "required".to_string(),
                Value::Array(property_names.into_iter().map(Value::String).collect()),
            );
        }
    }

    if has_array_type {
        if let Some(items) = object.get_mut("items") {
            strengthen_strict_tool_schema(items, false)?;
        }
    }

    if let Some(any_of) = object.get_mut("anyOf").and_then(Value::as_array_mut) {
        for variant in any_of {
            strengthen_strict_tool_schema(variant, false)?;
        }
    }
    if let Some(one_of) = object.get_mut("oneOf").and_then(Value::as_array_mut) {
        for variant in one_of {
            strengthen_strict_tool_schema(variant, false)?;
        }
    }

    if nullable {
        make_nullable(object);
    }

    Ok(())
}

fn make_nullable(object: &mut Map<String, Value>) {
    let type_names = schema_type_names(object.get("type"));
    if !type_names.is_empty() {
        let mut normalized = type_names
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if !normalized.iter().any(|schema_type| schema_type == "null") {
            normalized.push("null".to_string());
        }
        object.insert(
            "type".to_string(),
            Value::Array(normalized.into_iter().map(Value::String).collect()),
        );
    }

    if let Some(enum_values) = object.get_mut("enum").and_then(Value::as_array_mut) {
        if !enum_values.iter().any(Value::is_null) {
            enum_values.push(Value::Null);
        }
    }

    if object.contains_key("enum") && object.get("type").is_none() {
        object.insert(
            "type".to_string(),
            Value::Array(vec![Value::String("null".to_string())]),
        );
    }
}

#[cfg(test)]
fn validate_emitted_tool_schema_inner(
    schema: &Value,
    path: &str,
    contract: ToolSchemaContract,
) -> Result<()> {
    let Some(object) = schema.as_object() else {
        return Ok(());
    };

    let schema_types = schema_type_names(object.get("type"));
    if path == "$" {
        if !schema_types
            .iter()
            .any(|schema_type| *schema_type == "object")
        {
            return Err(anyhow!(
                "{path} OpenAI tool schema root must have type object"
            ));
        }
        for key in ["allOf", "anyOf", "oneOf", "enum", "not"] {
            if object.contains_key(key) {
                return Err(anyhow!(
                    "{path} OpenAI tool schema root must not contain {key}"
                ));
            }
        }
    }
    if schema_types
        .iter()
        .any(|schema_type| *schema_type == "object")
    {
        let properties = object
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("{path} object schema must expose properties"))?;
        let required_names = object
            .get("required")
            .map(|required| {
                required
                    .as_array()
                    .ok_or_else(|| anyhow!("{path} required must be an array"))?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| anyhow!("{path} required entries must be strings"))
                            .map(ToString::to_string)
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        for name in &required_names {
            if !properties.contains_key(name) {
                return Err(anyhow!(
                    "{path} required entry `{name}` is missing from properties"
                ));
            }
        }
        let expected_additional_properties = object.get("additionalProperties");
        if expected_additional_properties != Some(&Value::Bool(false)) {
            return Err(anyhow!(
                "{path} object schema must disable additionalProperties"
            ));
        }
        if matches!(contract, ToolSchemaContract::Strict) {
            if object.get("required").is_none() {
                return Err(anyhow!(
                    "{path} strict object schema must expose required array"
                ));
            }
            let property_names = properties.keys().cloned().collect::<Vec<_>>();
            if required_names != property_names {
                return Err(anyhow!(
                    "{path} strict object schema required must match all properties"
                ));
            }
        }
        for (name, child) in properties {
            validate_emitted_tool_schema_inner(
                child,
                &format!("{path}.properties.{name}"),
                contract,
            )?;
        }
    }

    if schema_types
        .iter()
        .any(|schema_type| *schema_type == "array")
    {
        let items = object
            .get("items")
            .ok_or_else(|| anyhow!("{path} array schema must define items"))?;
        validate_emitted_tool_schema_inner(items, &format!("{path}.items"), contract)?;
    }

    if let Some(any_of) = object.get("anyOf").and_then(Value::as_array) {
        for (index, variant) in any_of.iter().enumerate() {
            validate_emitted_tool_schema_inner(
                variant,
                &format!("{path}.anyOf[{index}]"),
                contract,
            )?;
        }
    }
    if let Some(one_of) = object.get("oneOf").and_then(Value::as_array) {
        for (index, variant) in one_of.iter().enumerate() {
            validate_emitted_tool_schema_inner(
                variant,
                &format!("{path}.oneOf[{index}]"),
                contract,
            )?;
        }
    }

    Ok(())
}

fn required_property_names(
    properties: &Map<String, Value>,
    required: Option<&Value>,
) -> Result<Vec<String>> {
    let Some(required) = required else {
        return Ok(Vec::new());
    };
    let required = required
        .as_array()
        .ok_or_else(|| anyhow!("required must be an array"))?;
    required
        .iter()
        .map(|value| {
            let name = value
                .as_str()
                .ok_or_else(|| anyhow!("required entries must be strings"))?;
            if !properties.contains_key(name) {
                return Err(anyhow!(
                    "required entry `{name}` is missing from properties"
                ));
            }
            Ok(name.to_string())
        })
        .collect()
}

fn schema_type_names(value: Option<&Value>) -> Vec<&str> {
    match value {
        Some(Value::String(schema_type)) => vec![schema_type.as_str()],
        Some(Value::Array(types)) => types.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}
