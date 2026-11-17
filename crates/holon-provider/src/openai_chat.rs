//! Holon's non-streaming Chat Completions adapter.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use holon_domain::model_values::ServiceTier;
use holon_model_client::{
    chat_completions::RequestBody, parse_response, ClientError, CompletionResponse,
};
use reqwest::{Body, Client, RequestBuilder};
use serde_json::Value;

use crate::budget::effective_output_tokens;

pub struct ChatCompletionsConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    /// Provider id used for endpoint-shape decisions that are not derivable
    /// from the base URL alone.
    pub provider: Option<String>,
    pub model: String,
    pub max_output_tokens: u32,
    pub max_output_tokens_parameter: &'static str,
    pub context_window_tokens: Option<usize>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone)]
pub struct OpenAiChatCompletionsProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    provider: Option<String>,
    model: String,
    max_output_tokens: u32,
    max_output_tokens_parameter: &'static str,
    context_window_tokens: Option<usize>,
    reasoning_effort: Option<String>,
    service_tier: Option<ServiceTier>,
}

impl OpenAiChatCompletionsProvider {
    pub fn new(client: Client, config: ChatCompletionsConfig) -> Self {
        Self {
            client,
            base_url: config.base_url.trim_end_matches('/').into(),
            api_key: config.api_key,
            provider: config.provider,
            model: config.model,
            max_output_tokens: config.max_output_tokens,
            max_output_tokens_parameter: config.max_output_tokens_parameter,
            context_window_tokens: config.context_window_tokens,
            reasoning_effort: config.reasoning_effort,
            service_tier: None,
        }
    }

    pub fn with_service_tier(mut self, tier: Option<ServiceTier>) -> Self {
        self.service_tier = tier;
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    pub fn service_tier(&self) -> Option<&'static str> {
        self.service_tier.map(|tier| match tier {
            ServiceTier::Default => "default",
            ServiceTier::Fast => "priority",
        })
    }

    pub fn authorization_headers(&self) -> Vec<(&'static str, String)> {
        self.api_key
            .as_ref()
            .map(|api_key| vec![("authorization", format!("Bearer {api_key}"))])
            .unwrap_or_default()
    }

    /// Build the complete wire body before Holon records its continuation shape.
    pub fn request_body(
        &self,
        messages: Vec<Value>,
        tools: Vec<Value>,
        prompt_cache_key: Option<&str>,
    ) -> Value {
        let tool_choice = (!tools.is_empty()).then_some("auto");
        let mut body = RequestBody {
            model: &self.model,
            messages,
            output_limit: Some((self.max_output_tokens_parameter, self.max_output_tokens)),
            stream: false,
            tools,
            tool_choice,
            prompt_cache_key,
            reasoning_effort: self.reasoning_effort(),
        }
        .encode();
        if let Some(tier) = self.service_tier() {
            body.insert("service_tier".into(), Value::from(tier));
        }
        Value::Object(body)
    }

    /// Apply the runtime-selected context window after continuation planning.
    pub fn apply_output_budget(&self, body: &mut Value) {
        let output_limit = effective_output_tokens(
            self.context_window_tokens,
            self.max_output_tokens,
            body,
            &[self.max_output_tokens_parameter],
        );
        if let Some(field) = body.get_mut(self.max_output_tokens_parameter) {
            *field = Value::from(output_limit);
        }
    }

    pub async fn complete<T: ChatCompletionsTransport>(
        &self,
        body: Value,
        headers: Vec<(&str, String)>,
        transport: &T,
    ) -> Result<T::Output> {
        let url = self.chat_completions_url();
        complete_chat_request(&self.client, &url, body, headers, transport).await
    }

    /// Resolve the chat-completions endpoint for this provider.
    ///
    /// GitHub Copilot serves the OpenAI-compatible chat surface at the API
    /// root, so it must not receive the generic `/v1` prefix.
    fn chat_completions_url(&self) -> String {
        if self.provider.as_deref() == Some(crate::config::GITHUB_COPILOT_PROVIDER_ID) {
            return format!("{}/chat/completions", self.base_url);
        }
        chat_completions_url(&self.base_url)
    }
}

pub struct ChatCompletionTarget<'a> {
    pub url: &'a str,
    pub model_ref: &'a str,
}

/// Host services scoped to one attempt. The host owns material and traces and
/// checks its dispatch admission immediately before executing the request.
#[async_trait]
pub trait ChatCompletionsTransport: Sync {
    type Trace: Send + Sync;
    type Output: Send;

    fn begin_request(
        &self,
        target: &ChatCompletionTarget<'_>,
        headers: &[(&str, String)],
        body: &Value,
    ) -> Result<Self::Trace>;

    fn request_body(&self, body: &Value) -> Result<Body>;

    async fn send(
        &self,
        request: RequestBuilder,
        target: &ChatCompletionTarget<'_>,
        trace: &Self::Trace,
        request_started_at: DateTime<Utc>,
    ) -> Result<Self::Output>;
}

/// Execute a request against a resolved URL. The adapter and host fixtures
/// share this entry point.
#[doc(hidden)]
pub async fn complete_chat_request<T: ChatCompletionsTransport>(
    client: &Client,
    url: &str,
    body: Value,
    headers: Vec<(&str, String)>,
    transport: &T,
) -> Result<T::Output> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let model_ref = format!("openai/{model}");
    let target = ChatCompletionTarget {
        url,
        model_ref: &model_ref,
    };
    let trace = transport.begin_request(&target, &headers, &body)?;
    let mut request = client.post(url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let request_started_at = Utc::now();
    let request = request.body(transport.request_body(&body)?);
    transport
        .send(request, &target, &trace, request_started_at)
        .await
}

pub fn chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else if has_trailing_version_segment(trimmed) {
        format!("{trimmed}/chat/completions")
    } else {
        format!("{trimmed}/v1/chat/completions")
    }
}

pub fn has_trailing_version_segment(url: &str) -> bool {
    let Some(last_segment) = url.rsplit('/').next() else {
        return false;
    };
    let Some(version_digits) = last_segment.strip_prefix('v') else {
        return false;
    };
    !version_digits.is_empty() && version_digits.chars().all(|ch| ch.is_ascii_digit())
}

pub enum ChatResponseError {
    MissingMessage,
    InvalidResponse(ClientError),
}

pub struct ChatResponse {
    pub completion: CompletionResponse,
    pub raw_message: Value,
    pub served_service_tier: Option<String>,
}

pub fn decode_response(response: Value) -> Result<ChatResponse, ChatResponseError> {
    let raw_message = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .cloned()
        .ok_or(ChatResponseError::MissingMessage)?;
    let served_service_tier = response
        .get("service_tier")
        .and_then(Value::as_str)
        .map(str::to_string);
    let completion = parse_response(response).map_err(ChatResponseError::InvalidResponse)?;
    Ok(ChatResponse {
        completion,
        raw_message,
        served_service_tier,
    })
}

/// Choose the output token field for the resolved endpoint and model.
pub fn output_limit_parameter(provider: &str, endpoint: &str, model: &str) -> &'static str {
    if provider == "tencent-tokenhub"
        && endpoint == "default"
        && matches!(
            model,
            "kimi-k3" | "kimi-k3.1-pro" | "kimi-k3.1-pro-highspeed"
        )
    {
        "max_completion_tokens"
    } else {
        "max_tokens"
    }
}

pub enum ChatFailureKind {
    RateLimited,
    AuthError,
    ContractError,
    ServerError,
}

/// Interpret the OpenAI-compatible error envelope. The host supplies its
/// deterministic-error check and attaches retry, trace and redaction metadata.
pub fn classify_error(
    code: Option<&str>,
    error_type: &str,
    deterministic: bool,
) -> ChatFailureKind {
    use ChatFailureKind::*;

    match code {
        Some("rate_limit_exceeded" | "rate_limit_exceeded_error") => RateLimited,
        Some(
            "insufficient_quota" | "quota_exceeded" | "invalid_api_key" | "invalid_authentication",
        ) => AuthError,
        Some(
            "context_length_exceeded" | "max_tokens" | "invalid_request_error" | "invalid_request",
        ) => ContractError,
        Some("server_error" | "service_unavailable") if !deterministic => ServerError,
        _ if error_type == "rate_limit_error" => RateLimited,
        _ if deterministic => ContractError,
        _ => match error_type {
            "authentication_error" => AuthError,
            "server_error" => ServerError,
            _ => ContractError,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holon_model_client::{chat_completions, Role};
    use serde_json::json;

    #[test]
    fn request_body_preserves_holon_wire_controls() {
        let provider = OpenAiChatCompletionsProvider::new(
            Client::new(),
            ChatCompletionsConfig {
                base_url: "https://model.example/v1".into(),
                api_key: None,
                provider: None,
                model: "kimi-k3".into(),
                max_output_tokens: 512,
                max_output_tokens_parameter: output_limit_parameter(
                    "tencent-tokenhub",
                    "default",
                    "kimi-k3",
                ),
                context_window_tokens: None,
                reasoning_effort: Some("none".into()),
            },
        )
        .with_service_tier(Some(ServiceTier::Fast));
        let assistant = chat_completions::message(
            Role::Assistant,
            Value::Null,
            vec![
                chat_completions::function_call("call-1", "lookup", &json!({"q": "needle"}))
                    .unwrap(),
            ],
            None,
        );
        let result = chat_completions::message(Role::Tool, json!("match"), vec![], Some("call-1"));
        let tool = chat_completions::function_tool(
            "lookup",
            Some("Find a match"),
            &json!({"type":"object"}),
            false,
        );

        let body = provider.request_body(vec![assistant, result], vec![tool], Some("cache-key"));
        assert_eq!(
            body,
            json!({
                "model":"kimi-k3",
                "messages":[
                    {"role":"assistant", "content":null, "tool_calls":[
                        {"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"needle\"}"}}
                    ]},
                    {"role":"tool","tool_call_id":"call-1","content":"match"}
                ],
                "max_completion_tokens":512,
                "stream":false,
                "tools":[{"type":"function","function":{"name":"lookup","description":"Find a match","parameters":{"type":"object"},"strict":false}}],
                "tool_choice":"auto",
                "prompt_cache_key":"cache-key",
                "reasoning_effort":"none",
                "service_tier":"priority"
            })
        );
    }

    #[test]
    fn chat_completions_url_accepts_openai_compatible_base_urls() {
        assert_eq!(
            chat_completions_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://qianfan.baidubce.com/v2"),
            "https://qianfan.baidubce.com/v2/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://ark.ap-southeast.bytepluses.com/api/v3"),
            "https://ark.ap-southeast.bytepluses.com/api/v3/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://api.z.ai/api/paas/v4"),
            "https://api.z.ai/api/paas/v4/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://proxy.example/chat/completions"),
            "https://proxy.example/chat/completions"
        );
    }

    #[test]
    fn github_copilot_chat_url_omits_the_generic_v1_prefix() {
        let provider = OpenAiChatCompletionsProvider::new(
            reqwest::Client::new(),
            ChatCompletionsConfig {
                base_url: "https://api.githubcopilot.com".into(),
                api_key: None,
                provider: Some(crate::config::GITHUB_COPILOT_PROVIDER_ID.to_string()),
                model: "gpt-5".into(),
                max_output_tokens: 1024,
                max_output_tokens_parameter: "max_tokens",
                context_window_tokens: None,
                reasoning_effort: None,
            },
        );
        assert_eq!(
            provider.chat_completions_url(),
            "https://api.githubcopilot.com/chat/completions"
        );

        // Other providers keep the generic OpenAI-compatible shape.
        let generic = OpenAiChatCompletionsProvider::new(
            reqwest::Client::new(),
            ChatCompletionsConfig {
                base_url: "https://api.example.com".into(),
                api_key: None,
                provider: Some("example".into()),
                model: "m".into(),
                max_output_tokens: 16,
                max_output_tokens_parameter: "max_tokens",
                context_window_tokens: None,
                reasoning_effort: None,
            },
        );
        assert_eq!(
            generic.chat_completions_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }
}
