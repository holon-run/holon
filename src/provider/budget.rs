use serde_json::Value;

use crate::token_estimate::estimate_json_tokens;

const MIN_SAFETY_HEADROOM_TOKENS: usize = 4_096;
const MAX_SAFETY_HEADROOM_TOKENS: usize = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContextBudget {
    pub(crate) context_window_tokens: usize,
    pub(crate) estimated_input_tokens: usize,
    pub(crate) requested_output_tokens: usize,
    pub(crate) safety_headroom_tokens: usize,
}

impl ContextBudget {
    pub(crate) fn required_tokens(self) -> usize {
        self.estimated_input_tokens
            .saturating_add(self.requested_output_tokens)
            .saturating_add(self.safety_headroom_tokens)
    }

    pub(crate) fn overflow_tokens(self) -> usize {
        self.required_tokens()
            .saturating_sub(self.context_window_tokens)
    }
}

pub(crate) fn estimate_context_budget(
    context_window_tokens: Option<usize>,
    requested_output_tokens: u32,
    request_body: &Value,
    output_fields: &[&str],
) -> Option<ContextBudget> {
    let context_window_tokens = context_window_tokens?;
    let estimated_input_tokens = estimate_input_tokens(request_body, output_fields);
    let safety_headroom_tokens = safety_headroom_tokens(context_window_tokens);

    Some(ContextBudget {
        context_window_tokens,
        estimated_input_tokens,
        requested_output_tokens: requested_output_tokens as usize,
        safety_headroom_tokens,
    })
}

pub(crate) fn effective_output_tokens(
    context_window_tokens: Option<usize>,
    configured_output_tokens: u32,
    request_body: &Value,
    output_fields: &[&str],
) -> u32 {
    let Some(context_window_tokens) = context_window_tokens else {
        return configured_output_tokens;
    };

    let input_tokens = estimate_input_tokens(request_body, output_fields);
    let headroom = safety_headroom_tokens(context_window_tokens);
    let available = context_window_tokens.saturating_sub(input_tokens + headroom);
    configured_output_tokens.min(available.max(1) as u32)
}

fn estimate_input_tokens(request_body: &Value, output_fields: &[&str]) -> usize {
    let mut input_body = request_body.clone();
    if let Value::Object(object) = &mut input_body {
        for field in output_fields {
            object.remove(*field);
        }
        // Thinking is part of the requested output budget, not input context.
        object.remove("thinking");
    }
    estimate_json_tokens(&input_body)
}

fn safety_headroom_tokens(context_window_tokens: usize) -> usize {
    (context_window_tokens / 100)
        .max(MIN_SAFETY_HEADROOM_TOKENS)
        .min(MAX_SAFETY_HEADROOM_TOKENS)
}

#[cfg(test)]
mod tests {
    use super::{effective_output_tokens, estimate_context_budget};
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

    #[test]
    fn context_budget_is_unknown_without_a_context_window() {
        let body = json!({"messages": ["short"], "max_tokens": 512});

        assert_eq!(
            estimate_context_budget(None, 512, &body, &["max_tokens"]),
            None
        );
    }

    #[test]
    fn context_budget_counts_final_input_but_not_output_controls() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "(continue)"}]
            }],
            "tools": [{"name": "ExecCommand", "input_schema": {"type": "object"}}],
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "max_tokens": 2048
        });
        let budget = estimate_context_budget(Some(100_000), 2048, &body, &["max_tokens"])
            .expect("context budget should be available");
        let input_body = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "(continue)"}]
            }],
            "tools": [{"name": "ExecCommand", "input_schema": {"type": "object"}}]
        });

        assert_eq!(
            budget.estimated_input_tokens,
            estimate_json_tokens(&input_body)
        );
        assert_eq!(budget.requested_output_tokens, 2048);
        assert!(budget.required_tokens() < budget.context_window_tokens);
    }

    #[test]
    fn context_budget_reports_overflow_with_safety_headroom() {
        let body = json!({"messages": "x".repeat(20_000), "max_tokens": 8192});
        let budget = estimate_context_budget(Some(8_192), 8192, &body, &["max_tokens"])
            .expect("context budget should be available");

        assert!(budget.required_tokens() > budget.context_window_tokens);
        assert_eq!(
            budget.overflow_tokens(),
            budget.required_tokens() - budget.context_window_tokens
        );
    }
}
