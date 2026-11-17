//! Runtime integration tests for provider round admission limits (#4192).
//!
//! Every test injects a fully scripted provider response, so the admission
//! gate is exercised at the shared response reception boundary for both
//! accepted and rejected rounds without calling a real model.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::*;
use super::support::*;

use crate::provider::ProviderTurnRequest;

struct RoundAdmissionProvider {
    rounds: Mutex<VecDeque<Vec<ModelBlock>>>,
    calls: AtomicUsize,
}

impl RoundAdmissionProvider {
    fn new(rounds: Vec<Vec<ModelBlock>>) -> Self {
        Self {
            rounds: Mutex::new(rounds.into()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AgentProvider for RoundAdmissionProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let blocks = self.rounds.lock().await.pop_front().unwrap_or_default();
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }

    fn configured_model_refs(&self) -> Vec<String> {
        vec!["stub".into()]
    }
}

fn tool_use(id: &str, name: &str, input: serde_json::Value) -> ModelBlock {
    ModelBlock::ToolUse {
        id: id.to_string(),
        name: name.to_string(),
        input,
        kind: Default::default(),
        provider_data: None,
    }
}

fn flood_calls(count: usize) -> Vec<ModelBlock> {
    (0..count)
        .map(|i| tool_use(&format!("flood-{i}"), "UnknownTool", serde_json::json!({})))
        .collect()
}

async fn drive_turn(runtime: &RuntimeHandle, provider: &RoundAdmissionProvider) -> MessageEnvelope {
    let message = runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "run the scripted round".into(),
            },
        ))
        .await
        .unwrap();
    let scheduler_executor::RunLoopPoll::Message(scheduled) =
        scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await
            .unwrap()
    else {
        panic!("operator prompt must be claimed");
    };
    assert_eq!(scheduled.message.id, message.id);
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await
        .unwrap();
    // Drive the claimed dispatch through the canonical fixture agent loop so
    // admitted rounds continue to the next provider request exactly as in
    // production, and a rejection fails the turn at the shared boundary.
    let loop_runtime = runtime.clone();
    let outcome = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop_with_fixture_dispatch(
                "default",
                AuthorityClass::OperatorInstruction,
                test_effective_prompt(),
                LoopControlOptions {
                    max_tool_rounds: None,
                },
                Some(scheduled),
            )
            .await
    })
    .await
    .unwrap();
    assert!(
        provider.calls() >= 1,
        "the scripted provider response must have been requested"
    );
    if let Err(error) = outcome {
        // The fixture loop settles the queue entry itself; production
        // dispatch settlement additionally records failure artifacts, so
        // mirror that here for the runtime failure assertions.
        runtime
            .persist_runtime_failure_artifacts(&message, &error)
            .await
            .unwrap();
    }
    message
}

fn assert_rejected_turn(state: &AgentState, expected_quota: &str) {
    let failure = state
        .last_runtime_failure
        .as_ref()
        .expect("admission rejection must record a runtime failure");
    assert!(
        failure
            .summary
            .contains("provider round exceeded admission limit"),
        "unexpected failure summary: {}",
        failure.summary
    );
    assert!(
        failure.summary.contains(expected_quota),
        "failure summary must name the exceeded quota {expected_quota}: {}",
        failure.summary
    );
}

fn queue_status_for(runtime: &RuntimeHandle, message: &MessageEnvelope) -> QueueEntryStatus {
    runtime
        .storage()
        .read_recent_queue_entries(100)
        .unwrap()
        .into_iter()
        .find(|entry| entry.message_id == message.id)
        .map(|entry| entry.status)
        .expect("queue entry must exist for the driven message")
}

fn turn_tool_executions(
    runtime: &RuntimeHandle,
    state: &AgentState,
) -> Vec<crate::types::ToolExecutionRecord> {
    let turn_id = state
        .current_turn_id
        .as_deref()
        .expect("turn must be recorded");
    runtime
        .storage()
        .read_tool_executions_for_turn(turn_id)
        .unwrap()
}

#[tokio::test]
async fn tool_call_flood_rejects_round_with_zero_side_effects() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(RoundAdmissionProvider::new(vec![flood_calls(9_412)]));
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = drive_turn(&runtime, &provider).await;

    assert_eq!(provider.calls(), 1, "no automatic re-ask after rejection");
    let state = runtime.agent_state().await.unwrap();
    assert_rejected_turn(&state, "quota=managed_tool_calls");
    assert!(
        turn_tool_executions(&runtime, &state).is_empty(),
        "a rejected round must not create any tool execution record"
    );
    assert_eq!(
        queue_status_for(&runtime, &message),
        QueueEntryStatus::Aborted
    );
}

#[tokio::test]
async fn sixty_four_calls_are_admitted_and_sixty_five_are_rejected() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    // Equal to the limit is admitted: all 64 calls keep the existing
    // per-call dispatch behavior. The turn then ends via the existing
    // turn-local baseline budget (64 in-conversation tool results exceed the
    // next-round projection budget) instead of issuing another provider
    // round; that termination is unrelated to round admission, and
    // `prior_round_side_effects_survive_a_later_rejected_round` covers the
    // continuation path after an admitted tool round.
    let provider = Arc::new(RoundAdmissionProvider::new(vec![
        flood_calls(64),
        vec![ModelBlock::Text {
            text: "done".into(),
        }],
    ]));
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = drive_turn(&runtime, &provider).await;

    assert!(
        provider.calls() >= 1,
        "the in-limit round must be dispatched and admitted instead of rejected"
    );
    let state = runtime.agent_state().await.unwrap();
    assert!(
        state.last_runtime_failure.is_none(),
        "an in-limit round must not fail the turn"
    );
    assert_eq!(
        turn_tool_executions(&runtime, &state).len(),
        64,
        "admitted calls keep the existing per-call dispatch behavior"
    );
    assert_eq!(
        queue_status_for(&runtime, &message),
        QueueEntryStatus::Processed
    );

    // Sixty-five calls in one response are rejected as a whole round.
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(RoundAdmissionProvider::new(vec![flood_calls(65)]));
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = drive_turn(&runtime, &provider).await;
    assert_eq!(provider.calls(), 1);
    let state = runtime.agent_state().await.unwrap();
    assert_rejected_turn(&state, "quota=managed_tool_calls");
    assert!(turn_tool_executions(&runtime, &state).is_empty());
    assert_eq!(
        queue_status_for(&runtime, &message),
        QueueEntryStatus::Aborted
    );
}

#[tokio::test]
async fn prior_round_side_effects_survive_a_later_rejected_round() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let provider = Arc::new(RoundAdmissionProvider::new(vec![
        vec![tool_use("one", "UnknownTool", serde_json::json!({}))],
        flood_calls(9_412),
    ]));
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = drive_turn(&runtime, &provider).await;

    assert_eq!(provider.calls(), 2);
    let state = runtime.agent_state().await.unwrap();
    assert_rejected_turn(&state, "quota=managed_tool_calls");
    let executions = turn_tool_executions(&runtime, &state);
    assert_eq!(
        executions.len(),
        1,
        "the earlier admitted round's side effects must be preserved exactly once"
    );
    assert_eq!(
        queue_status_for(&runtime, &message),
        QueueEntryStatus::Aborted
    );
}

#[tokio::test]
async fn terminal_control_tool_in_an_over_limit_round_is_not_executed() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let mut blocks = vec![tool_use(
        "terminal",
        "CompleteWorkItem",
        serde_json::json!({"work_item_id": "work_missing"}),
    )];
    blocks.extend(flood_calls(64));
    let provider = Arc::new(RoundAdmissionProvider::new(vec![blocks]));
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = drive_turn(&runtime, &provider).await;

    assert_eq!(provider.calls(), 1);
    let state = runtime.agent_state().await.unwrap();
    assert_rejected_turn(&state, "quota=managed_tool_calls");
    assert!(
        turn_tool_executions(&runtime, &state).is_empty(),
        "mixed terminal-control responses are rejected wholesale"
    );
    assert_eq!(
        queue_status_for(&runtime, &message),
        QueueEntryStatus::Aborted
    );
}

#[tokio::test]
async fn oversized_batch_argument_rejects_round_before_item_parsing() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let items: Vec<serde_json::Value> = (0..18_471).map(|_| serde_json::json!({})).collect();
    let blocks = vec![tool_use(
        "batch",
        "ExecCommandBatch",
        serde_json::json!({"items": items}),
    )];
    let provider = Arc::new(RoundAdmissionProvider::new(vec![blocks]));
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = drive_turn(&runtime, &provider).await;

    assert_eq!(provider.calls(), 1);
    let state = runtime.agent_state().await.unwrap();
    assert_rejected_turn(&state, "quota=batch_items");
    assert!(turn_tool_executions(&runtime, &state).is_empty());
    assert_eq!(
        queue_status_for(&runtime, &message),
        QueueEntryStatus::Aborted
    );
}

#[tokio::test]
async fn oversized_single_argument_rejects_round_with_lower_bound_count() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let blocks = vec![tool_use(
        "huge",
        "UnknownTool",
        serde_json::json!({"blob": "x".repeat(1_800_000)}),
    )];
    let provider = Arc::new(RoundAdmissionProvider::new(vec![blocks]));
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = drive_turn(&runtime, &provider).await;

    let state = runtime.agent_state().await.unwrap();
    assert_rejected_turn(&state, "quota=tool_call_input_bytes");
    let failure = state
        .last_runtime_failure
        .as_ref()
        .expect("failure recorded");
    assert!(
        failure.summary.contains("count=at_least"),
        "early-stopped accounting must be reported as a lower bound: {}",
        failure.summary
    );
    assert!(turn_tool_executions(&runtime, &state).is_empty());
    assert_eq!(
        queue_status_for(&runtime, &message),
        QueueEntryStatus::Aborted
    );
}

#[tokio::test]
async fn text_plus_calls_over_visible_payload_rejects_round() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let blocks = vec![
        ModelBlock::Text {
            text: "x".repeat(300_000),
        },
        tool_use("a", "UnknownTool", serde_json::json!({})),
    ];
    let provider = Arc::new(RoundAdmissionProvider::new(vec![blocks]));
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let message = drive_turn(&runtime, &provider).await;

    let state = runtime.agent_state().await.unwrap();
    assert_rejected_turn(&state, "quota=round_visible_payload_bytes");
    assert!(turn_tool_executions(&runtime, &state).is_empty());
    assert_eq!(
        queue_status_for(&runtime, &message),
        QueueEntryStatus::Aborted
    );
}
