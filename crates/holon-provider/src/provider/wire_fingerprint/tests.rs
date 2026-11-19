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
