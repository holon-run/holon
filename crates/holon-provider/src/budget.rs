//! Provider context-window and output-token budgets.

use serde_json::Value;

use crate::token_estimate::{
    anthropic_image_tokens, conservative_image_reserve_tokens, inline_image_dimensions,
    openai_image_tokens,
};

const MIN_SAFETY_HEADROOM_TOKENS: usize = 4_096;
const MAX_SAFETY_HEADROOM_TOKENS: usize = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    pub context_window_tokens: usize,
    pub estimated_input_tokens: usize,
    pub estimated_text_tokens: usize,
    pub image_reserve_tokens: usize,
    pub image_count: usize,
    pub inline_image_base64_bytes: usize,
    pub estimated_inline_image_decoded_bytes: usize,
    pub requested_output_tokens: usize,
    pub safety_headroom_tokens: usize,
}

impl ContextBudget {
    pub fn required_tokens(self) -> usize {
        self.estimated_input_tokens
            .saturating_add(self.requested_output_tokens)
            .saturating_add(self.safety_headroom_tokens)
    }

    pub fn overflow_tokens(self) -> usize {
        self.required_tokens()
            .saturating_sub(self.context_window_tokens)
    }
}

pub fn estimate_context_budget(
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

pub fn effective_output_tokens(
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
mod tests;
