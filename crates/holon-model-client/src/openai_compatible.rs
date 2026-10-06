use crate::{
    CallContext, ClientError, CompletionRequest, CompletionResponse, ContentPart, Message,
    ModelClient, RequestExtensions, ResponseExtensions, ResponseFormat, Role, ToolCall, Usage,
};
use async_trait::async_trait;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE},
    Client,
};
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, time::Duration};

/// Configuration for an OpenAI Chat Completions-compatible endpoint.
#[derive(Clone)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
    pub headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for OpenAiCompatibleConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let header_names = self.headers.keys().collect::<Vec<_>>();
        formatter
            .debug_struct("OpenAiCompatibleConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("timeout", &self.timeout)
            .field("header_names", &header_names)
            .finish()
    }
}

impl Default for OpenAiCompatibleConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: None,
            timeout: Duration::from_secs(120),
            headers: BTreeMap::new(),
        }
    }
}

/// Minimal transport for providers exposing the OpenAI Chat Completions wire
/// contract. It intentionally does not implement retries or fallback.
#[derive(Clone)]
pub struct OpenAiCompatibleClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    headers: HeaderMap,
}

impl std::fmt::Debug for OpenAiCompatibleClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let header_names = self.headers.keys().collect::<Vec<_>>();
        formatter
            .debug_struct("OpenAiCompatibleClient")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("header_names", &header_names)
            .finish()
    }
}

impl OpenAiCompatibleClient {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, ClientError> {
        let base_url = config.base_url.trim_end_matches('/').to_string();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(ClientError::InvalidConfiguration(
                "base_url must start with http:// or https://".to_string(),
            ));
        }

        let mut headers = HeaderMap::new();
        for (name, value) in config.headers {
            let name = HeaderName::try_from(name.as_str()).map_err(|error| {
                ClientError::InvalidConfiguration(format!("invalid header name: {error}"))
            })?;
            let value = HeaderValue::try_from(value.as_str()).map_err(|error| {
                ClientError::InvalidConfiguration(format!("invalid header value: {error}"))
            })?;
            headers.insert(name, value);
        }

        let client = Client::builder()
            .default_headers(headers.clone())
            .timeout(config.timeout)
            .build()
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;

        Ok(Self {
            client,
            base_url,
            api_key: config.api_key.filter(|value| !value.trim().is_empty()),
            headers,
        })
    }

    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    /// Lower a provider-neutral request to the OpenAI-compatible JSON shape.
    pub fn build_request_body(request: &CompletionRequest) -> Result<Value, ClientError> {
        validate_request(request)?;

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(request.model.clone()));
        body.insert(
            "messages".to_string(),
            Value::Array(
                request
                    .messages
                    .iter()
                    .map(message_to_json)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        );
        body.insert("stream".to_string(), Value::Bool(request.stream));

        if let Some(max_output_tokens) = request.max_output_tokens {
            body.insert(
                "max_tokens".to_string(),
                Value::Number(max_output_tokens.into()),
            );
        }

        if !request.tools.is_empty() {
            body.insert(
                "tools".to_string(),
                Value::Array(
                    request
                        .tools
                        .iter()
                        .map(|tool| {
                            json!({
                                "type": "function",
                                "function": {
                                    "name": tool.name,
                                    "description": tool.description,
                                    "parameters": tool.parameters,
                                    "strict": tool.strict,
                                }
                            })
                        })
                        .collect(),
                ),
            );
        }

        if let Some(response_format) = &request.response_format {
            body.insert(
                "response_format".to_string(),
                response_format_to_json(response_format),
            );
        }

        lower_extensions(&mut body, &request.extensions);
        Ok(Value::Object(body))
    }

    fn authorization_header(&self) -> Result<Option<HeaderValue>, ClientError> {
        self.api_key
            .as_deref()
            .map(|key| {
                HeaderValue::try_from(format!("Bearer {key}")).map_err(|error| {
                    ClientError::InvalidConfiguration(format!(
                        "api key cannot be encoded as a header: {error}"
                    ))
                })
            })
            .transpose()
    }

    fn request_id(headers: &HeaderMap) -> Option<String> {
        ["x-request-id", "request-id", "openai-request-id"]
            .iter()
            .find_map(|name| headers.get(*name))
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string)
    }
}

#[async_trait]
impl ModelClient for OpenAiCompatibleClient {
    fn provider_name(&self) -> &str {
        "openai-compatible"
    }

    async fn complete(
        &self,
        context: &CallContext,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ClientError> {
        if request.stream {
            return Err(ClientError::Unsupported(
                "streaming responses are not part of the first transport slice".to_string(),
            ));
        }

        let body = Self::build_request_body(&request)?;
        let mut request_builder = self
            .client
            .post(self.chat_completions_url())
            .headers(self.headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .json(&body);

        if let Some(authorization) = self.authorization_header()? {
            request_builder = request_builder.header(AUTHORIZATION, authorization);
        }
        if let Some(request_id) = context.request_id.as_deref() {
            request_builder = request_builder.header("x-client-request-id", request_id);
        }
        for (name, value) in &context.headers {
            let name = HeaderName::try_from(name.as_str()).map_err(|error| {
                ClientError::InvalidRequest(format!("invalid request header name: {error}"))
            })?;
            let value = HeaderValue::try_from(value.as_str()).map_err(|error| {
                ClientError::InvalidRequest(format!("invalid request header value: {error}"))
            })?;
            request_builder = request_builder.header(name, value);
        }
        if let Some(timeout) = context.timeout {
            request_builder = request_builder.timeout(timeout);
        }

        let response = request_builder
            .send()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        let response_headers = response.headers().clone();
        let request_id = Self::request_id(&response_headers);
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;

        if !status.is_success() {
            return Err(ClientError::Http {
                status: status.as_u16(),
                request_id,
                body,
            });
        }

        let value: Value =
            serde_json::from_str(&body).map_err(|error| ClientError::Decode(error.to_string()))?;
        parse_response(value)
    }
}

fn validate_request(request: &CompletionRequest) -> Result<(), ClientError> {
    if request.model.trim().is_empty() {
        return Err(ClientError::InvalidRequest(
            "model must not be empty".to_string(),
        ));
    }
    if request.messages.is_empty() {
        return Err(ClientError::InvalidRequest(
            "at least one message is required".to_string(),
        ));
    }
    if request.continuation.is_some() {
        return Err(ClientError::Unsupported(
            "continuation is not supported by the OpenAI-compatible transport until request replay is implemented"
                .to_string(),
        ));
    }
    Ok(())
}

fn message_to_json(message: &Message) -> Result<Value, ClientError> {
    let mut result = Map::new();
    result.insert(
        "role".to_string(),
        Value::String(role_name(message.role).to_string()),
    );

    let content = match message.content.as_slice() {
        [] => Value::Null,
        [ContentPart::Text { text }] => Value::String(text.clone()),
        _ => Value::Array(message.content.iter().map(content_part_to_json).collect()),
    };
    result.insert("content".to_string(), content);
    if !message.tool_calls.is_empty() {
        if message.role != Role::Assistant {
            return Err(ClientError::InvalidRequest(
                "tool calls require an assistant message".to_string(),
            ));
        }
        let tool_calls = message
            .tool_calls
            .iter()
            .map(|tool_call| {
                let arguments = serde_json::to_string(&tool_call.arguments).map_err(|error| {
                    ClientError::InvalidRequest(format!(
                        "failed to serialize tool call arguments: {error}"
                    ))
                })?;
                Ok(json!({
                    "id": tool_call.id,
                    "type": "function",
                    "function": {
                        "name": tool_call.name,
                        "arguments": arguments,
                    },
                }))
            })
            .collect::<Result<Vec<_>, ClientError>>()?;
        result.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        result.insert(
            "tool_call_id".to_string(),
            Value::String(tool_call_id.clone()),
        );
    }
    Ok(Value::Object(result))
}

fn content_part_to_json(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({ "type": "text", "text": text }),
        ContentPart::ImageUrl { image_url } => json!({
            "type": "image_url",
            "image_url": {
                "url": image_url.url,
                "detail": image_url.detail,
            }
        }),
    }
}

fn response_format_to_json(response_format: &ResponseFormat) -> Value {
    match response_format {
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => json!({
            "type": "json_schema",
            "json_schema": {
                "name": name,
                "schema": schema,
                "strict": strict,
            }
        }),
    }
}

fn lower_extensions(body: &mut Map<String, Value>, extensions: &RequestExtensions) {
    if let Some(reasoning_effort) = &extensions.reasoning_effort {
        body.entry("reasoning_effort".to_string())
            .or_insert_with(|| Value::String(reasoning_effort.clone()));
    }
    if let Some(search) = &extensions.native_web_search {
        body.entry("web_search_options".to_string())
            .or_insert_with(|| {
                json!({
                    "search_context_size": search.kind,
                    "query": search.query,
                })
            });
    }
    if let Some(cache) = &extensions.cache {
        if let Some(key) = &cache.key {
            body.entry("prompt_cache_key".to_string())
                .or_insert_with(|| Value::String(key.clone()));
        }
        if let Some(retention) = &cache.retention {
            body.entry("prompt_cache_retention".to_string())
                .or_insert_with(|| Value::String(retention.clone()));
        }
    }
    for (key, value) in &extensions.extra {
        body.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

/// Parse one non-streaming OpenAI Chat Completions response.
///
/// The original JSON is retained in [`CompletionResponse::provider_data`] so
/// an embedding runtime can preserve provider-native fields that are not part
/// of the common contract.
pub fn parse_response(value: Value) -> Result<CompletionResponse, ClientError> {
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| ClientError::Decode("response is missing choices".to_string()))?;
    let choice = choices
        .first()
        .ok_or_else(|| ClientError::Decode("response choices are empty".to_string()))?;
    let raw_message = choice
        .get("message")
        .ok_or_else(|| ClientError::Decode("response is missing message".to_string()))?;
    let mut message = parse_message(raw_message)?;
    let tool_calls: Vec<ToolCall> = raw_message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| calls.iter().map(parse_tool_call).collect())
        .transpose()?
        .unwrap_or_default();
    message.tool_calls = tool_calls.clone();
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let has_supported_content = message.content.iter().any(|part| match part {
        ContentPart::Text { text } => !text.is_empty(),
        ContentPart::ImageUrl { .. } => true,
    });
    if !has_supported_content && tool_calls.is_empty() && finish_reason.is_none() {
        return Err(ClientError::Decode(
            "response contained no supported content".to_string(),
        ));
    }

    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    Ok(CompletionResponse {
        continuation: None,
        id,
        model: value
            .get("model")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        message,
        tool_calls,
        finish_reason,
        usage: parse_usage(value.get("usage")),
        extensions: ResponseExtensions {
            reasoning: raw_message
                .get("reasoning")
                .cloned()
                .or_else(|| raw_message.get("reasoning_content").cloned()),
            extra: BTreeMap::new(),
        },
        provider_data: value,
    })
}

fn parse_message(value: &Value) -> Result<Message, ClientError> {
    let role = match value.get("role").and_then(Value::as_str) {
        Some("system") => Role::System,
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some("tool") => Role::Tool,
        Some(other) => {
            return Err(ClientError::Decode(format!(
                "unsupported message role {other:?}"
            )))
        }
        None => Role::Assistant,
    };
    let content = match value.get("content") {
        Some(Value::String(text)) if !text.is_empty() => {
            vec![ContentPart::Text { text: text.clone() }]
        }
        Some(Value::String(_)) => Vec::new(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    part.get("text")
                        .and_then(Value::as_str)
                        .map(|text| ContentPart::Text {
                            text: text.to_string(),
                        })
                }
                _ => None,
            })
            .collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => {
            return Err(ClientError::Decode(format!(
                "unsupported message content: {other}"
            )))
        }
    };
    Ok(Message {
        role,
        content,
        tool_calls: Vec::new(),
        tool_call_id: value
            .get("tool_call_id")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

fn parse_tool_call(value: &Value) -> Result<ToolCall, ClientError> {
    let function = value
        .get("function")
        .ok_or_else(|| ClientError::Decode("tool call is missing function".to_string()))?;
    let arguments = match function.get("arguments") {
        Some(Value::String(arguments)) if arguments.trim().is_empty() => json!({}),
        Some(Value::String(arguments)) => serde_json::from_str(arguments).map_err(|error| {
            ClientError::Decode(format!("invalid tool call arguments JSON: {error}"))
        })?,
        Some(Value::Null) | None => json!({}),
        Some(arguments) => arguments.clone(),
    };
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| ClientError::Decode("tool call is missing id".to_string()))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ClientError::Decode("tool call function is missing name".to_string()))?;
    Ok(ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    })
}

fn parse_usage(value: Option<&Value>) -> Option<Usage> {
    let value = value?;
    Some(Usage {
        input_tokens: value
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        total_tokens: value
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: value
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64),
    })
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentPart, ContinuationState, ImageUrl, ToolDefinition};

    #[test]
    fn lowers_tools_extensions_and_multimodal_content() {
        let mut request = CompletionRequest::new(
            "gpt-test",
            vec![
                Message::system("You are concise."),
                Message {
                    role: Role::User,
                    content: vec![
                        ContentPart::text("Describe this."),
                        ContentPart::ImageUrl {
                            image_url: ImageUrl {
                                url: "https://example.test/image.png".to_string(),
                                detail: Some("low".to_string()),
                            },
                        },
                    ],
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
            ],
        );
        request.max_output_tokens = Some(128);
        request.tools.push(ToolDefinition {
            name: "lookup".to_string(),
            description: Some("Look something up.".to_string()),
            parameters: json!({"type": "object", "properties": {}}),
            strict: true,
        });
        request.extensions.reasoning_effort = Some("low".to_string());
        request
            .extensions
            .extra
            .insert("temperature".to_string(), json!(0));

        let body = OpenAiCompatibleClient::build_request_body(&request).unwrap();
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["messages"][1]["content"][1]["type"], "image_url");
        assert_eq!(body["tools"][0]["function"]["name"], "lookup");
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["temperature"], 0);
    }

    #[test]
    fn parses_tool_calls_usage_without_unsupported_continuation_state() {
        let value = json!({
            "id": "chatcmpl-test",
            "model": "gpt-test",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "lookup",
                            "arguments": "{\"query\":\"rust\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 7,
                "completion_tokens": 3,
                "total_tokens": 10,
                "prompt_tokens_details": {"cached_tokens": 2}
            }
        });

        let response = parse_response(value).unwrap();
        assert_eq!(response.id.as_deref(), Some("chatcmpl-test"));
        assert!(response.continuation.is_none());
        assert_eq!(response.tool_calls[0].arguments["query"], "rust");
        assert_eq!(response.usage.unwrap().cached_input_tokens, Some(2));
    }

    #[test]
    fn parses_empty_tool_call_arguments_as_an_empty_object() {
        let response = parse_response(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call-noargs",
                        "type": "function",
                        "function": {
                            "name": "get_status",
                            "arguments": ""
                        }
                    }]
                }
            }]
        }))
        .unwrap();

        assert_eq!(response.tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn parses_missing_tool_call_arguments_as_an_empty_object() {
        let response = parse_response(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call-missing-args",
                        "type": "function",
                        "function": {
                            "name": "get_status"
                        }
                    }]
                }
            }]
        }))
        .unwrap();

        assert_eq!(response.tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn parses_null_tool_call_arguments_as_an_empty_object() {
        let response = parse_response(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call-null-args",
                        "type": "function",
                        "function": {
                            "name": "get_status",
                            "arguments": null
                        }
                    }]
                }
            }]
        }))
        .unwrap();

        assert_eq!(response.tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn rejects_empty_content_without_finish_reason_or_tool_calls() {
        let error = parse_response(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": ""
                }
            }]
        }))
        .unwrap_err();

        assert!(
            matches!(error, ClientError::Decode(message) if message.contains("no supported content"))
        );
    }

    #[test]
    fn rejects_continuation_until_request_replay_is_supported() {
        let mut request = CompletionRequest::new("gpt-test", vec![Message::user("continue")]);
        request.continuation = Some(ContinuationState::response_id("chatcmpl-test"));

        let error = OpenAiCompatibleClient::build_request_body(&request).unwrap_err();
        assert!(
            matches!(error, ClientError::Unsupported(message) if message.contains("request replay"))
        );
    }

    #[test]
    fn preserves_assistant_tool_calls_for_tool_result_follow_up() {
        let response = parse_response(json!({
            "id": "chatcmpl-tool-roundtrip",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-roundtrip",
                        "type": "function",
                        "function": {
                            "name": "lookup",
                            "arguments": "{\"query\":\"rust\"}"
                        }
                    }]
                }
            }]
        }))
        .unwrap();

        let request = CompletionRequest::new(
            "gpt-test",
            vec![
                response.message,
                Message::tool("call-roundtrip", "{\"result\":\"ok\"}"),
            ],
        );
        let body = OpenAiCompatibleClient::build_request_body(&request).unwrap();

        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"], Value::Null);
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["arguments"],
            "{\"query\":\"rust\"}"
        );
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "call-roundtrip");
    }

    #[test]
    fn rejects_empty_requests() {
        let request = CompletionRequest::new("", Vec::new());
        let error = OpenAiCompatibleClient::build_request_body(&request).unwrap_err();
        assert!(matches!(error, ClientError::InvalidRequest(_)));
    }

    #[test]
    fn debug_redacts_api_keys_and_header_values() {
        let mut headers = BTreeMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer do-not-log-this-header".to_string(),
        );
        let config = OpenAiCompatibleConfig {
            api_key: Some("do-not-log-this-key".to_string()),
            headers,
            ..Default::default()
        };

        let config_debug = format!("{config:?}");
        assert!(config_debug.contains("Authorization"));
        assert!(!config_debug.contains("do-not-log-this-key"));
        assert!(!config_debug.contains("do-not-log-this-header"));

        let client = OpenAiCompatibleClient::new(config).unwrap();
        let client_debug = format!("{client:?}");
        assert!(client_debug.to_ascii_lowercase().contains("authorization"));
        assert!(!client_debug.contains("do-not-log-this-key"));
        assert!(!client_debug.contains("do-not-log-this-header"));
    }
}
