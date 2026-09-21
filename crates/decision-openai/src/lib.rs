//! Adapter for OpenAI-compatible chat-completions endpoints.

use async_trait::async_trait;
use decision_core::{
    DecisionContext, DecisionError, DecisionProvider, DecisionRequest, DecisionResponse, Provenance,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fmt;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct OpenAiConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
    pub max_tokens: Option<u32>,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}

impl fmt::Debug for OpenAiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiConfig")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("timeout", &self.timeout)
            .field("max_tokens", &self.max_tokens)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

impl OpenAiConfig {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: normalize_endpoint(&endpoint.into()),
            model: model.into(),
            api_key: None,
            timeout: Duration::from_secs(30),
            max_tokens: None,
            max_request_bytes: 256 * 1024,
            max_response_bytes: 512 * 1024,
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_max_request_bytes(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }

    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }
}

#[derive(Clone, Debug)]
pub struct OpenAiProvider {
    client: reqwest::Client,
    config: OpenAiConfig,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Result<Self, DecisionError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| DecisionError::Transport(error.to_string()))?;
        Ok(Self { client, config })
    }

    pub fn config(&self) -> &OpenAiConfig {
        &self.config
    }
}

#[async_trait]
impl DecisionProvider<Value, Value> for OpenAiProvider {
    type Output = Value;

    async fn decide(
        &self,
        request: DecisionRequest<Value, Value>,
        context: DecisionContext,
    ) -> Result<DecisionResponse<Self::Output>, DecisionError> {
        request.validate()?;
        context.check()?;
        let started = Instant::now();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(api_key) = &self.config.api_key {
            let value = format!("Bearer {api_key}")
                .parse()
                .map_err(|_| DecisionError::InvalidRequest("invalid api key".into()))?;
            headers.insert(AUTHORIZATION, value);
        }

        let prompt = json!({
            "request_id": request.request_id,
            "input": request.input,
            "candidates": request.candidates,
            "schema": request.schema,
            "schema_version": request.schema_version,
            "metadata": request.metadata,
            "deadline_ms": request.deadline_ms,
        });
        let mut body = json!({
            "model": self.config.model,
            "temperature": 0,
            "response_format": { "type": "json_object" },
            "messages": [
                {
                    "role": "system",
                    "content": "Return only a JSON DecisionResponse matching the requested schema. Do not invent provenance."
                },
                {
                    "role": "user",
                    "content": prompt.to_string()
                }
            ]
        });
        if let Some(max_tokens) = self.config.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        let body = serde_json::to_vec(&body)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?;
        if body.len() > self.config.max_request_bytes {
            return Err(DecisionError::ResourceExhausted(format!(
                "request body exceeds {} bytes",
                self.config.max_request_bytes
            )));
        }

        let timeout = context
            .remaining()
            .map(|remaining| remaining.min(self.config.timeout))
            .unwrap_or(self.config.timeout);
        let response = send_with_context(
            self.client
                .post(&self.config.endpoint)
                .headers(headers)
                .timeout(timeout)
                .body(body),
            &context,
        )
        .await?;
        context.check()?;
        let status = response.status();
        let response_body =
            read_bounded(response, &context, self.config.max_response_bytes).await?;
        if !status.is_success() {
            return Err(DecisionError::Provider(format!(
                "openai-compatible endpoint returned {status}: {}",
                truncate(&response_body, 512)
            )));
        }

        let content = parse_content(&response_body)?;
        let mut decision: DecisionResponse<Value> = serde_json::from_str(&content)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?;
        if decision.provenance.provider.is_empty() {
            decision.provenance =
                Provenance::new("openai-compatible", Some(self.config.model.clone()));
        }
        decision.elapsed_ms = Some(started.elapsed().as_millis() as u64);
        decision.validate(&request.schema_version)?;
        Ok(decision)
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

fn parse_content(body: &str) -> Result<String, DecisionError> {
    let response: ChatResponse = serde_json::from_str(body)
        .map_err(|error| DecisionError::Serialization(error.to_string()))?;
    response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| DecisionError::InvalidResponse("missing message content".into()))
}

fn normalize_endpoint(endpoint: &str) -> String {
    let endpoint = endpoint.trim_end_matches('/');
    if endpoint.ends_with("/chat/completions") {
        endpoint.to_owned()
    } else if endpoint.ends_with("/v1") {
        format!("{endpoint}/chat/completions")
    } else {
        format!("{endpoint}/v1/chat/completions")
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

async fn send_with_context(
    request: reqwest::RequestBuilder,
    context: &DecisionContext,
) -> Result<reqwest::Response, DecisionError> {
    let send = request.send();
    tokio::pin!(send);
    loop {
        tokio::select! {
            result = &mut send => {
                return result.map_err(|error| {
                    if error.is_timeout() {
                        DecisionError::DeadlineExceeded
                    } else if context.is_cancelled() {
                        DecisionError::Cancelled
                    } else {
                        DecisionError::Transport(error.to_string())
                    }
                });
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => context.check()?,
        }
    }
}

async fn read_bounded(
    mut response: reqwest::Response,
    context: &DecisionContext,
    max_bytes: usize,
) -> Result<String, DecisionError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(DecisionError::ResourceExhausted(format!(
            "response body exceeds {max_bytes} bytes"
        )));
    }
    let mut body = Vec::new();
    loop {
        let chunk = response.chunk();
        tokio::pin!(chunk);
        let chunk = tokio::select! {
            result = &mut chunk => result,
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                context.check()?;
                continue;
            }
        }
        .map_err(|error| DecisionError::Transport(error.to_string()))?;
        let Some(chunk) = chunk else { break };
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(DecisionError::ResourceExhausted(format!(
                "response body exceeds {max_bytes} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|error| DecisionError::Serialization(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_normalization_supports_local_servers() {
        assert_eq!(
            normalize_endpoint("http://localhost:11434"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            normalize_endpoint("http://localhost:8000/v1/chat/completions"),
            "http://localhost:8000/v1/chat/completions"
        );
        assert_eq!(
            normalize_endpoint("http://localhost:8000/v1"),
            "http://localhost:8000/v1/chat/completions"
        );
    }

    #[test]
    fn redacts_api_key_in_debug_output() {
        let debug = format!(
            "{:?}",
            OpenAiConfig::new("https://example.test", "model").with_api_key("secret")
        );
        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn content_parser_rejects_empty_choices() {
        let error = parse_content(r#"{"choices":[]}"#).expect_err("must fail");
        assert!(matches!(error, DecisionError::InvalidResponse(_)));
    }

    #[test]
    fn content_parser_extracts_json_envelope() {
        let body = r#"{"choices":[{"message":{"content":"{\"schema_version\":\"1\"}"}}]}"#;
        assert_eq!(
            parse_content(body).expect("content"),
            r#"{"schema_version":"1"}"#
        );
    }
}
