//! Adapter for Jev's typed decision HTTP endpoint.
//!
//! The wire format is intentionally kept at this boundary. Jev-specific
//! `choice`, `score`, and `noul` fields are converted into core outcomes.

use async_trait::async_trait;
use decision_core::{
    DecisionContext, DecisionError, DecisionOutcome, DecisionProvider, DecisionRequest,
    DecisionResponse, Evidence, Provenance,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct JevConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}

impl fmt::Debug for JevConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JevConfig")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("timeout", &self.timeout)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

impl JevConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: "typesafe-ai/jev".into(),
            api_key: None,
            timeout: Duration::from_secs(30),
            max_request_bytes: 256 * 1024,
            max_response_bytes: 512 * 1024,
        }
    }

    pub fn vercel_gateway(api_key: impl Into<String>) -> Self {
        Self::new("https://ai-gateway.vercel.sh/v4/ai/evaluation-model")
            .with_model("typesafe-ai/jev")
            .with_api_key(api_key)
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
pub struct JevProvider {
    client: reqwest::Client,
    config: JevConfig,
}

impl JevProvider {
    pub fn new(config: JevConfig) -> Result<Self, DecisionError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| DecisionError::Transport(error.to_string()))?;
        Ok(Self { client, config })
    }

    pub fn config(&self) -> &JevConfig {
        &self.config
    }
}

#[async_trait]
impl DecisionProvider<Value, Value> for JevProvider {
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
        headers.insert(
            "ai-gateway-protocol-version",
            HeaderValue::from_static("0.0.1"),
        );
        headers.insert(
            "ai-evaluation-model-specification-version",
            HeaderValue::from_static("4"),
        );
        let model = HeaderValue::from_str(&self.config.model)
            .map_err(|_| DecisionError::InvalidRequest("invalid Jev model".into()))?;
        headers.insert("ai-model-id", model);
        if let Some(api_key) = &self.config.api_key {
            let value = format!("Bearer {api_key}")
                .parse()
                .map_err(|_| DecisionError::InvalidRequest("invalid api key".into()))?;
            headers.insert(AUTHORIZATION, value);
        }

        let wire_request = JevRequest::from_request(&request)?;
        let body = serde_json::to_vec(&wire_request)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?;
        if body.len() > self.config.max_request_bytes {
            return Err(DecisionError::ResourceExhausted(format!(
                "request body exceeds {} bytes",
                self.config.max_request_bytes
            )));
        }
        let response = send_with_context(
            self.client
                .post(&self.config.endpoint)
                .headers(headers)
                .timeout(
                    context
                        .remaining()
                        .map(|remaining| remaining.min(self.config.timeout))
                        .unwrap_or(self.config.timeout),
                )
                .body(body),
            &context,
        )
        .await?;
        context.check()?;
        let status = response.status();
        let body = read_bounded(response, &context, self.config.max_response_bytes).await?;
        // Buffered chunks can complete the read without the polling timer
        // firing, so re-check the context before treating this as a success.
        context.check()?;
        if !status.is_success() {
            return Err(DecisionError::Provider(format!(
                "jev endpoint returned {status}: {}",
                body.chars().take(512).collect::<String>()
            )));
        }

        let wire: JevResponse = serde_json::from_str(&body)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?;
        let decision = map_response(wire, &request, started.elapsed())?;
        decision.validate(&request.schema_version)?;
        Ok(decision)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct JevResponse {
    pub answers: BTreeMap<String, JevAnswer>,
    #[serde(default, rename = "providerMetadata")]
    pub provider_metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum JevAnswer {
    #[serde(rename = "choice")]
    Choice {
        choice: String,
        #[serde(default)]
        probabilities: BTreeMap<String, f32>,
    },
    #[serde(rename = "score")]
    Score { score: f32 },
    #[serde(rename = "boolean")]
    Boolean { probability: f32 },
}

#[derive(Debug, Serialize)]
struct JevRequest<'a> {
    state: &'a Value,
    questions: BTreeMap<String, JevQuestion>,
}

#[derive(Debug, Serialize)]
struct JevQuestion {
    #[serde(rename = "type")]
    question_type: &'static str,
    instructions: String,
    criteria: Map<String, Value>,
}

impl<'a> JevRequest<'a> {
    fn from_request(request: &'a DecisionRequest<Value, Value>) -> Result<Self, DecisionError> {
        let mut criteria = Map::new();
        for (index, candidate) in request.candidates.iter().enumerate() {
            criteria.insert(
                format!("candidate_{index}"),
                Value::String(candidate.to_string()),
            );
        }

        let mut questions = BTreeMap::new();
        questions.insert(
            "decision".into(),
            JevQuestion {
                question_type: "choice",
                instructions: request.schema.clone(),
                criteria,
            },
        );
        Ok(Self {
            state: &request.input,
            questions,
        })
    }
}

pub fn map_response<I, C: Serialize>(
    response: JevResponse,
    request: &DecisionRequest<I, C>,
    elapsed: Duration,
) -> Result<DecisionResponse<Value>, DecisionError> {
    let answer = response.answers.get("decision").ok_or_else(|| {
        DecisionError::InvalidResponse("jev response has no decision answer".into())
    })?;
    let (outcome, confidence) = match answer {
        JevAnswer::Choice {
            choice,
            probabilities,
        } => {
            let index = choice
                .strip_prefix("candidate_")
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| {
                    DecisionError::InvalidResponse(format!("jev returned unknown choice {choice}"))
                })?;
            let value = serde_json::to_value(request.candidates.get(index).ok_or_else(|| {
                DecisionError::InvalidResponse(format!("jev returned out-of-range choice {choice}"))
            })?)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?;
            (
                DecisionOutcome::Select { value },
                probabilities.get(choice).copied(),
            )
        }
        JevAnswer::Score { score } => (
            DecisionOutcome::Abstain {
                reason: format!("jev returned score {score} for a choice request"),
            },
            None,
        ),
        JevAnswer::Boolean { probability } => (
            DecisionOutcome::Abstain {
                reason: format!(
                    "jev returned boolean probability {probability} for a choice request"
                ),
            },
            Some(*probability),
        ),
    };

    Ok(DecisionResponse {
        schema_version: request.schema_version.clone(),
        outcome,
        confidence,
        evidence: vec![Evidence::new("provider", "Jev evaluation response")],
        provenance: Provenance {
            provider: "jev".into(),
            model: None,
            policy: None,
            request_id: Some(request.request_id.clone()),
        },
        elapsed_ms: Some(elapsed.as_millis() as u64),
    })
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

fn map_request_error(error: reqwest::Error, context: &DecisionContext) -> DecisionError {
    if error.is_timeout() {
        DecisionError::DeadlineExceeded
    } else if context.is_cancelled() {
        DecisionError::Cancelled
    } else {
        DecisionError::Transport(error.to_string())
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
        .map_err(|error| map_request_error(error, context))?;
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
    use decision_core::DecisionProvider;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

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

    fn request() -> DecisionRequest<Value, Value> {
        DecisionRequest {
            request_id: "req-1".into(),
            input: serde_json::json!({"text":"hello"}),
            candidates: vec![serde_json::json!("greeting")],
            schema: "classification".into(),
            schema_version: "1".into(),
            metadata: BTreeMap::new(),
            deadline_ms: None,
        }
    }

    #[test]
    fn maps_choice_to_select() {
        let result = map_response(
            JevResponse {
                answers: BTreeMap::from([(
                    "decision".into(),
                    JevAnswer::Choice {
                        choice: "candidate_0".into(),
                        probabilities: BTreeMap::from([("candidate_0".into(), 0.9)]),
                    },
                )]),
                provider_metadata: None,
            },
            &request(),
            Duration::from_millis(4),
        )
        .expect("mapping");
        assert!(matches!(result.outcome, DecisionOutcome::Select { .. }));
        assert_eq!(result.provenance.provider, "jev");
    }

    #[test]
    fn maps_selected_choice_probability_to_confidence() {
        let result = map_response(
            JevResponse {
                answers: BTreeMap::from([(
                    "decision".into(),
                    JevAnswer::Choice {
                        choice: "candidate_0".into(),
                        probabilities: BTreeMap::from([
                            ("candidate_0".into(), 0.4),
                            ("candidate_1".into(), 0.6),
                        ]),
                    },
                )]),
                provider_metadata: None,
            },
            &request(),
            Duration::ZERO,
        )
        .expect("mapping");
        assert_eq!(result.confidence, Some(0.4));
    }

    #[test]
    fn redacts_api_key_in_debug_output() {
        let debug = format!(
            "{:?}",
            JevConfig::new("https://example.test").with_api_key("secret")
        );
        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn maps_noul_to_abstain() {
        let result = map_response(
            JevResponse {
                answers: BTreeMap::from([(
                    "decision".into(),
                    JevAnswer::Boolean { probability: 0.1 },
                )]),
                provider_metadata: None,
            },
            &request(),
            Duration::ZERO,
        )
        .expect("mapping");
        assert!(matches!(
            result.outcome,
            DecisionOutcome::Abstain { ref reason } if reason.contains("boolean")
        ));
    }

    #[test]
    fn builds_vercel_evaluation_request() {
        let request = request();
        let wire = JevRequest::from_request(&request).expect("request");
        assert_eq!(wire.questions["decision"].question_type, "choice");
        assert_eq!(
            wire.questions["decision"].criteria["candidate_0"],
            serde_json::json!("\"greeting\"")
        );
    }

    #[tokio::test]
    async fn calls_jev_through_vercel_gateway() {
        let Some(api_key) = std::env::var("HOLON_LIVE_DECISION_JEV_VERCEL_API_KEY")
            .or_else(|_| std::env::var("AI_GATEWAY_API_KEY"))
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            eprintln!(
                "skipping live decision-jev test: HOLON_LIVE_DECISION_JEV_VERCEL_API_KEY or \
                 AI_GATEWAY_API_KEY is not configured"
            );
            return;
        };
        let provider = JevProvider::new(JevConfig::vercel_gateway(api_key)).expect("provider");
        let result = provider
            .decide(request(), DecisionContext::default())
            .await
            .expect("Jev response");
        assert!(matches!(result.outcome, DecisionOutcome::Select { .. }));
    }

    #[tokio::test]
    async fn rejects_oversized_request_before_http_call() {
        let provider =
            JevProvider::new(JevConfig::new("http://127.0.0.1:1").with_max_request_bytes(1))
                .expect("provider");
        let result = provider.decide(request(), DecisionContext::new()).await;
        assert!(matches!(result, Err(DecisionError::ResourceExhausted(_))));
    }

    #[tokio::test]
    async fn rejects_response_exceeding_content_length_limit() {
        let endpoint = test_server(
            Some(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n123456"),
            Duration::ZERO,
        );
        let provider =
            JevProvider::new(JevConfig::new(endpoint).with_max_response_bytes(5)).expect("provider");
        let result = provider.decide(request(), DecisionContext::new()).await;
        assert!(matches!(result, Err(DecisionError::ResourceExhausted(_))));
    }

    #[tokio::test]
    async fn rejects_response_exceeding_incremental_limit_without_content_length() {
        let endpoint =
            test_server(Some(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n123456"), Duration::ZERO);
        let provider =
            JevProvider::new(JevConfig::new(endpoint).with_max_response_bytes(5)).expect("provider");
        let result = provider.decide(request(), DecisionContext::new()).await;
        assert!(matches!(result, Err(DecisionError::ResourceExhausted(_))));
    }

    #[tokio::test]
    async fn propagates_cancellation_and_deadline_to_http_request() {
        let provider = JevProvider::new(
            JevConfig::new(test_server(None, Duration::from_millis(250)))
                .with_timeout(Duration::from_secs(2)),
        )
        .expect("provider");
        let context = DecisionContext::new();
        let cancellation = context.cancellation_token();
        let task = tokio::spawn({
            let provider = provider.clone();
            let context = context.clone();
            async move { provider.decide(request(), context).await }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancellation.cancel();
        assert!(matches!(
            task.await.expect("cancel task"),
            Err(DecisionError::Cancelled)
        ));

        let provider = JevProvider::new(
            JevConfig::new(test_server(None, Duration::from_millis(250)))
                .with_timeout(Duration::from_secs(2)),
        )
        .expect("provider");
        let result = provider
            .decide(request(), DecisionContext::with_timeout(Duration::from_millis(30)))
            .await;
        assert!(matches!(result, Err(DecisionError::DeadlineExceeded)));
    }

    #[tokio::test]
    async fn classifies_body_read_timeout_as_deadline_exceeded() {
        let endpoint = drip_test_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n",
            &[b"xx"],
            Duration::from_millis(400),
        );
        let provider = JevProvider::new(
            JevConfig::new(endpoint).with_timeout(Duration::from_millis(100)),
        )
        .expect("provider");
        let result = provider.decide(request(), DecisionContext::new()).await;
        assert!(matches!(result, Err(DecisionError::DeadlineExceeded)));
    }

    #[tokio::test]
    async fn deadline_expiring_during_bounded_body_read_is_not_reported_as_success() {
        let endpoint = drip_test_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n",
            &[b"xx", b"xx", b"xx", b"xx"],
            Duration::from_millis(8),
        );
        let provider = JevProvider::new(
            JevConfig::new(endpoint).with_timeout(Duration::from_secs(5)),
        )
        .expect("provider");
        let result = provider
            .decide(
                request(),
                DecisionContext::with_timeout(Duration::from_millis(20)),
            )
            .await;
        assert!(matches!(result, Err(DecisionError::DeadlineExceeded)));
    }
}
