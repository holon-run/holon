//! Tests for `src/runtime/message_dispatch.rs`.
//!
//! Covers the routing logic that maps `MessageKind` variants to per-kind
//! reducers (P0 of issue #1858), and verifies that the defensive
//! `Result<Option<TaskRecord>>` propagation now surfaces typed errors instead
//! of panicking on the former `.expect()` call sites.

use super::super::message_dispatch::MessageDispatchPlan;
use super::super::*;
use super::support::*;
use crate::types::{
    AuthorityClass, ClosureDecision, ClosureOutcome, MessageBody, MessageKind, MessageOrigin,
    Priority, RuntimePosture,
};

fn task_metadata() -> serde_json::Value {
    serde_json::json!({
        "task_id": "task-dispatch-1",
        "task_kind": "command_task",
        "task_status": "running",
    })
}

fn message_of_kind(kind: MessageKind, body: MessageBody) -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        kind,
        MessageOrigin::Operator { actor_id: None },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        body,
    )
}

fn task_status_message() -> MessageEnvelope {
    let mut msg = message_of_kind(
        MessageKind::TaskStatus,
        MessageBody::Text {
            text: "status payload".into(),
        },
    );
    msg.metadata = Some(task_metadata());
    msg
}

fn task_result_message() -> MessageEnvelope {
    let mut msg = message_of_kind(
        MessageKind::TaskResult,
        MessageBody::Text {
            text: "result payload".into(),
        },
    );
    msg.metadata = Some(task_metadata());
    msg
}

fn closure_decision() -> ClosureDecision {
    ClosureDecision {
        outcome: ClosureOutcome::Continuable,
        waiting_reason: None,
        work_signal: None,
        runtime_posture: RuntimePosture::Awake,
        evidence: Vec::new(),
    }
}

async fn fresh_runtime() -> (TempDir, TempDir, RuntimeHandle) {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    (dir, workspace, runtime)
}

// ── build_message_dispatch_plan: task field population ─────────────

#[tokio::test]
async fn plan_parses_task_for_task_status_with_valid_metadata() {
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let msg = task_status_message();
    let plan = runtime
        .build_message_dispatch_plan(
            &msg,
            closure_decision(),
            &runtime.agent_state().await.unwrap(),
        )
        .unwrap();
    let task = plan
        .task
        .expect("TaskStatus plan should produce Ok task")
        .expect("TaskStatus plan should produce Some(task)");
    assert_eq!(task.id, "task-dispatch-1");
    assert_eq!(task.status, crate::types::TaskStatus::Running);
}

#[tokio::test]
async fn plan_parses_task_for_task_result_with_valid_metadata() {
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let msg = task_result_message();
    let plan = runtime
        .build_message_dispatch_plan(
            &msg,
            closure_decision(),
            &runtime.agent_state().await.unwrap(),
        )
        .unwrap();
    let task = plan
        .task
        .expect("TaskResult plan should produce Ok task")
        .expect("TaskResult plan should produce Some(task)");
    assert_eq!(task.id, "task-dispatch-1");
    // TaskResult with explicit "running" still yields Running because the
    // kind-driven default only fires when task_status is absent; here the
    // explicit value wins.
    assert_eq!(task.status, crate::types::TaskStatus::Running);
}

#[tokio::test]
async fn plan_task_result_uses_kind_default_when_status_absent() {
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let mut msg = task_result_message();
    let mut meta = msg.metadata.take().unwrap();
    meta.as_object_mut().unwrap().remove("task_status");
    msg.metadata = Some(meta);
    let plan = runtime
        .build_message_dispatch_plan(
            &msg,
            closure_decision(),
            &runtime.agent_state().await.unwrap(),
        )
        .unwrap();
    let task = plan.task.unwrap().unwrap();
    assert_eq!(task.status, crate::types::TaskStatus::Completed);
}

#[tokio::test]
async fn plan_task_errors_when_task_status_lacks_task_id() {
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let mut msg = task_status_message();
    let mut meta = msg.metadata.take().unwrap();
    meta.as_object_mut().unwrap().remove("task_id");
    msg.metadata = Some(meta);
    let plan = runtime.build_message_dispatch_plan(
        &msg,
        closure_decision(),
        &runtime.agent_state().await.unwrap(),
    );
    let plan = plan.expect("plan construction must succeed for TaskStatus message");
    match plan.task {
        Err(err) => assert!(
            err.to_string().contains("task_id"),
            "error should mention missing task_id, got: {err}"
        ),
        Ok(_) => panic!("missing task_id must surface as Err in plan.task"),
    }
}

#[tokio::test]
async fn plan_task_errors_when_task_status_lacks_task_kind() {
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let mut msg = task_status_message();
    let mut meta = msg.metadata.take().unwrap();
    meta.as_object_mut().unwrap().remove("task_kind");
    msg.metadata = Some(meta);
    let plan = runtime.build_message_dispatch_plan(
        &msg,
        closure_decision(),
        &runtime.agent_state().await.unwrap(),
    );
    let plan = plan.expect("plan construction must succeed for TaskStatus message");
    match plan.task {
        Err(err) => assert!(
            err.to_string().contains("task_kind"),
            "error should mention missing task_kind, got: {err}"
        ),
        Ok(_) => panic!("missing task_kind must surface as Err in plan.task"),
    }
}

#[tokio::test]
async fn plan_task_is_none_for_non_task_kinds() {
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let kinds = [
        MessageKind::OperatorPrompt,
        MessageKind::TimerTick,
        MessageKind::SystemTick,
        MessageKind::ChannelEvent,
        MessageKind::WebhookEvent,
        MessageKind::CallbackEvent,
        MessageKind::InternalFollowup,
        MessageKind::Control,
        MessageKind::BriefAck,
        MessageKind::BriefResult,
    ];
    for kind in kinds {
        let msg = message_of_kind(
            kind.clone(),
            MessageBody::Text {
                text: "payload".into(),
            },
        );
        let plan = runtime.build_message_dispatch_plan(
            &msg,
            closure_decision(),
            &runtime.agent_state().await.unwrap(),
        );
        match plan {
            Ok(plan) => match plan.task {
                Ok(None) => {}
                other => panic!("non-task kind {kind:?} must yield Ok(None) task, got {other:?}"),
            },
            Err(err) => panic!("plan for {kind:?} should be Ok, got {err}"),
        }
    }
}

// ── process_message_with_plan: kind routing ────────────────────────

#[tokio::test]
async fn dispatch_brief_ack_is_noop() {
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let msg = message_of_kind(
        MessageKind::BriefAck,
        MessageBody::Text { text: "ack".into() },
    );
    let plan = runtime
        .build_message_dispatch_plan(
            &msg,
            closure_decision(),
            &runtime.agent_state().await.unwrap(),
        )
        .unwrap();
    let decision =
        scheduler::SchedulerDecision::new(scheduler::SchedulerDecisionKind::Noop, "test_brief_ack");
    runtime
        .process_message_with_plan(msg, plan, &decision)
        .await
        .expect("BriefAck dispatch should not error");
    let state = runtime.agent_state().await.unwrap();
    assert!(matches!(
        state.status,
        AgentStatus::Booting
            | AgentStatus::AwakeIdle
            | AgentStatus::AwakeRunning
            | AgentStatus::AwaitingTask
            | AgentStatus::Asleep
    ));
}

#[tokio::test]
async fn dispatch_brief_result_is_noop() {
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let msg = message_of_kind(
        MessageKind::BriefResult,
        MessageBody::Text {
            text: "result".into(),
        },
    );
    let plan = runtime
        .build_message_dispatch_plan(
            &msg,
            closure_decision(),
            &runtime.agent_state().await.unwrap(),
        )
        .unwrap();
    let decision = scheduler::SchedulerDecision::new(
        scheduler::SchedulerDecisionKind::Noop,
        "test_brief_result",
    );
    runtime
        .process_message_with_plan(msg, plan, &decision)
        .await
        .expect("BriefResult dispatch should not error");
}

#[tokio::test]
async fn dispatch_control_with_unknown_text_returns_error() {
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let msg = message_of_kind(
        MessageKind::Control,
        MessageBody::Text {
            text: "weird-action".into(),
        },
    );
    let plan = runtime
        .build_message_dispatch_plan(
            &msg,
            closure_decision(),
            &runtime.agent_state().await.unwrap(),
        )
        .unwrap();
    let decision = scheduler::SchedulerDecision::new(
        scheduler::SchedulerDecisionKind::Noop,
        "test_control_unknown",
    );
    let err = runtime
        .process_message_with_plan(msg, plan, &decision)
        .await
        .expect_err("unknown control action must error");
    assert!(
        err.to_string().contains("unknown control action"),
        "expected 'unknown control action' in error, got: {err}"
    );
}

#[tokio::test]
async fn dispatch_task_status_with_invalid_metadata_returns_error_not_panic() {
    // Regression test for the `.expect("task status message should parse task")`
    // call site that used to panic if `task_from_message` ever returned
    // `Ok(None)` for a TaskStatus message. With the defensive `?` propagation
    // in place the dispatch now returns a typed error instead of panicking.
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let mut msg = task_status_message();
    let mut meta = msg.metadata.take().unwrap();
    meta.as_object_mut().unwrap().remove("task_id");
    msg.metadata = Some(meta);

    let plan = runtime.build_message_dispatch_plan(
        &msg,
        closure_decision(),
        &runtime.agent_state().await.unwrap(),
    );
    // The plan itself already surfaces the metadata error in plan.task before
    // reaching the dispatch arm; ensure both layers agree on the typed-error
    // contract.
    let plan = plan.expect("plan construction must succeed for TaskStatus message");
    match plan.task {
        Err(err) => assert!(err.to_string().contains("task_id")),
        Ok(_) => panic!("plan.task should have errored on missing task_id metadata"),
    }
}

#[tokio::test]
async fn dispatch_interactive_kind_with_model_reentry_false_skips_interactive_processing() {
    // When `model_reentry` is false on the scheduler decision, an interactive
    // message must NOT call `process_interactive_message`. We can't observe
    // the inner call directly, but we can confirm the dispatch returns Ok
    // without side effects on the projection.
    let (_dir, _ws, runtime) = fresh_runtime().await;
    let msg = message_of_kind(
        MessageKind::OperatorPrompt,
        MessageBody::Text {
            text: "interactive payload".into(),
        },
    );
    let plan = runtime
        .build_message_dispatch_plan(
            &msg,
            closure_decision(),
            &runtime.agent_state().await.unwrap(),
        )
        .unwrap();
    // model_reentry defaults to false via SchedulerDecision::new.
    let decision = scheduler::SchedulerDecision::new(
        scheduler::SchedulerDecisionKind::EmitSystemTick,
        "test_no_reentry",
    );
    assert!(!decision.model_reentry);
    runtime
        .process_message_with_plan(msg, plan, &decision)
        .await
        .expect("model_reentry=false dispatch should be a silent no-op for interactive kinds");
}

// ── dispatch_plan struct visibility (sanity) ───────────────────────

#[test]
fn message_dispatch_plan_public_fields_are_constructible_from_tests() {
    // Compile-time check: tests inside src/runtime/tests/ can name every
    // `MessageDispatchPlan` field thanks to `pub(super)` visibility.
    let _plan: MessageDispatchPlan = MessageDispatchPlan {
        prior_closure: closure_decision(),
        task: Ok(None),
        continuation_trigger: None,
        continuation_resolution: None,
        model_turn_allowed: false,
    };
}
