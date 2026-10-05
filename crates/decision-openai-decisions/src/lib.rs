//! Adapter for OpenAI's dedicated Decisions API.
//!
//! The Decisions endpoint is not Chat Completions or Responses. Its request
//! and response translation intentionally stays in this crate so the
//! decision-core contract remains provider-neutral.

use async_trait::async_trait;
use decision_core::{
    DecisionContext, DecisionError, DecisionOutcome, DecisionProvider, DecisionRequest,
    DecisionResponse, Evidence, Provenance,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Map, Value};
use std::fmt;
use std::time::{Duration, Instant};

const DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1/decisions";
const DEFAULT_MODEL: &str = "gpt-6-luna";
const QUESTION_ID: &str = "decision";
const DEFAULT_QUESTION_TYPE: &str = "choice";

#[derive(Clone)]
pub struct OpenAiDecisionsConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}

impl fmt::Debug for OpenAiDecisionsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiDecisionsConfig")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("timeout", &self.timeout)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

impl OpenAiDecisionsConfig {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: normalize_endpoint(&endpoint.into()),
            model: model.into(),
            api_key: None,
            timeout: Duration::from_secs(30),
            max_request_bytes: 256 * 1024,
            max_response_bytes: 512 * 1024,
        }
    }

    pub fn default_api() -> Self {
        Self::new(DEFAULT_ENDPOINT, DEFAULT_MODEL)
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
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
pub struct OpenAiDecisionsProvider {
    client: reqwest::Client,
    config: OpenAiDecisionsConfig,
}

impl OpenAiDecisionsProvider {
    pub fn new(config: OpenAiDecisionsConfig) -> Result<Self, DecisionError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| DecisionError::Transport(error.to_string()))?;
        Ok(Self { client, config })
    }

    pub fn config(&self) -> &OpenAiDecisionsConfig {
        &self.config
    }
}

#[async_trait]
impl DecisionProvider<Value, Value> for OpenAiDecisionsProvider {
    type Output = Value;

    async fn decide(
        &self,
        request: DecisionRequest<Value, Value>,
        context: DecisionContext,
    ) -> Result<DecisionResponse<Self::Output>, DecisionError> {
        request.validate()?;
        context.check()?;
        let started = Instant::now();
        let body = build_request_body(&self.config.model, &request)?;
        let body = serde_json::to_vec(&body)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?;
        if body.len() > self.config.max_request_bytes {
            return Err(DecisionError::ResourceExhausted(format!(
                "request body exceeds {} bytes",
                self.config.max_request_bytes
            )));
        }

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(api_key) = self.config.api_key.as_deref() {
            let value = format!("Bearer {api_key}");
            let header = HeaderValue::from_str(&value)
                .map_err(|error| DecisionError::InvalidRequest(error.to_string()))?;
            headers.insert(AUTHORIZATION, header);
        }

        let timeout = context
            .remaining()
            .map(|remaining| remaining.min(self.config.timeout))
            .unwrap_or(self.config.timeout);
        let request_builder = self
            .client
            .post(&self.config.endpoint)
            .headers(headers)
            .timeout(timeout)
            .body(body);
        let response = send_with_context(request_builder, &context).await?;
        context.check()?;
        let status = response.status();
        let response_body =
            read_bounded(response, &context, self.config.max_response_bytes).await?;
        context.check()?;
        if !status.is_success() {
            return Err(DecisionError::Provider(format!(
                "OpenAI Decisions API returned {status}: {}",
                truncate_for_error(&response_body)
            )));
        }

        let value: Value = serde_json::from_str(&response_body)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?;
        let mut decision = map_response(&request, &value, started.elapsed())?;
        decision.provenance = Provenance {
            provider: "openai-decisions".into(),
            model: Some(
                value
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.config.model)
                    .to_owned(),
            ),
            policy: None,
            request_id: Some(request.request_id.clone()),
        };
        decision.validate(&request.schema_version)?;
        Ok(decision)
    }
}

fn build_request_body<I, C>(
    model: &str,
    request: &DecisionRequest<I, C>,
) -> Result<Value, DecisionError>
where
    I: serde::Serialize,
    C: serde::Serialize,
{
    let question_type = request
        .metadata
        .get("openai_decisions.question_type")
        .map(String::as_str)
        .unwrap_or(DEFAULT_QUESTION_TYPE);
    if !matches!(question_type, "predicate" | "choice" | "score") {
        return Err(DecisionError::InvalidRequest(format!(
            "unsupported OpenAI Decisions question type {question_type}"
        )));
    }

    let mut criteria = Map::new();
    for (index, candidate) in request.candidates.iter().enumerate() {
        criteria.insert(
            format!("candidate_{index}"),
            serde_json::to_value(candidate)
                .map_err(|error| DecisionError::Serialization(error.to_string()))?,
        );
    }

    let mut question = Map::new();
    question.insert("type".into(), json!(question_type));
    question.insert("instructions".into(), json!(request.schema));
    question.insert("criteria".into(), Value::Object(criteria));

    let mut questions = Map::new();
    questions.insert(QUESTION_ID.into(), Value::Object(question));

    Ok(json!({
        "model": model,
        "input": request.input,
        "questions": questions,
    }))
}

fn map_response(
    request: &DecisionRequest<Value, Value>,
    response: &Value,
    elapsed: Duration,
) -> Result<DecisionResponse<Value>, DecisionError> {
    let answer = response
        .get("answers")
        .and_then(|answers| answers.get(QUESTION_ID))
        .or_else(|| response.get("answer"))
        .ok_or_else(|| DecisionError::InvalidResponse("missing answers.decision".into()))?;

    let answer_type = answer
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_QUESTION_TYPE);
    let confidence = answer
        .get("confidence")
        .and_then(value_as_f32)
        .or_else(|| answer.get("probability").and_then(value_as_f32));

    let (outcome, confidence, evidence) = match answer_type {
        "predicate" | "noul" => {
            let value = answer
                .get("predicate")
                .or_else(|| answer.get("value"))
                .or_else(|| answer.get("answer"))
                .ok_or_else(|| {
                    DecisionError::InvalidResponse("predicate answer has no value".into())
                })?;
            (
                DecisionOutcome::Select {
                    value: coerce_value(value, request.candidates.first()),
                },
                confidence,
                vec![Evidence::new(
                    "openai_decisions",
                    "OpenAI Decisions predicate answer",
                )],
            )
        }
        "score" => {
            let value = answer
                .get("score")
                .or_else(|| answer.get("value"))
                .ok_or_else(|| {
                    DecisionError::InvalidResponse("score answer has no score".into())
                })?;
            (
                DecisionOutcome::Select {
                    value: value.clone(),
                },
                confidence,
                vec![Evidence::new(
                    "openai_decisions",
                    "OpenAI Decisions score answer",
                )],
            )
        }
        "choice" => {
            let choice = answer
                .get("choice")
                .or_else(|| answer.get("value"))
                .ok_or_else(|| {
                    DecisionError::InvalidResponse("choice answer has no choice".into())
                })?;
            let selected = if let Some(key) = choice.as_str() {
                if let Some(index) = key.strip_prefix("candidate_") {
                    let index = index.parse::<usize>().map_err(|_| {
                        DecisionError::InvalidResponse(format!(
                            "choice answer contains invalid candidate id {key}"
                        ))
                    })?;
                    request.candidates.get(index).cloned().ok_or_else(|| {
                        DecisionError::InvalidResponse(format!(
                            "choice answer references unknown candidate {key}"
                        ))
                    })?
                } else {
                    choice.clone()
                }
            } else {
                choice.clone()
            };
            let mut evidence = Evidence::new("openai_decisions", "OpenAI Decisions choice answer");
            if let Some(probabilities) = answer.get("probabilities") {
                evidence
                    .metadata
                    .insert("probabilities".into(), probabilities.to_string());
            }
            (
                DecisionOutcome::Select { value: selected },
                confidence,
                vec![evidence],
            )
        }
        other => {
            return Err(DecisionError::InvalidResponse(format!(
                "unsupported OpenAI Decisions answer type {other}"
            )));
        }
    };

    Ok(DecisionResponse {
        schema_version: request.schema_version.clone(),
        outcome,
        confidence,
        evidence,
        provenance: Provenance::new("openai-decisions", None),
        elapsed_ms: Some(elapsed.as_millis() as u64),
    })
}

fn coerce_value(value: &Value, candidate: Option<&Value>) -> Value {
    if value.is_boolean() || value.is_number() || value.is_object() || value.is_array() {
        return value.clone();
    }
    if value.as_str() == Some("candidate_0") {
        if let Some(candidate) = candidate {
            return candidate.clone();
        }
    }
    value.clone()
}

fn value_as_f32(value: &Value) -> Option<f32> {
    value.as_f64().map(|value| value as f32)
}

fn normalize_endpoint(endpoint: &str) -> String {
    let endpoint = endpoint.trim_end_matches('/');
    if endpoint.ends_with("/v1/decisions") {
        endpoint.to_owned()
    } else if endpoint.ends_with("/v1") {
        format!("{endpoint}/decisions")
    } else {
        format!("{endpoint}/v1/decisions")
    }
}

async fn send_with_context(
    request: reqwest::RequestBuilder,
    context: &DecisionContext,
) -> Result<reqwest::Response, DecisionError> {
    let send = request.send();
    tokio::pin!(send);
    loop {
        tokio::select! {
            response = &mut send => {
                return response.map_err(|error| {
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
        let next = tokio::select! {
            result = &mut chunk => result,
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                context.check()?;
                continue;
            }
        }
        .map_err(|error| map_request_error(error, context))?;
        let Some(chunk) = next else {
            break;
        };
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(DecisionError::ResourceExhausted(format!(
                "response body exceeds {max_bytes} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|error| DecisionError::Serialization(error.to_string()))
}

fn map_request_error(error: reqwest::Error, context: &DecisionContext) -> DecisionError {
    if error.is_timeout() {
        DecisionError::DeadlineExceeded
    } else if context.is_cancelled() {
        DecisionError::Cancelled
    } else {
        DecisionError::Transport(error.to_string())
    }
}

fn truncate_for_error(body: &str) -> String {
    body.chars().take(512).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use decision_core::DecisionProvider;
    use serde_json::json;
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn request(metadata: BTreeMap<String, String>) -> DecisionRequest<Value, Value> {
        DecisionRequest {
            request_id: "req-1".into(),
            input: json!({"text": "hello"}),
            candidates: vec![json!({"id": "a"}), json!({"id": "b"})],
            schema: "choose one".into(),
            schema_version: "1".into(),
            metadata,
            deadline_ms: None,
        }
    }

    fn test_server(response: Option<&'static [u8]>, delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            thread::sleep(delay);
            if let Some(response) = response {
                stream.write_all(response).expect("response");
            }
        });
        format!("http://{address}")
    }

    fn auth_test_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).expect("request");
            let request = String::from_utf8_lossy(&request[..length]).to_ascii_lowercase();
            let response: &[u8] = if request.contains("authorization: bearer secret") {
                b"HTTP/1.1 200 OK\r\nContent-Length: 86\r\nConnection: close\r\n\r\n{\"model\":\"gpt-6-luna\",\"answers\":{\"decision\":{\"type\":\"choice\",\"choice\":\"candidate_1\"}}}"
            } else {
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 7\r\nConnection: close\r\n\r\nmissing"
            };
            stream.write_all(response).expect("response");
        });
        format!("http://{address}")
    }

    fn drip_test_server(
        head: &'static [u8],
        chunks: &'static [&'static [u8]],
        gap: Duration,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream.write_all(head).expect("head");
            for chunk in chunks {
                thread::sleep(gap);
                stream.write_all(chunk).expect("chunk");
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn normalizes_endpoint() {
        assert_eq!(
            normalize_endpoint("https://api.openai.com"),
            "https://api.openai.com/v1/decisions"
        );
        assert_eq!(
            normalize_endpoint("http://localhost:8000/v1"),
            "http://localhost:8000/v1/decisions"
        );
        assert_eq!(
            normalize_endpoint("http://localhost:8000/v1/decisions"),
            "http://localhost:8000/v1/decisions"
        );
    }

    #[test]
    fn redacts_api_key() {
        let debug = format!(
            "{:?}",
            OpenAiDecisionsConfig::default_api().with_api_key("secret")
        );
        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn builds_decisions_request_with_choice_question() {
        let body = build_request_body("gpt-6-luna", &request(BTreeMap::new())).unwrap();
        assert_eq!(body["model"], "gpt-6-luna");
        assert_eq!(body["questions"]["decision"]["type"], "choice");
        assert_eq!(
            body["questions"]["decision"]["criteria"]["candidate_0"]["id"],
            "a"
        );
    }

    #[test]
    fn supports_predicate_and_score_question_types() {
        for question_type in ["predicate", "score"] {
            let mut metadata = BTreeMap::new();
            metadata.insert(
                "openai_decisions.question_type".into(),
                question_type.into(),
            );
            let body = build_request_body("gpt-6-luna", &request(metadata)).unwrap();
            assert_eq!(body["questions"]["decision"]["type"], question_type);
        }
    }

    #[test]
    fn maps_choice_and_preserves_probabilities() {
        let response = map_response(
            &request(BTreeMap::new()),
            &json!({
                "model": "gpt-6-luna",
                "answers": {
                    "decision": {
                        "type": "choice",
                        "choice": "candidate_1",
                        "probabilities": {"candidate_0": 0.2, "candidate_1": 0.8},
                        "confidence": 0.8
                    }
                }
            }),
            Duration::from_millis(4),
        )
        .unwrap();
        assert!(matches!(
            &response.outcome,
            DecisionOutcome::Select { value } if value == &json!({"id": "b"})
        ));
        assert_eq!(response.confidence, Some(0.8));
        assert_eq!(
            response.evidence[0].metadata["probabilities"],
            r#"{"candidate_0":0.2,"candidate_1":0.8}"#
        );
    }

    #[test]
    fn rejects_unknown_choice_candidate() {
        let error = map_response(
            &request(BTreeMap::new()),
            &json!({"answers": {"decision": {
                "type": "choice", "choice": "candidate_2"
            }}}),
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            DecisionError::InvalidResponse(message) if message.contains("candidate_2")
        ));
    }

    #[test]
    fn maps_predicate_and_score() {
        let predicate = map_response(
            &request(BTreeMap::new()),
            &json!({"answers": {"decision": {
                "type": "predicate", "predicate": true, "confidence": 0.9
            }}}),
            Duration::ZERO,
        )
        .unwrap();
        assert!(matches!(
            &predicate.outcome,
            DecisionOutcome::Select { value } if value == &json!(true)
        ));

        let score = map_response(
            &request(BTreeMap::new()),
            &json!({"answers": {"decision": {
                "type": "score", "score": 0.75, "confidence": 0.7
            }}}),
            Duration::ZERO,
        )
        .unwrap();
        assert!(matches!(
            &score.outcome,
            DecisionOutcome::Select { value } if value == &json!(0.75)
        ));
    }

    #[test]
    fn rejects_unknown_answer_type() {
        let error = map_response(
            &request(BTreeMap::new()),
            &json!({"answers": {"decision": {"type": "other"}}}),
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(matches!(error, DecisionError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn sends_bearer_auth_and_maps_successful_response() {
        let provider = OpenAiDecisionsProvider::new(
            OpenAiDecisionsConfig::new(auth_test_server(), "gpt-6-luna").with_api_key("secret"),
        )
        .expect("provider");
        let response = provider
            .decide(request(BTreeMap::new()), DecisionContext::new())
            .await
            .expect("decision");
        assert!(matches!(
            response.outcome,
            DecisionOutcome::Select { value } if value == json!({"id": "b"})
        ));
    }

    #[tokio::test]
    async fn maps_non_success_response_to_provider_error() {
        let endpoint = test_server(
            Some(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 7\r\nConnection: close\r\n\r\ninvalid",
            ),
            Duration::ZERO,
        );
        let provider = OpenAiDecisionsProvider::new(OpenAiDecisionsConfig::new(endpoint, "test"))
            .expect("provider");
        let result = provider
            .decide(request(BTreeMap::new()), DecisionContext::new())
            .await;
        assert!(matches!(result, Err(DecisionError::Provider(message)) if message.contains("401")));
    }

    #[tokio::test]
    async fn rejects_oversized_request_before_http_call() {
        let provider = OpenAiDecisionsProvider::new(
            OpenAiDecisionsConfig::new("http://127.0.0.1:1", "test").with_max_request_bytes(1),
        )
        .expect("provider");
        let result = provider
            .decide(request(BTreeMap::new()), DecisionContext::new())
            .await;
        assert!(matches!(result, Err(DecisionError::ResourceExhausted(_))));
    }

    #[tokio::test]
    async fn rejects_response_exceeding_content_length_limit() {
        let endpoint = test_server(
            Some(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n123456"),
            Duration::ZERO,
        );
        let provider = OpenAiDecisionsProvider::new(
            OpenAiDecisionsConfig::new(endpoint, "test").with_max_response_bytes(5),
        )
        .expect("provider");
        let result = provider
            .decide(request(BTreeMap::new()), DecisionContext::new())
            .await;
        assert!(matches!(result, Err(DecisionError::ResourceExhausted(_))));
    }

    #[tokio::test]
    async fn rejects_response_exceeding_incremental_limit_without_content_length() {
        let endpoint = test_server(
            Some(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n123456"),
            Duration::ZERO,
        );
        let provider = OpenAiDecisionsProvider::new(
            OpenAiDecisionsConfig::new(endpoint, "test").with_max_response_bytes(5),
        )
        .expect("provider");
        let result = provider
            .decide(request(BTreeMap::new()), DecisionContext::new())
            .await;
        assert!(matches!(result, Err(DecisionError::ResourceExhausted(_))));
    }

    #[tokio::test]
    async fn propagates_cancellation_and_configured_timeout_to_http_request() {
        let provider = OpenAiDecisionsProvider::new(
            OpenAiDecisionsConfig::new(test_server(None, Duration::from_millis(250)), "test")
                .with_timeout(Duration::from_secs(2)),
        )
        .expect("provider");
        let context = DecisionContext::new();
        let cancellation = context.cancellation_token();
        let task = tokio::spawn({
            let provider = provider.clone();
            let context = context.clone();
            async move { provider.decide(request(BTreeMap::new()), context).await }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancellation.cancel();
        assert!(matches!(
            task.await.expect("cancel task"),
            Err(DecisionError::Cancelled)
        ));

        let provider = OpenAiDecisionsProvider::new(
            OpenAiDecisionsConfig::new(test_server(None, Duration::from_millis(250)), "test")
                .with_timeout(Duration::from_millis(30)),
        )
        .expect("provider");
        let result = provider
            .decide(request(BTreeMap::new()), DecisionContext::new())
            .await;
        assert!(matches!(result, Err(DecisionError::DeadlineExceeded)));
    }

    #[tokio::test]
    async fn classifies_body_read_timeout_as_deadline_exceeded() {
        let endpoint = drip_test_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n",
            &[b"{}"],
            Duration::from_millis(400),
        );
        let provider = OpenAiDecisionsProvider::new(
            OpenAiDecisionsConfig::new(endpoint, "test").with_timeout(Duration::from_millis(100)),
        )
        .expect("provider");
        let result = provider
            .decide(request(BTreeMap::new()), DecisionContext::new())
            .await;
        assert!(matches!(result, Err(DecisionError::DeadlineExceeded)));
    }

    #[tokio::test]
    async fn deadline_expiring_during_bounded_body_read_is_not_reported_as_success() {
        let endpoint = drip_test_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n",
            &[b"{}", b"{}", b"{}", b"{}"],
            Duration::from_millis(8),
        );
        let provider = OpenAiDecisionsProvider::new(
            OpenAiDecisionsConfig::new(endpoint, "test").with_timeout(Duration::from_secs(5)),
        )
        .expect("provider");
        let result = provider
            .decide(
                request(BTreeMap::new()),
                DecisionContext::with_timeout(Duration::from_millis(20)),
            )
            .await;
        assert!(matches!(result, Err(DecisionError::DeadlineExceeded)));
    }
}
