use serde_json::json;

#[test]
fn legacy_context_budget_diagnostics_default_composition_fields() {
    let budget: crate::provider::ProviderContextBudgetDiagnostics = serde_json::from_value(json!({
        "estimated_input_tokens": 10_000,
        "requested_output_tokens": 1,
        "safety_headroom_tokens": 4096,
        "required_tokens": 14_097,
        "context_window_tokens": 8192,
        "overflow_tokens": 5905,
        "suggestion": "reduce input"
    }))
    .unwrap();

    assert_eq!(budget.estimated_input_tokens, 10_000);
    assert_eq!(budget.estimated_text_tokens, 0);
    assert_eq!(budget.image_reserve_tokens, 0);
    assert_eq!(budget.image_count, 0);
    assert_eq!(budget.inline_image_base64_bytes, 0);
    assert_eq!(budget.estimated_inline_image_decoded_bytes, 0);
}
