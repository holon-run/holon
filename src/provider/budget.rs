use serde_json::Value;

use crate::token_estimate::{
    anthropic_image_tokens, conservative_image_reserve_tokens, inline_image_dimensions,
    openai_image_tokens,
};

const MIN_SAFETY_HEADROOM_TOKENS: usize = 4_096;
const MAX_SAFETY_HEADROOM_TOKENS: usize = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContextBudget {
    pub(crate) context_window_tokens: usize,
    pub(crate) estimated_input_tokens: usize,
    pub(crate) estimated_text_tokens: usize,
    pub(crate) image_reserve_tokens: usize,
    pub(crate) image_count: usize,
    pub(crate) inline_image_base64_bytes: usize,
    pub(crate) estimated_inline_image_decoded_bytes: usize,
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
    let input = estimate_input(request_body, output_fields);
    let safety_headroom_tokens = safety_headroom_tokens(context_window_tokens);

    Some(ContextBudget {
        context_window_tokens,
        estimated_input_tokens: input.total_tokens(),
        estimated_text_tokens: input.estimated_text_tokens,
        image_reserve_tokens: input.image_reserve_tokens,
        image_count: input.image_count,
        inline_image_base64_bytes: input.inline_image_base64_bytes,
        estimated_inline_image_decoded_bytes: input.estimated_inline_image_decoded_bytes,
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
    let available = context_window_tokens.saturating_sub(input_tokens.saturating_add(headroom));
    configured_output_tokens.min(available.max(1).min(u32::MAX as usize) as u32)
}

fn estimate_input_tokens(request_body: &Value, output_fields: &[&str]) -> usize {
    estimate_input(request_body, output_fields).total_tokens()
}

#[derive(Default)]
struct InputEstimate {
    estimated_text_tokens: usize,
    image_reserve_tokens: usize,
    image_count: usize,
    inline_image_base64_bytes: usize,
    estimated_inline_image_decoded_bytes: usize,
}

impl InputEstimate {
    fn total_tokens(&self) -> usize {
        self.estimated_text_tokens
            .saturating_add(self.image_reserve_tokens)
    }
}

fn estimate_input(request_body: &Value, output_fields: &[&str]) -> InputEstimate {
    let mut counter = InputCounter {
        output_fields,
        model: request_body["model"].as_str().unwrap_or_default(),
        estimate: InputEstimate::default(),
    };
    let bytes = counter.json_bytes(request_body, Position::Root);
    counter.estimate.estimated_text_tokens = bytes.saturating_add(3) / 4;
    counter.estimate
}

#[derive(Clone, Copy)]
enum Position {
    Root,
    Messages,
    Message,
    Content,
    Block,
    AnthropicImageSource,
    OpenAiImageUrl,
    GeminiContents,
    GeminiContent,
    GeminiParts,
    GeminiPart,
    GeminiInlineData,
    GenerationConfig,
    Plain,
}

struct InputCounter<'a> {
    output_fields: &'a [&'a str],
    model: &'a str,
    estimate: InputEstimate,
}

impl InputCounter<'_> {
    // Count the same escaped JSON bytes as the text estimator without cloning
    // the request or serializing its (potentially very large) image payloads.
    fn json_bytes(&mut self, value: &Value, position: Position) -> usize {
        match value {
            Value::Null => 4,
            Value::Bool(true) => 4,
            Value::Bool(false) => 5,
            Value::Number(number) => number.to_string().len(),
            Value::String(text) => json_string_bytes(text),
            Value::Array(values) => {
                let child = match position {
                    Position::Messages => Position::Message,
                    Position::Content => Position::Block,
                    Position::GeminiContents => Position::GeminiContent,
                    Position::GeminiParts => Position::GeminiPart,
                    _ => Position::Plain,
                };
                values.iter().fold(
                    2usize.saturating_add(values.len().saturating_sub(1)),
                    |bytes, value| bytes.saturating_add(self.json_bytes(value, child)),
                )
            }
            Value::Object(object) => {
                let is_message = matches!(position, Position::Message)
                    && matches!(
                        object.get("role").and_then(Value::as_str),
                        Some("user" | "assistant" | "system" | "developer")
                    )
                    && object
                        .get("type")
                        .is_none_or(|kind| kind.as_str() == Some("message"));
                let anthropic_image = matches!(position, Position::Block)
                    && object.get("type").and_then(Value::as_str) == Some("image")
                    && object.get("source").is_some_and(|source| {
                        source["type"].as_str() == Some("base64")
                            && source["media_type"]
                                .as_str()
                                .is_some_and(|media_type| media_type.starts_with("image/"))
                            && source["data"].is_string()
                    });
                let openai_image = matches!(position, Position::Block)
                    && matches!(
                        object.get("type").and_then(Value::as_str),
                        Some("image_url" | "input_image")
                    )
                    && object.get("image_url").is_some_and(|url| {
                        let url = url.as_str().or_else(|| url["url"].as_str());
                        url.is_some_and(is_image_url)
                    });
                let gemini_image = matches!(position, Position::GeminiPart)
                    && object.get("inlineData").is_some_and(|data| {
                        data["mimeType"]
                            .as_str()
                            .is_some_and(|mime| mime.starts_with("image/"))
                            && data["data"].is_string()
                    });
                if anthropic_image || openai_image || gemini_image {
                    self.estimate.image_count = self.estimate.image_count.saturating_add(1);
                    let inline_data = if anthropic_image {
                        object["source"]["data"].as_str()
                    } else if gemini_image {
                        object["inlineData"]["data"].as_str()
                    } else {
                        let image_url = &object["image_url"];
                        image_url
                            .as_str()
                            .or_else(|| image_url["url"].as_str())
                            .filter(|url| url.starts_with("data:image/"))
                            .and_then(|url| url.split_once(',').map(|(_, data)| data))
                    };
                    let dimensions = inline_data.and_then(inline_image_dimensions);
                    let reserve = if anthropic_image {
                        anthropic_image_tokens(self.model, dimensions)
                    } else if gemini_image {
                        // Gemini media-resolution/model rules vary. Keep a
                        // bounded admission heuristic, not base64 text tokens
                        // or a claim about exact provider usage.
                        conservative_image_reserve_tokens()
                    } else {
                        let detail = object.get("detail").and_then(Value::as_str).or_else(|| {
                            object
                                .get("image_url")
                                .and_then(|url| url.get("detail"))
                                .and_then(Value::as_str)
                        });
                        openai_image_tokens(self.model, detail, dimensions)
                    };
                    self.estimate.image_reserve_tokens =
                        self.estimate.image_reserve_tokens.saturating_add(reserve);
                    if let Some(data) = inline_data {
                        self.estimate.inline_image_base64_bytes = self
                            .estimate
                            .inline_image_base64_bytes
                            .saturating_add(data.len());
                        // Length-only byte accounting, independent of header probing.
                        let padding = if data.ends_with("==") {
                            2
                        } else {
                            usize::from(data.ends_with('='))
                        };
                        let decoded_bytes = (data.len() / 4)
                            .saturating_mul(3)
                            .saturating_add(data.len() % 4 * 3 / 4)
                            .saturating_sub(padding);
                        self.estimate.estimated_inline_image_decoded_bytes = self
                            .estimate
                            .estimated_inline_image_decoded_bytes
                            .saturating_add(decoded_bytes);
                    }
                }
                let mut bytes = 2usize;
                let mut fields = 0usize;
                for (key, value) in object {
                    if matches!(position, Position::Root)
                        && (self.output_fields.contains(&key.as_str()) || key == "thinking")
                    {
                        continue;
                    }
                    if matches!(position, Position::GenerationConfig)
                        && key == "maxOutputTokens"
                        && self
                            .output_fields
                            .contains(&"generationConfig.maxOutputTokens")
                    {
                        continue;
                    }
                    let payload = (matches!(position, Position::AnthropicImageSource)
                        && key == "data")
                        || (matches!(position, Position::GeminiInlineData) && key == "data")
                        || (matches!(position, Position::OpenAiImageUrl) && key == "url")
                        || (openai_image && key == "image_url" && value.is_string());
                    let child = if matches!(position, Position::Root) {
                        match key.as_str() {
                            "messages" | "input" => Position::Messages,
                            "contents" => Position::GeminiContents,
                            "generationConfig" => Position::GenerationConfig,
                            _ => Position::Plain,
                        }
                    } else if is_message && key == "content" {
                        Position::Content
                    } else if matches!(position, Position::GeminiContent)
                        && matches!(
                            object.get("role").and_then(Value::as_str),
                            Some("user" | "model")
                        )
                        && key == "parts"
                    {
                        Position::GeminiParts
                    } else if anthropic_image && key == "source" {
                        Position::AnthropicImageSource
                    } else if openai_image && key == "image_url" {
                        Position::OpenAiImageUrl
                    } else if gemini_image && key == "inlineData" {
                        Position::GeminiInlineData
                    } else if matches!(position, Position::Block)
                        && object.get("type").and_then(Value::as_str) == Some("tool_result")
                        && key == "content"
                    {
                        Position::Content
                    } else {
                        Position::Plain
                    };
                    bytes = bytes
                        .saturating_add(usize::from(fields > 0))
                        .saturating_add(json_string_bytes(key))
                        .saturating_add(1)
                        .saturating_add(if payload {
                            2 // Empty JSON string: preserve syntax, not encoded image bytes.
                        } else {
                            self.json_bytes(value, child)
                        });
                    fields += 1;
                }
                bytes
            }
        }
    }
}

fn is_image_url(url: &str) -> bool {
    url.starts_with("http://")
        || url.starts_with("https://")
        || (url.starts_with("data:image/")
            && url
                .split_once(',')
                .is_some_and(|(header, _)| header.ends_with(";base64")))
}

fn json_string_bytes(text: &str) -> usize {
    text.bytes().fold(2usize, |bytes, byte| {
        bytes.saturating_add(match byte {
            b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 2,
            0..=0x1f => 6,
            _ => 1,
        })
    })
}

fn safety_headroom_tokens(context_window_tokens: usize) -> usize {
    (context_window_tokens / 100)
        .max(MIN_SAFETY_HEADROOM_TOKENS)
        .min(MAX_SAFETY_HEADROOM_TOKENS)
}

#[cfg(test)]
mod tests {
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
    fn legacy_context_budget_diagnostics_default_composition_fields() {
        let budget: crate::provider::ProviderContextBudgetDiagnostics =
            serde_json::from_value(json!({
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

        body["contents"][0]["parts"][1]["inlineData"]["data"] =
            Value::String("A".repeat(4_000_000));
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
}
