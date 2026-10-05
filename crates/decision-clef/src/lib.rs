//! Adapter for Cloudflare's Clef and Clef-flash decision models.
//!
//! Clef uses the System One-shaped `state/questions` request, but its
//! response answers are untagged. This crate keeps that wire detail outside
//! the provider-neutral decision-core contract.

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

pub const QUESTION_TYPE_METADATA: &str = "cloudflare_clef.question_type";
const QUESTION_ID: &str = "decision";
pub const DEFAULT_MODEL: &str = "clef";
const DEFAULT_QUESTION_TYPE: &str = "choice";

#[derive(Clone)]
pub struct ClefConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}

impl fmt::Debug for ClefConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClefConfig")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("timeout", &self.timeout)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

impl ClefConfig {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            api_key: None,
            timeout: Duration::from_secs(30),
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

    pub fn with_max_request_bytes(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }

    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }
}

pub struct ClefProvider {
    client: reqwest::Client,
    config: ClefConfig,
}

impl ClefProvider {
    pub fn new(config: ClefConfig) -> Result<Self, DecisionError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| DecisionError::Provider(error.to_string()))?;
        Ok(Self { client, config })
    }
}

#[async_trait]
impl DecisionProvider<Value, Value> for ClefProvider {
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
        if let Some(api_key) = &self.config.api_key {
            let value = format!("Bearer {api_key}")
                .parse()
                .map_err(|_| DecisionError::InvalidRequest("invalid api key".into()))?;
            headers.insert(AUTHORIZATION, value);
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
        context.check()?;
        if !status.is_success() {
            return Err(DecisionError::Provider(format!(
                "Cloudflare Clef endpoint returned {status}: {}",
                body.chars().take(512).collect::<String>()
            )));
        }

        let wire: ClefResponse = serde_json::from_str(&body)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?;
        let decision = map_response(&wire, &request, &self.config.model, started.elapsed())?;
        decision.validate(&request.schema_version)?;
        Ok(decision)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClefResponse {
    #[serde(default)]
    pub answers: BTreeMap<String, Value>,
    #[serde(default, rename = "providerMetadata")]
    pub provider_metadata: Option<Value>,
    #[serde(default)]
    pub result: Option<Box<ClefResponse>>,
}

#[derive(Debug, Serialize)]
struct ClefRequest<'a> {
    model: &'a str,
    state: &'a Value,
    questions: BTreeMap<String, ClefQuestion>,
}

#[derive(Debug, Serialize)]
struct ClefQuestion {
    #[serde(rename = "type")]
    question_type: &'static str,
    instructions: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    criteria: Option<ClefCriteria>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ClefCriteria {
    Choice(Map<String, Value>),
    Score(Vec<String>),
}

fn build_request_body<I, C>(
    model: &str,
    request: &DecisionRequest<I, C>,
) -> Result<Value, DecisionError>
where
    I: Serialize,
    C: Serialize,
{
    let question_type = request
        .metadata
        .get(QUESTION_TYPE_METADATA)
        .map(String::as_str)
        .unwrap_or(DEFAULT_QUESTION_TYPE);
    if !matches!(question_type, "noul" | "choice" | "score") {
        return Err(DecisionError::InvalidRequest(format!(
            "unsupported Cloudflare Clef question type {question_type}"
        )));
    }

    let criteria = match question_type {
        "noul" => None,
        "choice" => {
            let mut values = Map::new();
            for (index, candidate) in request.candidates.iter().enumerate() {
                values.insert(
                    format!("candidate_{index}"),
                    Value::String(candidate_description(candidate)?),
                );
            }
            Some(ClefCriteria::Choice(values))
        }
        "score" => Some(ClefCriteria::Score(
            request
                .candidates
                .iter()
                .map(candidate_description)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        _ => unreachable!("question type validated above"),
    };

    let question = ClefQuestion {
        question_type: match question_type {
            "noul" => "noul",
            "choice" => "choice",
            "score" => "score",
            _ => unreachable!(),
        },
        instructions: request.schema.clone(),
        criteria,
    };
    let mut questions = BTreeMap::new();
    questions.insert(QUESTION_ID.to_string(), question);
    serde_json::to_value(ClefRequest {
        model,
        state: &serde_json::to_value(&request.input)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?,
        questions,
    })
    .map_err(|error| DecisionError::Serialization(error.to_string()))
}

fn candidate_description<C: Serialize>(candidate: &C) -> Result<String, DecisionError> {
    let value = serde_json::to_value(candidate)
        .map_err(|error| DecisionError::Serialization(error.to_string()))?;
    Ok(value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string()))
}

pub fn map_response<I, C: Serialize>(
    response: &ClefResponse,
    request: &DecisionRequest<I, C>,
    model: &str,
    elapsed: Duration,
) -> Result<DecisionResponse<Value>, DecisionError> {
    let response = response.result.as_deref().unwrap_or(response);
    let answer = response.answers.get(QUESTION_ID).ok_or_else(|| {
        DecisionError::InvalidResponse("Cloudflare Clef response has no decision answer".into())
    })?;
    let question_type = request
        .metadata
        .get(QUESTION_TYPE_METADATA)
        .map(String::as_str)
        .unwrap_or(DEFAULT_QUESTION_TYPE);

    let (outcome, confidence, summary) = match question_type {
        "noul" => {
            let probability = answer
                .get("noul")
                .or_else(|| answer.get("yes"))
                .or_else(|| answer.get("probability"))
                .and_then(value_as_f64)
                .ok_or_else(|| {
                    DecisionError::InvalidResponse(
                        "Cloudflare Clef noul answer has no probability".into(),
                    )
                })?;
            ensure_probability("noul probability", probability)?;
            (
                DecisionOutcome::Select {
                    value: Value::Bool(probability >= 0.5),
                },
                Some(probability.max(1.0 - probability) as f32),
                format!("Cloudflare Clef noul answer (yes probability {probability:.4})"),
            )
        }
        "choice" => {
            let choice = answer
                .get("choice")
                .or_else(|| answer.get("value"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    DecisionError::InvalidResponse(
                        "Cloudflare Clef choice answer has no choice label".into(),
                    )
                })?;
            let value = resolve_choice(choice, request.candidates.iter())?;
            let confidence = answer.get("confidence").and_then(value_as_f64).or_else(|| {
                answer
                    .get("probabilities")
                    .and_then(|probabilities| probabilities.get(choice))
                    .and_then(value_as_f64)
            });
            if let Some(confidence) = confidence {
                ensure_probability("choice confidence", confidence)?;
            }
            (
                DecisionOutcome::Select { value },
                confidence.map(|confidence| confidence as f32),
                format!("Cloudflare Clef choice answer: {choice}"),
            )
        }
        "score" => {
            let score = answer
                .get("score")
                .or_else(|| answer.get("value"))
                .and_then(value_as_f64)
                .ok_or_else(|| {
                    DecisionError::InvalidResponse(
                        "Cloudflare Clef score answer has no score".into(),
                    )
                })?;
            if !score.is_finite() {
                return Err(DecisionError::InvalidResponse(
                    "Cloudflare Clef score must be finite".into(),
                ));
            }
            let confidence = answer.get("confidence").and_then(value_as_f64);
            if let Some(confidence) = confidence {
                ensure_probability("score confidence", confidence)?;
            }
            (
                DecisionOutcome::Select {
                    value: Value::from(score),
                },
                confidence.map(|confidence| confidence as f32),
                format!("Cloudflare Clef score answer: {score}"),
            )
        }
        _ => {
            return Err(DecisionError::InvalidRequest(format!(
                "unsupported Cloudflare Clef question type {question_type}"
            )))
        }
    };

    Ok(DecisionResponse {
        schema_version: request.schema_version.clone(),
        outcome,
        confidence,
        evidence: vec![Evidence::new("provider", summary)],
        provenance: Provenance {
            provider: "cloudflare-clef".into(),
            model: Some(model.to_owned()),
            policy: None,
            request_id: Some(request.request_id.clone()),
        },
        elapsed_ms: Some(elapsed.as_millis() as u64),
    })
}

fn resolve_choice<'a, C, I>(choice: &str, candidates: I) -> Result<Value, DecisionError>
where
    C: Serialize + 'a,
    I: IntoIterator<Item = &'a C>,
{
    let candidates = candidates.into_iter().collect::<Vec<_>>();
    if let Some(index) = choice
        .strip_prefix("candidate_")
        .and_then(|value| value.parse::<usize>().ok())
    {
        let candidate = candidates.get(index).ok_or_else(|| {
            DecisionError::InvalidResponse(format!(
                "Cloudflare Clef returned out-of-range choice {choice}"
            ))
        })?;
        return serde_json::to_value(candidate)
            .map_err(|error| DecisionError::Serialization(error.to_string()));
    }

    for candidate in candidates {
        let value = serde_json::to_value(candidate)
            .map_err(|error| DecisionError::Serialization(error.to_string()))?;
        if value.as_str() == Some(choice) || value.to_string() == choice {
            return Ok(value);
        }
    }
    Err(DecisionError::InvalidResponse(format!(
        "Cloudflare Clef returned unknown choice {choice}"
    )))
}

fn value_as_f64(value: &Value) -> Option<f64> {
    value.as_f64()
}

fn ensure_probability(name: &str, value: f64) -> Result<(), DecisionError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(DecisionError::InvalidResponse(format!(
            "{name} must be a finite value between 0 and 1"
        )));
    }
    Ok(())
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

    fn request(question_type: Option<&str>) -> DecisionRequest<Value, Value> {
        let mut metadata = BTreeMap::new();
        if let Some(question_type) = question_type {
            metadata.insert(QUESTION_TYPE_METADATA.into(), question_type.into());
        }
        DecisionRequest {
            request_id: "req-1".into(),
            input: serde_json::json!({"text": "hello"}),
            candidates: vec![serde_json::json!("billing"), serde_json::json!("technical")],
            schema: "Which team?".into(),
            schema_version: "1".into(),
            metadata,
            deadline_ms: None,
        }
    }

    fn response(answer: Value) -> ClefResponse {
        ClefResponse {
            answers: BTreeMap::from([("decision".into(), answer)]),
            provider_metadata: None,
            result: None,
        }
    }

    fn test_server(response: &'static [u8], delay: Duration, require_auth: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..length]);
            if require_auth {
                assert!(request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-token"));
            }
            assert!(request.contains("\"model\":\"clef\""));
            thread::sleep(delay);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n",
                response.len()
            );
            stream.write_all(header.as_bytes()).expect("write header");
            stream.write_all(response).expect("write response");
        });
        format!("http://{address}")
    }

    #[test]
    fn builds_choice_score_and_noul_questions() {
        let choice = build_request_body("clef", &request(None)).expect("choice body");
        assert_eq!(choice["questions"]["decision"]["type"], "choice");
        assert_eq!(
            choice["questions"]["decision"]["criteria"]["candidate_0"],
            "billing"
        );

        let score_request = request(Some("score"));
        let score = build_request_body("clef", &score_request).expect("score body");
        assert_eq!(score["questions"]["decision"]["type"], "score");
        assert_eq!(score["questions"]["decision"]["criteria"][1], "technical");

        let noul_request = request(Some("noul"));
        let noul = build_request_body("clef", &noul_request).expect("noul body");
        assert_eq!(noul["questions"]["decision"]["type"], "noul");
        assert!(noul["questions"]["decision"].get("criteria").is_none());
    }

    #[test]
    fn maps_choice_and_result_envelope() {
        let mut wire = response(serde_json::json!({
            "choice": "candidate_1",
            "probabilities": {"candidate_1": 0.8},
            "confidence": 0.8
        }));
        let mapped = map_response(&wire, &request(None), "clef", Duration::from_millis(4))
            .expect("choice response");
        assert!(matches!(
            mapped.outcome,
            DecisionOutcome::Select { value } if value == serde_json::json!("technical")
        ));
        assert_eq!(mapped.provenance.provider, "cloudflare-clef");
        assert_eq!(mapped.provenance.model.as_deref(), Some("clef"));

        wire = ClefResponse {
            answers: BTreeMap::new(),
            provider_metadata: None,
            result: Some(Box::new(response(serde_json::json!({
                "choice": "billing"
            })))),
        };
        let mapped = map_response(&wire, &request(None), "clef", Duration::ZERO)
            .expect("enveloped response");
        assert!(matches!(
            mapped.outcome,
            DecisionOutcome::Select { value } if value == serde_json::json!("billing")
        ));
    }

    #[test]
    fn maps_noul_and_score_with_confidence_validation() {
        let mapped = map_response(
            &response(serde_json::json!({"noul": 0.9})),
            &request(Some("noul")),
            "clef-flash",
            Duration::ZERO,
        )
        .expect("noul response");
        assert!(matches!(
            mapped.outcome,
            DecisionOutcome::Select { value } if value == serde_json::json!(true)
        ));
        assert_eq!(mapped.confidence, Some(0.9));

        let mapped = map_response(
            &response(serde_json::json!({"score": 2.5, "confidence": 0.75})),
            &request(Some("score")),
            "clef",
            Duration::ZERO,
        )
        .expect("score response");
        assert!(matches!(
            mapped.outcome,
            DecisionOutcome::Select { value } if value == serde_json::json!(2.5)
        ));
        assert_eq!(mapped.confidence, Some(0.75));

        let error = map_response(
            &response(serde_json::json!({"noul": 1.5})),
            &request(Some("noul")),
            "clef",
            Duration::ZERO,
        )
        .expect_err("invalid probability");
        assert!(
            matches!(error, DecisionError::InvalidResponse(message) if message.contains("between 0 and 1"))
        );
    }

    #[test]
    fn preserves_f64_score_and_noul_threshold_precision() {
        let score = 0.1_f64;
        let mapped = map_response(
            &response(serde_json::json!({"score": score})),
            &request(Some("score")),
            "clef",
            Duration::ZERO,
        )
        .expect("score response");
        assert!(matches!(
            mapped.outcome,
            DecisionOutcome::Select { value } if value.as_f64() == Some(score)
        ));
        assert!(mapped.evidence[0].summary.contains("0.1"));

        let mapped = map_response(
            &response(serde_json::json!({"noul": 0.49999999})),
            &request(Some("noul")),
            "clef-flash",
            Duration::ZERO,
        )
        .expect("noul response");
        assert!(matches!(
            mapped.outcome,
            DecisionOutcome::Select { value } if value == serde_json::json!(false)
        ));
    }

    #[tokio::test]
    async fn sends_bearer_request_and_maps_response() {
        let endpoint = test_server(
            br#"{"answers":{"decision":{"choice":"candidate_0","confidence":0.9}}}"#,
            Duration::ZERO,
            true,
        );
        let provider = ClefProvider::new(
            ClefConfig::new(endpoint, DEFAULT_MODEL)
                .with_api_key("test-token")
                .with_timeout(Duration::from_secs(1)),
        )
        .expect("provider");
        let response = provider
            .decide(
                request(None),
                DecisionContext::with_timeout(Duration::from_secs(1)),
            )
            .await
            .expect("decision");
        assert!(matches!(
            response.outcome,
            DecisionOutcome::Select { value } if value == serde_json::json!("billing")
        ));
    }

    #[tokio::test]
    async fn enforces_deadline_and_response_limit() {
        let endpoint = test_server(
            br#"{"answers":{"decision":{"choice":"candidate_0"}}}"#,
            Duration::from_millis(200),
            false,
        );
        let provider = ClefProvider::new(
            ClefConfig::new(endpoint, DEFAULT_MODEL).with_timeout(Duration::from_secs(1)),
        )
        .expect("provider");
        let error = provider
            .decide(
                request(None),
                DecisionContext::with_timeout(Duration::from_millis(20)),
            )
            .await
            .expect_err("deadline");
        assert!(matches!(error, DecisionError::DeadlineExceeded));

        let endpoint = test_server(
            br#"{"answers":{"decision":{"choice":"candidate_0"}}}"#,
            Duration::ZERO,
            false,
        );
        let provider =
            ClefProvider::new(ClefConfig::new(endpoint, DEFAULT_MODEL).with_max_response_bytes(8))
                .expect("provider");
        let error = provider
            .decide(
                request(None),
                DecisionContext::with_timeout(Duration::from_secs(1)),
            )
            .await
            .expect_err("response limit");
        assert!(matches!(error, DecisionError::ResourceExhausted(_)));
    }
}
