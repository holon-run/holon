//! Provider-agnostic contracts for typed decision providers.
//!
//! This crate deliberately does not decide whether a caller should apply a
//! result. `off`, `shadow`, and authoritative application are integration
//! policies owned by the caller.

use async_trait::async_trait;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use thiserror::Error;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DecisionRequest<I, C> {
    pub request_id: String,
    pub input: I,
    pub candidates: Vec<C>,
    pub schema: String,
    pub schema_version: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub metadata: BTreeMap<String, String>,
    /// Relative deadline in milliseconds. Runtime enforcement uses
    /// `DecisionContext`; this field is part of the provider wire contract.
    #[cfg_attr(feature = "serde", serde(default))]
    pub deadline_ms: Option<u64>,
}

impl<I, C> DecisionRequest<I, C> {
    pub fn validate(&self) -> Result<(), DecisionError> {
        if self.request_id.trim().is_empty() {
            return Err(DecisionError::InvalidRequest("request_id is empty".into()));
        }
        if self.schema.trim().is_empty() {
            return Err(DecisionError::InvalidRequest("schema is empty".into()));
        }
        if self.schema_version.trim().is_empty() {
            return Err(DecisionError::InvalidRequest(
                "schema_version is empty".into(),
            ));
        }
        if self.candidates.is_empty() {
            return Err(DecisionError::InvalidRequest(
                "candidates must not be empty".into(),
            ));
        }
        if self.deadline_ms == Some(0) {
            return Err(DecisionError::InvalidRequest(
                "deadline_ms must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub struct DecisionResponse<O> {
    pub schema_version: String,
    pub outcome: DecisionOutcome<O>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub confidence: Option<f32>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub evidence: Vec<Evidence>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub provenance: Provenance,
    #[cfg_attr(feature = "serde", serde(default))]
    pub elapsed_ms: Option<u64>,
}

impl<O> DecisionResponse<O> {
    pub fn validate(&self, expected_schema_version: &str) -> Result<(), DecisionError> {
        if self.schema_version != expected_schema_version {
            return Err(DecisionError::InvalidResponse(format!(
                "schema version mismatch: expected {expected_schema_version}, got {}",
                self.schema_version
            )));
        }
        if let Some(confidence) = self.confidence {
            if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                return Err(DecisionError::InvalidResponse(
                    "confidence must be a finite value between 0 and 1".into(),
                ));
            }
        }
        if self.provenance.provider.trim().is_empty() {
            return Err(DecisionError::InvalidResponse(
                "provenance.provider is empty".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "snake_case"))]
pub enum DecisionOutcome<O> {
    Rank { items: Vec<Ranked<O>> },
    Select { value: O },
    Abstain { reason: String },
    Fallback { value: O },
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Ranked<O> {
    pub value: O,
    pub score: f32,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Evidence {
    pub kind: String,
    pub summary: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub metadata: BTreeMap<String, String>,
}

impl Evidence {
    pub fn new(kind: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            summary: summary.into(),
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Provenance {
    pub provider: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub model: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub policy: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub request_id: Option<String>,
}

impl Provenance {
    pub fn new(provider: impl Into<String>, model: Option<String>) -> Self {
        Self {
            provider: provider.into(),
            model,
            policy: None,
            request_id: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum DecisionError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("decision was cancelled")]
    Cancelled,
    #[error("decision deadline exceeded")]
    DeadlineExceeded,
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
pub struct DecisionContext {
    deadline: Option<Instant>,
    cancellation: CancellationToken,
}

impl Default for DecisionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl DecisionContext {
    pub fn new() -> Self {
        Self {
            deadline: None,
            cancellation: CancellationToken::default(),
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            deadline: Some(Instant::now() + timeout),
            cancellation: CancellationToken::default(),
        }
    }

    pub fn with_deadline(deadline: Instant) -> Self {
        Self {
            deadline: Some(deadline),
            cancellation: CancellationToken::default(),
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub fn check(&self) -> Result<(), DecisionError> {
        if self.cancellation.is_cancelled() {
            return Err(DecisionError::Cancelled);
        }
        if self.remaining() == Some(Duration::ZERO) {
            return Err(DecisionError::DeadlineExceeded);
        }
        Ok(())
    }
}

#[async_trait]
pub trait DecisionProvider<I, C>: Send + Sync {
    type Output: Send;

    async fn decide(
        &self,
        request: DecisionRequest<I, C>,
        context: DecisionContext,
    ) -> Result<DecisionResponse<Self::Output>, DecisionError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_validation_rejects_empty_candidates() {
        let request = DecisionRequest {
            request_id: "req-1".into(),
            input: "input",
            candidates: Vec::<&str>::new(),
            schema: "test".into(),
            schema_version: "1".into(),
            metadata: BTreeMap::new(),
            deadline_ms: None,
        };
        assert!(matches!(
            request.validate(),
            Err(DecisionError::InvalidRequest(message)) if message.contains("candidates")
        ));
    }

    #[test]
    fn response_validation_checks_confidence_and_provenance() {
        let response: DecisionResponse<()> = DecisionResponse {
            schema_version: "1".into(),
            outcome: DecisionOutcome::Abstain {
                reason: "uncertain".into(),
            },
            confidence: Some(1.5),
            evidence: vec![],
            provenance: Provenance::new("test", None),
            elapsed_ms: None,
        };
        assert!(matches!(
            response.validate("1"),
            Err(DecisionError::InvalidResponse(message)) if message.contains("confidence")
        ));
    }

    #[test]
    fn cancellation_is_explicit() {
        let context = DecisionContext::new();
        context.cancellation_token().cancel();
        assert!(matches!(context.check(), Err(DecisionError::Cancelled)));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn outcome_round_trips_as_tagged_json() {
        let outcome = DecisionOutcome::Select { value: "candidate" };
        let json = serde_json::to_string(&outcome).expect("serialize");
        assert_eq!(json, r#"{"kind":"select","value":"candidate"}"#);
    }
}
