use super::{effective_output_tokens, estimate_context_budget, estimate_input_tokens};
use crate::token_estimate::{
    conservative_image_reserve_tokens, estimate_json_tokens, ANTHROPIC_IMAGE_RESERVE_TOKENS,
    OPENAI_IMAGE_RESERVE_TOKENS,
};
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
    assert_eq!(budget.estimated_text_tokens, budget.estimated_input_tokens);
    assert_eq!(budget.image_reserve_tokens, 0);
    assert_eq!(budget.image_count, 0);
    assert_eq!(budget.inline_image_base64_bytes, 0);
    assert_eq!(budget.estimated_inline_image_decoded_bytes, 0);
    assert_eq!(budget.requested_output_tokens, 2048);
    assert!(budget.required_tokens() < budget.context_window_tokens);
}

#[test]
fn context_budget_reports_inline_and_remote_image_composition() {
    let body = json!({
        "messages": [{"role": "user", "content": [{
            "type": "image",
            "source": {"type": "base64", "media_type": "image/png", "data": "c2VjcmV0"}
        }, {
            "type": "image_url",
            "image_url": {"url": "data:image/png;base64,YQ=="}
        }, {
            "type": "image_url",
            "image_url": {"url": "https://example.com/private-image.png"}
        }, {
            "type": "tool_result",
            "tool_use_id": "view",
            "content": [{
                "type": "image",
                "source": {"type": "base64", "media_type": "image/png", "data": "YWI="}
            }]
        }]}],
        "input": [{"type": "message", "role": "user", "content": [{
            "type": "input_image", "image_url": "data:image/png;base64,YWJj"
        }]}]
    });
    let budget = estimate_context_budget(Some(1_000_000), 1024, &body, &[]).unwrap();

    assert_eq!(budget.image_count, 5);
    assert_eq!(
        budget.image_reserve_tokens,
        2 * ANTHROPIC_IMAGE_RESERVE_TOKENS + 3 * OPENAI_IMAGE_RESERVE_TOKENS
    );
    assert_eq!(budget.inline_image_base64_bytes, 20);
    assert_eq!(budget.estimated_inline_image_decoded_bytes, 12);
    assert!(budget.estimated_text_tokens > 0);
    assert_eq!(
        budget.estimated_input_tokens,
        budget.estimated_text_tokens + budget.image_reserve_tokens
    );
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

#[test]
fn structured_image_payload_size_does_not_become_text_tokens() {
    for body in [
        json!({"messages": [{"role": "user", "content": [{
            "type": "image",
            "source": {"type": "base64", "media_type": "image/png", "data": "AA=="}
        }]}]}),
        json!({"messages": [{"role": "user", "content": [{
            "type": "image_url", "image_url": {"url": "data:image/png;base64,AA=="}
        }]}]}),
        json!({"input": [{"type": "message", "role": "user", "content": [{
            "type": "input_image", "image_url": "data:image/png;base64,AA=="
        }]}]}),
    ] {
        let mut large = body.clone();
        let block = if large.get("messages").is_some() {
            &mut large["messages"][0]["content"][0]
        } else {
            &mut large["input"][0]["content"][0]
        };
        let anthropic = block["type"] == "image";
        if anthropic {
            block["source"]["data"] = Value::String("A".repeat(4_000_000));
        } else if block["image_url"].is_string() {
            block["image_url"] =
                Value::String(format!("data:image/png;base64,{}", "A".repeat(4_000_000)));
        } else {
            block["image_url"]["url"] =
                Value::String(format!("data:image/png;base64,{}", "A".repeat(4_000_000)));
        }
        let estimated = estimate_input_tokens(&body, &[]);
        assert_eq!(estimated, estimate_input_tokens(&large, &[]));
        let reserve = if anthropic {
            ANTHROPIC_IMAGE_RESERVE_TOKENS
        } else {
            OPENAI_IMAGE_RESERVE_TOKENS
        };
        assert!(estimated > reserve, "image envelope still counts as input");
    }
}

#[test]
fn small_images_fit_small_context_windows_for_known_and_unknown_models() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use image::{DynamicImage, ImageFormat};
    use std::io::Cursor;

    let mut png = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(200, 200)
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let data = STANDARD.encode(png.into_inner());
    for (model, image, expected_reserve) in [
        (
            "gpt-4o",
            json!({"type": "input_image", "image_url": format!("data:image/png;base64,{data}")}),
            255,
        ),
        (
            "gpt-4.1-mini",
            json!({"type": "input_image", "image_url": format!("data:image/png;base64,{data}")}),
            80,
        ),
        (
            "compatible-alias",
            json!({"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{data}")}}),
            64,
        ),
        (
            "claude-sonnet-4-6",
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": data}}),
            64,
        ),
        (
            "gpt-4o",
            json!({"type": "image_url", "image_url": {"url": "https://example.invalid/image.png", "detail": "low"}}),
            85,
        ),
        (
            "gpt-4o",
            json!({"type": "input_image", "image_url": "https://example.invalid/image.png", "detail": "low"}),
            85,
        ),
    ] {
        let body = json!({
            "model": model,
            "messages": [{"role": "user", "content": [image]}]
        });
        let budget = estimate_context_budget(Some(8_192), 256, &body, &[]).unwrap();
        assert_eq!(budget.image_reserve_tokens, expected_reserve, "{model}");
        assert_eq!(budget.overflow_tokens(), 0, "{model}");
        assert_eq!(effective_output_tokens(Some(8_192), 256, &body, &[]), 256);
    }
}

#[test]
fn ordinary_text_and_tool_json_are_not_mistaken_for_images() {
    let image_shaped_json = json!({
        "type": "image",
        "source": {"type": "base64", "media_type": "image/png", "data": "A".repeat(40_000)}
    });
    let body = json!({
        "messages": [{"role": "assistant", "content": [{
            "type": "tool_use", "name": "Example", "input": image_shaped_json
        }]}, {"role": "user", "content": [{
            "type": "text",
            "text": format!("data:image/png;base64,{}", "A".repeat(40_000))
        }]}],
        "tools": [{"name": "Example", "input_schema": image_shaped_json}],
        "input": [{"type": "function_call", "arguments": image_shaped_json}]
    });
    assert_eq!(
        estimate_input_tokens(&body, &[]),
        estimate_json_tokens(&body)
    );
    assert!(estimate_input_tokens(&body, &[]) > 30_000);
    let budget = estimate_context_budget(Some(100_000), 1024, &body, &[]).unwrap();
    assert_eq!(budget.estimated_input_tokens, budget.estimated_text_tokens);
    assert_eq!(budget.image_count, 0);
    assert_eq!(budget.image_reserve_tokens, 0);
    assert_eq!(budget.inline_image_base64_bytes, 0);
    assert_eq!(budget.estimated_inline_image_decoded_bytes, 0);
}

#[test]
fn malformed_image_blocks_still_count_their_full_text() {
    let body = json!({"messages": [{"role": "user", "content": [{
        "type": "image",
        "source": {"type": "base64", "media_type": "text/plain", "data": "A".repeat(40_000)}
    }, {
        "type": "input_image", "image_url": "not an image URL".repeat(4000)
    }]}]});
    assert_eq!(
        estimate_input_tokens(&body, &[]),
        estimate_json_tokens(&body)
    );
}

#[test]
fn image_reserves_accumulate_without_discarding_other_block_fields() {
    let image = json!({
        "type": "image",
        "source": {"type": "base64", "media_type": "image/png", "data": "AA=="}
    });
    let mut body = json!({"messages": [{"role": "user", "content": [image, image]}]});
    let before = estimate_input_tokens(&body, &[]);
    assert!(before > 2 * ANTHROPIC_IMAGE_RESERVE_TOKENS);
    body["messages"][0]["content"][0]["caption"] = Value::String("x".repeat(40_000));
    assert!(estimate_input_tokens(&body, &[]) >= before + 10_000);
}

#[test]
fn borrowed_json_count_matches_text_estimator_including_escaping() {
    for body in [
        json!(null),
        json!([true, false, 12.75, -100, {}, []]),
        json!({"text": "\"\\\n\r\t\u{0000}\u{0008}\u{000c}\u{001f}中文🙂"}),
        json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": "normal input"}
            ]}], "tools": [{"description": "schema text"}]}),
    ] {
        assert_eq!(
            estimate_input_tokens(&body, &[]),
            estimate_json_tokens(&body)
        );
    }
}

#[test]
fn gemini_image_payloads_use_reserves_without_losing_text_or_tool_arguments() {
    let image = json!({"inlineData": {"mimeType": "image/png", "data": "AA=="}});
    let mut body = json!({
        "contents": [{"role": "user", "parts": [{"text": "Inspect both"}, image, image]}],
        "generationConfig": {"maxOutputTokens": 8192}
    });
    let before =
        estimate_context_budget(Some(32_768), 512, &body, &[]).expect("known context window");
    assert_eq!(before.image_count, 2);
    assert_eq!(
        before.image_reserve_tokens,
        2 * conservative_image_reserve_tokens()
    );
    assert_eq!(before.inline_image_base64_bytes, 8);
    assert_eq!(before.estimated_inline_image_decoded_bytes, 2);

    body["contents"][0]["parts"][1]["inlineData"]["data"] = Value::String("A".repeat(4_000_000));
    let large = estimate_context_budget(Some(32_768), 512, &body, &[]).unwrap();
    assert_eq!(large.estimated_input_tokens, before.estimated_input_tokens);
    assert_eq!(large.inline_image_base64_bytes, 4_000_004);
    assert_eq!(large.estimated_inline_image_decoded_bytes, 3_000_001);
    assert_eq!(large.overflow_tokens(), 0);

    body["contents"][0]["parts"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "functionCall": {"name": "inspect", "args": {
                "inlineData": {"mimeType": "image/png", "data": "A".repeat(40_000)}
            }}
        }));
    let with_tool = estimate_context_budget(Some(32_768), 512, &body, &[]).unwrap();
    assert_eq!(with_tool.image_count, 2);
    assert!(with_tool.estimated_text_tokens >= large.estimated_text_tokens + 10_000);
}

#[test]
fn gemini_image_lookalikes_outside_wire_image_parts_remain_text() {
    let image = json!({"inlineData": {"mimeType": "image/png", "data": "A".repeat(40_000)}});
    for body in [
        json!({"contents": [{"role": "user", "parts": [{
            "inlineData": {"mimeType": "application/pdf", "data": "A".repeat(40_000)}
        }]}]}),
        json!({"contents": [{"role": "user", "parts": [{
            "inlineData": {"mimeType": "image/png", "data": [1, 2, 3]}
        }]}]}),
        json!({"contents": [{"role": "user", "parts": [{
            "functionResponse": {"name": "read", "response": image}
        }]}]}),
        json!({"tools": [{"contents": [{"role": "user", "parts": [image]}]}]}),
        json!({"contents": [{"role": "tool", "parts": [image]}]}),
    ] {
        assert_eq!(
            estimate_input_tokens(&body, &[]),
            estimate_json_tokens(&body)
        );
    }
}

#[test]
fn gemini_output_control_is_not_input_but_other_generation_fields_are() {
    let body = json!({
        "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
        "generationConfig": {"maxOutputTokens": 65536, "responseMimeType": "text/plain"}
    });
    let mut input_body = body.clone();
    input_body["generationConfig"]
        .as_object_mut()
        .unwrap()
        .remove("maxOutputTokens");
    assert_eq!(
        estimate_input_tokens(&body, &["generationConfig.maxOutputTokens"]),
        estimate_json_tokens(&input_body)
    );
}
