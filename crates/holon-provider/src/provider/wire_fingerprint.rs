use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::{ProviderStablePrefixComponentDiagnostics, ProviderStablePrefixDiagnostics};

const STABLE_PREFIX_SCHEMA_VERSION: u32 = 1;
const HASH_ALGORITHM: &str = "sha256";

pub(crate) fn stable_prefix_diagnostics(
    actual_request: &Value,
    stable_shape: &Value,
    history_prefix: &[Value],
    dynamic_tail_items: usize,
    contract: &Value,
) -> ProviderStablePrefixDiagnostics {
    let request_controls = request_controls(stable_shape);
    let system = stable_shape
        .get("instructions")
        .or_else(|| stable_shape.get("system"))
        .cloned()
        .unwrap_or(Value::Null);
    let tools = stable_shape
        .get("tools")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let history = Value::Array(history_prefix.to_vec());
    let components = vec![
        component("contract", contract, None),
        component("request_controls", &request_controls, None),
        component("system", &system, value_item_count(&system)),
        component("tools", &tools, value_item_count(&tools)),
        component("history_prefix", &history, Some(history_prefix.len())),
    ];
    let stable_identity = Value::Array(
        components
            .iter()
            .map(|component| {
                json!({
                    "name": component.name,
                    "fingerprint": component.fingerprint,
                })
            })
            .collect(),
    );

    ProviderStablePrefixDiagnostics {
        schema_version: STABLE_PREFIX_SCHEMA_VERSION,
        algorithm: HASH_ALGORITHM.to_string(),
        full_request_fingerprint: fingerprint("full_request", actual_request),
        stable_prefix_fingerprint: fingerprint("stable_prefix", &stable_identity),
        history_prefix_items: history_prefix.len(),
        dynamic_tail_items,
        components,
    }
}

fn component(
    name: &str,
    value: &Value,
    item_count: Option<usize>,
) -> ProviderStablePrefixComponentDiagnostics {
    ProviderStablePrefixComponentDiagnostics {
        name: name.to_string(),
        fingerprint: fingerprint(name, value),
        item_count,
    }
}

fn request_controls(stable_shape: &Value) -> Value {
    let mut controls = Map::new();
    if let Some(object) = stable_shape.as_object() {
        for (key, value) in object {
            if !matches!(
                key.as_str(),
                "instructions" | "system" | "tools" | "input" | "messages" | "previous_response_id"
            ) {
                controls.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(controls)
}

fn value_item_count(value: &Value) -> Option<usize> {
    value.as_array().map(Vec::len)
}

fn fingerprint(domain: &str, value: &Value) -> String {
    let canonical = canonical_json(value);
    let payload = format!(
        "holon-provider-stable-prefix:v{STABLE_PREFIX_SCHEMA_VERSION}:{domain}:{canonical}"
    );
    let digest = Sha256::digest(payload.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into()),
        Value::Array(values) => {
            let values = values.iter().map(canonical_json).collect::<Vec<_>>();
            format!("[{}]", values.join(","))
        }
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            let fields = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()),
                        canonical_json(&map[key])
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", fields.join(","))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_fingerprints_ignore_object_insertion_order() {
        let left = json!({"b": 2, "a": {"y": 2, "x": 1}});
        let right = json!({"a": {"x": 1, "y": 2}, "b": 2});

        assert_eq!(
            fingerprint("fixture", &left),
            fingerprint("fixture", &right)
        );
        assert_eq!(
            fingerprint("fixture", &left),
            "5a3ce91ba76ca4af924d0cca6f765671bc3924c2554eea1f9b26ea3ac6b321c2"
        );
    }

    #[test]
    fn dynamic_tail_does_not_change_stable_prefix() {
        let stable_shape = json!({
            "model": "model",
            "instructions": "secret instructions",
            "tools": [{"name": "tool"}],
        });
        let first = json!({
            "model": "model",
            "instructions": "secret instructions",
            "tools": [{"name": "tool"}],
            "input": [{"role": "user", "content": "first"}],
        });
        let second = json!({
            "model": "model",
            "instructions": "secret instructions",
            "tools": [{"name": "tool"}],
            "input": [{"role": "user", "content": "second"}],
        });
        let contract = json!({"transport": "responses", "dialect": "test"});

        let first = stable_prefix_diagnostics(&first, &stable_shape, &[], 1, &contract);
        let second = stable_prefix_diagnostics(&second, &stable_shape, &[], 1, &contract);

        assert_ne!(
            first.full_request_fingerprint,
            second.full_request_fingerprint
        );
        assert_eq!(
            first.stable_prefix_fingerprint,
            second.stable_prefix_fingerprint
        );
        let serialized = serde_json::to_string(&first).unwrap();
        assert!(!serialized.contains("secret instructions"));
        assert!(!serialized.contains("\"content\":\"first\""));
    }

    #[test]
    fn request_diagnostics_remain_compatible_without_stable_prefix() {
        let diagnostics: super::super::ProviderRequestDiagnostics =
            serde_json::from_value(json!({"request_lowering_mode": "legacy"})).unwrap();

        assert!(diagnostics.stable_prefix.is_none());
    }
}
