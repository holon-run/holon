use serde_json::Value;

use crate::token_estimate::estimate_json_tokens;

const MIN_SAFETY_HEADROOM_TOKENS: usize = 4_096;
const MAX_SAFETY_HEADROOM_TOKENS: usize = 32_768;

pub(crate) fn effective_output_tokens(
    context_window_tokens: Option<usize>,
    configured_output_tokens: u32,
    request_body: &Value,
    output_fields: &[&str],
) -> u32 {
    let Some(context_window_tokens) = context_window_tokens else {
        return configured_output_tokens;
    };

    let mut input_body = request_body.clone();
    if let Value::Object(object) = &mut input_body {
        for field in output_fields {
            object.remove(*field);
        }
        object.remove("thinking");
    }
    let input_tokens = estimate_json_tokens(&input_body);
    let headroom = (context_window_tokens / 100)
        .max(MIN_SAFETY_HEADROOM_TOKENS)
        .min(MAX_SAFETY_HEADROOM_TOKENS);
    let available = context_window_tokens.saturating_sub(input_tokens + headroom);
    configured_output_tokens.min(available.max(1) as u32)
}

#[cfg(test)]
mod tests {
    use super::effective_output_tokens;
    use crate::token_estimate::estimate_json_tokens;
    use serde_json::{json, Value};

    fn body_with_estimated_tokens(tokens: usize) -> Value {
        let body = Value::String("x".repeat(tokens * 4 - 2));
        assert_eq!(estimate_json_tokens(&body), tokens);
        body
    }

    #[test]
    fn unknown_context_window_preserves_configured_output() {
        let body = json!({ "messages": ["short"] });

        assert_eq!(
            effective_output_tokens(None, 384_000, &body, &["max_tokens"]),
            384_000
        );
    }

    #[test]
    fn incident_budget_clamps_output_before_provider_send() {
        let body = body_with_estimated_tokens(666_198);

        assert_eq!(
            effective_output_tokens(Some(1_048_576), 384_000, &body, &[]),
            371_893
        );
    }

    #[test]
    fn small_request_keeps_configured_output_limit() {
        let body = body_with_estimated_tokens(10_000);

        assert_eq!(
            effective_output_tokens(Some(1_048_576), 384_000, &body, &[]),
            384_000
        );
    }

    #[test]
    fn near_limit_request_gets_remaining_budget() {
        let body = body_with_estimated_tokens(95_000);

        assert_eq!(
            effective_output_tokens(Some(100_000), 384_000, &body, &[]),
            904
        );
    }
    #[test]
    fn accident_fixture_clamps_output_with_headroom() {
        let input = "x".repeat(666_198 * 4);
        let body = json!({"messages": input, "max_tokens": 384_000});
        let effective = effective_output_tokens(Some(1_048_576), 384_000, &body, &["max_tokens"]);

        assert!(effective < 384_000);
        assert!(666_198 + effective as usize + 10_485 <= 1_048_576);
    }

    #[test]
    fn small_request_preserves_configured_output() {
        let body = json!({"messages": "hello", "max_output_tokens": 256});
        assert_eq!(
            effective_output_tokens(Some(128_000), 256, &body, &["max_output_tokens"]),
            256
        );
    }

    #[test]
    fn missing_context_window_preserves_legacy_behavior() {
        let body = json!({"messages": "hello", "max_tokens": 384_000});
        assert_eq!(
            effective_output_tokens(None, 384_000, &body, &["max_tokens"]),
            384_000
        );
    }
}
