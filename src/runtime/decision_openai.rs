#[cfg(test)]
use super::scheduler::SemanticCandidateSelectionHook;
use super::scheduler::{
    AsyncSemanticCandidateSelectionHook, AutonomousContinuationCandidate,
    AutonomousContinuationProposal, AutonomousContinuationSelectionContext,
    SemanticCandidateSelectionHookError, SemanticCandidateSelectionHookErrorKind,
    SemanticCandidateSelectionHookResult,
};
use async_trait::async_trait;
use decision_core::{
    DecisionContext, DecisionError, DecisionOutcome, DecisionProvider, DecisionRequest,
    DecisionResponse,
};
use decision_openai::{OpenAiConfig, OpenAiProvider};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Semaphore;

const SCHEMA: &str = "holon.scheduler.semantic_candidate_selection";
const SCHEMA_VERSION: &str = "1";
const DEFAULT_DECISION_TIMEOUT: Duration = Duration::from_millis(1500);
const DEFAULT_DECISION_CONCURRENCY: usize = 4;
const DEFAULT_DECISION_QUEUE_CAPACITY: usize = 32;

pub(crate) struct OpenAiSemanticCandidateSelectionHook {
    executor: DecisionExecutor,
}

impl OpenAiSemanticCandidateSelectionHook {
    #[cfg(test)]
    pub(crate) fn new(config: OpenAiConfig) -> Result<Self, DecisionError> {
        Ok(Self {
            executor: DecisionExecutor::new(
                OpenAiProvider::new(config)?,
                DEFAULT_DECISION_TIMEOUT,
                DEFAULT_DECISION_CONCURRENCY,
                DEFAULT_DECISION_QUEUE_CAPACITY,
            ),
        })
    }

    pub(crate) fn from_app_config(
        config: &crate::config::AppConfig,
    ) -> anyhow::Result<Option<Self>> {
        let configured = &config.stored_config.decision;
        if !configured.enabled.unwrap_or(false) {
            return Ok(None);
        }
        let route = configured
            .route
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("decision.enabled requires decision.route"))?;
        let endpoint = route
            .endpoint
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("decision.route.endpoint is required"))?;
        let model = route
            .model
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("decision.route.model is required"))?;
        anyhow::ensure!(
            !endpoint.trim().is_empty() && !model.trim().is_empty(),
            "decision.route endpoint and model must not be empty"
        );
        let timeout = Duration::from_millis(
            configured
                .timeout_ms
                .unwrap_or(DEFAULT_DECISION_TIMEOUT.as_millis() as u64)
                .max(1),
        );
        let mut provider_config =
            OpenAiConfig::new(endpoint.to_owned(), model.to_owned()).with_timeout(timeout);
        if let Some(max_tokens) = configured.max_tokens {
            provider_config = provider_config.with_max_tokens(max_tokens);
        }
        if let Some(profile) = route.credential_profile.as_deref() {
            let store = crate::config::load_credential_store_at(
                &crate::config::credential_store_path(&config.home_dir),
            )?;
            let credential = store.profiles.get(profile).ok_or_else(|| {
                anyhow::anyhow!("decision credential profile {profile} not found")
            })?;
            anyhow::ensure!(
                matches!(
                    credential.kind,
                    crate::config::CredentialKind::ApiKey
                        | crate::config::CredentialKind::BearerToken
                ),
                "decision credential profile {profile} must contain an API key or bearer token"
            );
            provider_config = provider_config.with_api_key(credential.material.clone());
        }
        Ok(Some(Self {
            executor: DecisionExecutor::new(
                OpenAiProvider::new(provider_config)?,
                timeout,
                configured
                    .concurrency
                    .unwrap_or(DEFAULT_DECISION_CONCURRENCY)
                    .max(1),
                configured
                    .queue_capacity
                    .unwrap_or(DEFAULT_DECISION_QUEUE_CAPACITY),
            ),
        }))
    }

    fn request(
        &self,
        context: &AutonomousContinuationSelectionContext,
    ) -> Result<DecisionRequest<Value, Value>, SemanticCandidateSelectionHookError> {
        let input = json!({
            "snapshot": {
                "agent_id": context.snapshot_identity.agent_id,
                "status": context.snapshot_identity.status,
                "queue_len": context.snapshot_identity.queue_len,
                "active_run_id": context.snapshot_identity.active_run_id,
            },
            "baseline": context.baseline,
        });
        let candidates = context
            .candidates
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SemanticCandidateSelectionHookError::unknown())?;
        let mut metadata = BTreeMap::new();
        metadata.insert("integration".into(), "holon-runtime".into());
        metadata.insert("selection_policy".into(), "bounded_candidate_only".into());

        Ok(DecisionRequest {
            request_id: format!(
                "holon-scheduler:{}:{}",
                context.snapshot_identity.agent_id,
                context
                    .snapshot_identity
                    .active_run_id
                    .as_deref()
                    .unwrap_or("idle")
            ),
            input,
            candidates,
            schema: SCHEMA.into(),
            schema_version: SCHEMA_VERSION.into(),
            metadata,
            deadline_ms: Some(self.executor.timeout.as_millis() as u64),
        })
    }
}

fn response_to_hook_result(
    response: DecisionResponse<Value>,
    context: &AutonomousContinuationSelectionContext,
) -> Result<SemanticCandidateSelectionHookResult, SemanticCandidateSelectionHookError> {
    response
        .validate(SCHEMA_VERSION)
        .map_err(|_| SemanticCandidateSelectionHookError {
            kind: SemanticCandidateSelectionHookErrorKind::MalformedResponse,
        })?;
    let value = match response.outcome {
        DecisionOutcome::Select { value } => value,
        DecisionOutcome::Fallback { .. }
        | DecisionOutcome::Abstain { .. }
        | DecisionOutcome::Rank { .. } => return Ok(SemanticCandidateSelectionHookResult::Abstain),
    };
    let candidate: AutonomousContinuationCandidate =
        serde_json::from_value(value).map_err(|_| SemanticCandidateSelectionHookError {
            kind: SemanticCandidateSelectionHookErrorKind::MalformedResponse,
        })?;
    Ok(SemanticCandidateSelectionHookResult::Propose(
        AutonomousContinuationProposal {
            snapshot_identity: context.snapshot_identity.clone(),
            candidate,
        },
    ))
}

fn map_decision_error(error: DecisionError) -> SemanticCandidateSelectionHookError {
    let kind = match error {
        DecisionError::Cancelled => SemanticCandidateSelectionHookErrorKind::Cancelled,
        DecisionError::DeadlineExceeded => SemanticCandidateSelectionHookErrorKind::Timeout,
        DecisionError::ResourceExhausted(_) => {
            SemanticCandidateSelectionHookErrorKind::ResourceExhausted
        }
        DecisionError::InvalidResponse(_) | DecisionError::Serialization(_) => {
            SemanticCandidateSelectionHookErrorKind::MalformedResponse
        }
        DecisionError::InvalidRequest(_) => {
            SemanticCandidateSelectionHookErrorKind::MalformedResponse
        }
        DecisionError::Provider(_) | DecisionError::Transport(_) => {
            SemanticCandidateSelectionHookErrorKind::ProviderError
        }
    };
    SemanticCandidateSelectionHookError { kind }
}

#[async_trait]
impl AsyncSemanticCandidateSelectionHook for OpenAiSemanticCandidateSelectionHook {
    async fn select_autonomous_continuation(
        &self,
        context: &AutonomousContinuationSelectionContext,
    ) -> Result<SemanticCandidateSelectionHookResult, SemanticCandidateSelectionHookError> {
        let response = self
            .executor
            .decide(self.request(context)?)
            .await
            .map_err(map_decision_error)?;
        response_to_hook_result(response, context)
    }
}

#[derive(Clone)]
struct DecisionExecutor {
    provider: Arc<OpenAiProvider>,
    timeout: Duration,
    concurrency: Arc<Semaphore>,
    pending: Arc<AtomicUsize>,
    queue_capacity: usize,
}

impl DecisionExecutor {
    fn new(
        provider: OpenAiProvider,
        timeout: Duration,
        concurrency: usize,
        queue_capacity: usize,
    ) -> Self {
        Self {
            provider: Arc::new(provider),
            timeout,
            concurrency: Arc::new(Semaphore::new(concurrency)),
            pending: Arc::new(AtomicUsize::new(0)),
            queue_capacity,
        }
    }

    async fn decide(
        &self,
        request: DecisionRequest<Value, Value>,
    ) -> Result<DecisionResponse<Value>, DecisionError> {
        let pending = self.pending.fetch_add(1, Ordering::AcqRel);
        if pending >= self.queue_capacity {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return Err(DecisionError::Provider(
                "decision executor queue is full".into(),
            ));
        }
        let _pending_guard = PendingGuard(Arc::clone(&self.pending));
        let context = DecisionContext::with_timeout(self.timeout);
        let remaining = context.remaining().ok_or(DecisionError::DeadlineExceeded)?;
        let permit = tokio::time::timeout(remaining, self.concurrency.acquire())
            .await
            .map_err(|_| DecisionError::DeadlineExceeded)?
            .map_err(|_| DecisionError::Cancelled)?;
        let cancellation = context.cancellation_token();
        let remaining = context.remaining().ok_or(DecisionError::DeadlineExceeded)?;
        let result = tokio::time::timeout(remaining, self.provider.decide(request, context)).await;
        drop(permit);
        match result {
            Ok(result) => result,
            Err(_) => {
                cancellation.cancel();
                Err(DecisionError::DeadlineExceeded)
            }
        }
    }
}

struct PendingGuard(Arc<AtomicUsize>);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
impl SemanticCandidateSelectionHook for OpenAiSemanticCandidateSelectionHook {
    fn select_autonomous_continuation(
        &self,
        context: &AutonomousContinuationSelectionContext,
    ) -> Result<SemanticCandidateSelectionHookResult, SemanticCandidateSelectionHookError> {
        let request = self.request(context)?;
        let executor = self.executor.clone();
        let context = context.clone();
        std::thread::Builder::new()
            .name("holon-decision-openai-test".into())
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| SemanticCandidateSelectionHookError::unknown())?
                    .block_on(async {
                        let response =
                            executor.decide(request).await.map_err(map_decision_error)?;
                        response_to_hook_result(response, &context)
                    })
            })
            .map_err(|_| SemanticCandidateSelectionHookError::unknown())?
            .join()
            .map_err(|_| SemanticCandidateSelectionHookError::unknown())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::scheduler::{
            AutonomousContinuationSnapshotIdentity, SemanticCandidateSelectionHook,
        },
        types::{AgentStatus, WorkReactivationMode},
    };
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::mpsc,
        thread,
    };

    fn test_context() -> AutonomousContinuationSelectionContext {
        let candidates = vec![
            AutonomousContinuationCandidate {
                work_item_id: "active".into(),
                work_item_revision: 3,
                work_item_generation: Some(7),
                reactivation_mode: WorkReactivationMode::ContinueActive,
            },
            AutonomousContinuationCandidate {
                work_item_id: "queued".into(),
                work_item_revision: 5,
                work_item_generation: Some(2),
                reactivation_mode: WorkReactivationMode::ActivateQueued,
            },
        ];
        AutonomousContinuationSelectionContext {
            snapshot_identity: AutonomousContinuationSnapshotIdentity {
                agent_id: "default".into(),
                status: AgentStatus::AwakeIdle,
                queue_len: 2,
                active_run_id: Some("run-1".into()),
                candidates: candidates.clone(),
            },
            baseline: candidates[0].clone(),
            candidates,
        }
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end;
        loop {
            let count = stream.read(&mut chunk).expect("request");
            assert!(count > 0, "request closed before headers");
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                header_end = position + 4;
                break;
            }
        }
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_default();
        while bytes.len() < header_end + content_length {
            let count = stream.read(&mut chunk).expect("request body");
            assert!(count > 0, "request closed before body");
            bytes.extend_from_slice(&chunk[..count]);
        }
        String::from_utf8(bytes).expect("utf8 request")
    }

    fn serve_one(listener: TcpListener, response: String, captured: mpsc::Sender<String>) {
        let (mut stream, _) = listener.accept().expect("connection");
        let request = read_request(&mut stream);
        captured.send(request).expect("capture request");
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        );
        stream.write_all(wire.as_bytes()).expect("response");
    }

    #[test]
    fn calls_openai_adapter_and_returns_bounded_candidate() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let endpoint = format!("http://{}", listener.local_addr().expect("address"));
        let response_content = json!({
            "schema_version": SCHEMA_VERSION,
            "outcome": {
                "kind": "select",
                "value": {
                    "work_item_id": "queued",
                    "work_item_revision": 5,
                    "work_item_generation": 2,
                    "reactivation_mode": "activate_queued"
                }
            },
            "confidence": 0.91,
            "evidence": [],
            "provenance": {
                "provider": "mock-openai",
                "model": "mock-model"
            }
        })
        .to_string();
        let response = json!({
            "choices": [{
                "message": { "content": response_content }
            }]
        })
        .to_string();
        let (captured_tx, captured_rx) = mpsc::channel();
        let server = thread::spawn(move || serve_one(listener, response, captured_tx));
        let hook =
            OpenAiSemanticCandidateSelectionHook::new(OpenAiConfig::new(endpoint, "mock-model"))
                .expect("provider");

        let context = test_context();
        let result =
            SemanticCandidateSelectionHook::select_autonomous_continuation(&hook, &context)
                .expect("hook result");
        let request = captured_rx.recv().expect("captured request");
        server.join().expect("server");

        assert!(request.contains(SCHEMA));
        match result {
            SemanticCandidateSelectionHookResult::Propose(proposal) => {
                assert_eq!(proposal.candidate.work_item_id, "queued");
                assert_eq!(
                    proposal.candidate.reactivation_mode,
                    WorkReactivationMode::ActivateQueued
                );
            }
            SemanticCandidateSelectionHookResult::Abstain => panic!("expected proposal"),
        }
    }

    #[test]
    fn executor_cancels_provider_after_deadline() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let endpoint = format!("http://{}", listener.local_addr().expect("address"));
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request_byte = [0_u8; 1];
            stream.read_exact(&mut request_byte).expect("request");
            thread::sleep(Duration::from_millis(100));
        });

        let provider = OpenAiProvider::new(
            OpenAiConfig::new(endpoint, "mock-model").with_timeout(Duration::from_secs(1)),
        )
        .expect("provider");
        let executor = DecisionExecutor::new(provider, Duration::from_millis(20), 1, 1);
        let request = DecisionRequest {
            request_id: "deadline-test".into(),
            input: json!({}),
            candidates: vec![json!({"work_item_id": "queued"})],
            schema: SCHEMA.into(),
            schema_version: SCHEMA_VERSION.into(),
            metadata: BTreeMap::new(),
            deadline_ms: Some(20),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let result = runtime.block_on(executor.decide(request));

        assert!(matches!(result, Err(DecisionError::DeadlineExceeded)));
        server.join().expect("server");
    }
}
