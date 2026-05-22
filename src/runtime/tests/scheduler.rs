use super::super::*;
use super::support::*;
use crate::types::{
    AgentPostureProjection, AgentSchedulingPosture, ToolExecutionStatus, WaitConditionKind,
    WaitConditionRecord, WaitConditionStatus, WakeSource, WorkItemPlanStatus,
    WorkItemSchedulingState, WorkReactivationMode,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct AgentFixture {
    #[serde(default)]
    current_work_item_id: Option<String>,
    #[serde(default)]
    pending_wake_hint_reason: Option<String>,
    #[serde(default)]
    turn_index: u64,
    #[serde(default)]
    last_turn_terminal_kind: Option<TurnTerminalKind>,
}

#[derive(Deserialize)]
struct WorkItemFixture {
    id: String,
    objective: String,
    state: WorkItemState,
    plan_status: Option<WorkItemPlanStatus>,
    revision: u64,
}

#[derive(Deserialize)]
struct TaskFixture {
    id: String,
    status: TaskStatus,
    wait_policy: String,
    work_item_id: Option<String>,
}

#[derive(Deserialize)]
struct WaitingIntentFixture {
    id: String,
    scope: ExternalTriggerScope,
    work_item_id: Option<String>,
}

#[derive(Deserialize)]
struct TimerFixture {
    id: String,
}

#[derive(Deserialize)]
struct MessageFixture {
    id: String,
    kind: MessageKind,
    text: String,
    #[serde(default)]
    priority: Option<Priority>,
    #[serde(default)]
    work_item_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(Deserialize)]
struct QueueEntryFixture {
    message_id: String,
    status: QueueEntryStatus,
    #[serde(default)]
    priority: Option<Priority>,
}

#[derive(Deserialize)]
struct EventFixture {
    kind: String,
    #[serde(default)]
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct BriefFixture {
    id: String,
    kind: BriefKind,
    text: String,
}

#[derive(Deserialize)]
struct ToolExecutionFixture {
    id: String,
    tool_name: String,
    status: ToolExecutionStatus,
    #[serde(default)]
    work_item_id: Option<String>,
}

#[derive(Deserialize)]
struct ExpectedFixture {
    current_work_item_id: Option<String>,
    current_work_item_revision: Option<u64>,
    queued_work_items: usize,
    active_tasks: usize,
    has_blocking_active_tasks: bool,
    pending_wake_hint: bool,
    #[serde(default)]
    active_waiting_intents: usize,
    #[serde(default)]
    active_work_item_waiting_intents: usize,
    #[serde(default)]
    active_agent_waiting_intents: usize,
    #[serde(default)]
    active_timers: usize,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    decision_reason: Option<String>,
    #[serde(default)]
    runtime_error: bool,
    #[serde(default)]
    replay_message_ids: Vec<String>,
    #[serde(default)]
    tool_executions: usize,
    #[serde(default)]
    turn_terminal_kind: Option<TurnTerminalKind>,
}

fn scheduler_fixture_path(name: &str, path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/scheduler")
        .join(name)
        .join(path)
}

fn read_scheduler_fixture<T: DeserializeOwned>(name: &str, path: &str) -> T {
    let path = scheduler_fixture_path(name, path);
    let content = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "failed to read scheduler fixture {}: {error}",
            path.display()
        )
    });
    serde_json::from_str(&content).unwrap_or_else(|error| {
        panic!(
            "failed to parse scheduler fixture {}: {error}",
            path.display()
        )
    })
}

fn read_optional_scheduler_fixture<T: DeserializeOwned + Default>(name: &str, path: &str) -> T {
    let path_buf = scheduler_fixture_path(name, path);
    if path_buf.exists() {
        read_scheduler_fixture(name, path)
    } else {
        T::default()
    }
}

fn read_optional_scheduler_jsonl_fixture<T: DeserializeOwned>(name: &str, path: &str) -> Vec<T> {
    let jsonl_path = scheduler_fixture_path(name, path);
    if jsonl_path.exists() {
        let content = std::fs::read_to_string(&jsonl_path).unwrap_or_else(|error| {
            panic!(
                "failed to read scheduler fixture {}: {error}",
                jsonl_path.display()
            )
        });
        return content
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                Some(serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!(
                        "failed to parse scheduler fixture {} line {}: {error}",
                        jsonl_path.display(),
                        index + 1
                    )
                }))
            })
            .collect();
    }

    if let Some(json_path) = path.strip_suffix(".jsonl") {
        let json_path = format!("{json_path}.json");
        return read_optional_scheduler_fixture(name, &json_path);
    }

    Vec::new()
}

fn build_scheduler_fixture(name: &str) -> (tempfile::TempDir, AppStorage, AgentState) {
    let agent_fixture: AgentFixture = read_scheduler_fixture(name, "agent.json");
    let work_items: Vec<WorkItemFixture> =
        read_optional_scheduler_jsonl_fixture(name, "ledger/work_items.jsonl");
    let tasks: Vec<TaskFixture> = read_optional_scheduler_jsonl_fixture(name, "ledger/tasks.jsonl");
    let waiting_intents: Vec<WaitingIntentFixture> =
        read_optional_scheduler_jsonl_fixture(name, "ledger/waiting_intents.jsonl");
    let timers: Vec<TimerFixture> =
        read_optional_scheduler_jsonl_fixture(name, "ledger/timers.jsonl");
    let messages: Vec<MessageFixture> =
        read_optional_scheduler_jsonl_fixture(name, "ledger/messages.jsonl");
    let queue_entries: Vec<QueueEntryFixture> =
        read_optional_scheduler_jsonl_fixture(name, "ledger/queue_entries.jsonl");
    let events: Vec<EventFixture> =
        read_optional_scheduler_jsonl_fixture(name, "ledger/events.jsonl");
    let briefs: Vec<BriefFixture> =
        read_optional_scheduler_jsonl_fixture(name, "ledger/briefs.jsonl");
    let tool_executions: Vec<ToolExecutionFixture> =
        read_optional_scheduler_jsonl_fixture(name, "ledger/tools.jsonl");
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();

    let mut agent = AgentState::new("default");
    agent.current_work_item_id = agent_fixture.current_work_item_id;
    agent.turn_index = agent_fixture.turn_index;
    if let Some(kind) = agent_fixture.last_turn_terminal_kind {
        agent.last_turn_terminal = Some(TurnTerminalRecord {
            turn_index: agent.turn_index,
            kind,
            reason: Some("fixture terminal".into()),
            last_assistant_message: None,
            checkpoint: None,
            completed_at: Utc::now(),
            duration_ms: 1,
        });
    }
    if let Some(reason) = agent_fixture.pending_wake_hint_reason {
        agent.pending_wake_hint = Some(PendingWakeHint {
            reason,
            description: None,
            scope: None,
            waiting_intent_id: None,
            external_trigger_id: None,
            source: Some("fixture".into()),
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        });
    }
    storage.write_agent(&agent).unwrap();

    for item in work_items {
        let mut record = WorkItemRecord::new("default", item.objective, item.state);
        record.id = item.id;
        if let Some(plan_status) = item.plan_status {
            record.plan_status = plan_status;
        }
        record.revision = item.revision;
        storage.append_work_item(&record).unwrap();
    }
    for task in tasks {
        storage
            .append_task(&TaskRecord {
                id: task.id,
                agent_id: "default".into(),
                kind: TaskKind::CommandTask,
                status: task.status,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: task.work_item_id,
                summary: Some("fixture task".into()),
                detail: Some(serde_json::json!({
                    "wait_policy": task.wait_policy,
                })),
                recovery: None,
            })
            .unwrap();
    }
    for intent in waiting_intents {
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: intent.id.clone(),
                agent_id: "default".into(),
                scope: intent.scope,
                work_item_id: intent.work_item_id,
                description: format!("fixture waiting intent {}", intent.id),
                source: "fixture".into(),
                resource: None,
                condition: None,
                delivery_mode: CallbackDeliveryMode::WakeHint,
                status: WaitingIntentStatus::Active,
                external_trigger_id: format!("trigger-{}", intent.id),
                created_at: Utc::now(),
                cancelled_at: None,
                last_triggered_at: None,
                trigger_count: 0,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();
    }
    for timer in timers {
        storage
            .append_timer(&TimerRecord {
                id: timer.id,
                agent_id: "default".into(),
                created_at: Utc::now(),
                duration_ms: 1000,
                interval_ms: None,
                repeat: false,
                status: TimerStatus::Active,
                summary: Some("fixture timer".into()),
                next_fire_at: Some(Utc::now()),
                last_fired_at: None,
                fire_count: 0,
            })
            .unwrap();
    }
    for message_fixture in messages {
        let origin = match message_fixture.kind {
            MessageKind::SystemTick => MessageOrigin::System {
                subsystem: "fixture".into(),
            },
            MessageKind::TaskResult | MessageKind::TaskStatus => MessageOrigin::Task {
                task_id: message_fixture
                    .task_id
                    .clone()
                    .unwrap_or_else(|| "fixture-task".into()),
            },
            _ => MessageOrigin::Webhook {
                source: "fixture".into(),
                event_type: None,
            },
        };
        let mut message = MessageEnvelope::new(
            "default",
            message_fixture.kind,
            origin,
            TrustLevel::TrustedIntegration,
            message_fixture.priority.unwrap_or(Priority::Normal),
            MessageBody::Text {
                text: message_fixture.text,
            },
        );
        message.id = message_fixture.id;
        message.work_item_id = message_fixture.work_item_id;
        message.task_id = message_fixture.task_id;
        storage.append_message(&message).unwrap();
    }
    for queue_entry in queue_entries {
        storage
            .append_queue_entry(&QueueEntryRecord {
                message_id: queue_entry.message_id,
                agent_id: "default".into(),
                priority: queue_entry.priority.unwrap_or(Priority::Normal),
                status: queue_entry.status,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .unwrap();
    }
    for event in events {
        storage
            .append_event(&AuditEvent::new(event.kind, event.data))
            .unwrap();
    }
    for brief in briefs {
        let mut record = BriefRecord::new("default", brief.kind, brief.text, None, None);
        record.id = brief.id;
        storage.append_brief(&record).unwrap();
    }
    for tool in tool_executions {
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: tool.id,
                agent_id: "default".into(),
                work_item_id: tool.work_item_id,
                turn_index: 1,
                tool_name: tool.tool_name,
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 1,
                trust: TrustLevel::TrustedOperator,
                status: tool.status,
                input: serde_json::json!({ "fixture": true }),
                output: serde_json::json!({ "fixture": true }),
                summary: "fixture tool execution".into(),
                invocation_surface: Some("fixture".into()),
            })
            .unwrap();
    }

    (dir, storage, agent)
}

fn assert_scheduler_fixture(name: &str) {
    let expected: ExpectedFixture = read_scheduler_fixture(name, "expected.json");
    let (_dir, storage, agent) = build_scheduler_fixture(name);

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(
        projection
            .current_work_item
            .as_ref()
            .map(|item| item.id.as_str()),
        expected.current_work_item_id.as_deref(),
        "{name}: current work item"
    );
    assert_eq!(
        projection
            .current_work_item
            .as_ref()
            .map(|item| item.revision),
        expected.current_work_item_revision,
        "{name}: current work item revision"
    );
    assert_eq!(
        projection.queued_work_items, expected.queued_work_items,
        "{name}: queued work items"
    );
    assert_eq!(
        projection.active_tasks.len(),
        expected.active_tasks,
        "{name}: active tasks"
    );
    assert_eq!(
        projection.has_blocking_active_tasks, expected.has_blocking_active_tasks,
        "{name}: blocking active tasks"
    );
    assert_eq!(
        projection.pending_wake_hint, expected.pending_wake_hint,
        "{name}: pending wake hint"
    );
    assert_eq!(
        projection.active_waiting_intents, expected.active_waiting_intents,
        "{name}: active waiting intents"
    );
    assert_eq!(
        projection.active_work_item_waiting_intents, expected.active_work_item_waiting_intents,
        "{name}: work-item waiting intents"
    );
    assert_eq!(
        projection.active_agent_waiting_intents, expected.active_agent_waiting_intents,
        "{name}: agent waiting intents"
    );
    assert_eq!(
        projection.active_timers, expected.active_timers,
        "{name}: active timers"
    );
    assert_eq!(
        projection.runtime_error, expected.runtime_error,
        "{name}: runtime error"
    );
    assert_eq!(
        agent.last_turn_terminal.map(|record| record.kind),
        expected.turn_terminal_kind,
        "{name}: turn terminal kind"
    );
    let snapshot = storage.recovery_snapshot().unwrap();
    let replay_message_ids = snapshot
        .replay_messages
        .into_iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    assert_eq!(
        replay_message_ids, expected.replay_message_ids,
        "{name}: replay messages"
    );
    assert_eq!(
        storage
            .read_recent_tool_executions(usize::MAX)
            .unwrap()
            .len(),
        expected.tool_executions,
        "{name}: tool executions"
    );
    if let Some(expected_decision) = expected.decision {
        let decision = scheduler::decide_next_action(
            &projection,
            scheduler::SchedulerBoundary::RunLoopIdle,
            scheduler::SchedulerInput::Idle,
        );
        assert_eq!(
            decision.kind.as_str(),
            expected_decision,
            "{name}: scheduler decision"
        );
        assert_eq!(
            Some(decision.reason.as_str()),
            expected.decision_reason.as_deref(),
            "{name}: scheduler decision reason"
        );
    }
}

#[test]
fn scheduler_projection_replays_fixture_facts() {
    for name in [
        "basic",
        "agent_wait",
        "blocking_task",
        "timer_wait",
        "queued_available",
        "pending_wake_hint",
        "dequeued_replay",
        "tool_call_replay_boundary",
        "baseline_over_budget_terminal",
    ] {
        assert_scheduler_fixture(name);
    }
}

#[test]
fn compaction_events_and_briefs_do_not_change_scheduler_projection() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some("work-active".into());
    storage.write_agent(&agent).unwrap();

    let mut work_item = WorkItemRecord::new("default", "continue work", WorkItemState::Open);
    work_item.id = "work-active".into();
    work_item.revision = 3;
    storage.append_work_item(&work_item).unwrap();
    storage
        .append_task(&TaskRecord {
            id: "task-active".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: Some("work-active".into()),
            summary: Some("blocking task".into()),
            detail: Some(serde_json::json!({ "wait_policy": "blocking" })),
            recovery: None,
        })
        .unwrap();

    let before = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    storage
        .append_event(&AuditEvent::new(
            "turn_local_compaction_completed",
            serde_json::json!({
                "agent_id": "default",
                "turn_index": 1,
                "checkpoint": "fixture checkpoint",
            }),
        ))
        .unwrap();
    storage
        .append_event(&AuditEvent::new(
            "turn_local_baseline_over_budget",
            serde_json::json!({
                "agent_id": "default",
                "reason": "baseline_unfit",
            }),
        ))
        .unwrap();
    storage
        .append_brief(&BriefRecord::new(
            "default",
            BriefKind::Result,
            "compaction recap only",
            Some("work-active".into()),
            None,
        ))
        .unwrap();

    let after = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(
        before, after,
        "compaction artifacts must not become scheduler truth"
    );
}

#[test]
fn scheduler_projection_breaks_down_waiting_intent_scopes() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();
    let now = Utc::now();
    for (id, scope, work_item_id) in [
        ("work-wait", ExternalTriggerScope::WorkItem, Some("work-1")),
        ("agent-wait", ExternalTriggerScope::Agent, None),
    ] {
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: id.into(),
                agent_id: "default".into(),
                scope,
                work_item_id: work_item_id.map(ToString::to_string),
                description: format!("{id} description"),
                source: "test".into(),
                resource: None,
                condition: None,
                delivery_mode: CallbackDeliveryMode::WakeHint,
                status: WaitingIntentStatus::Active,
                external_trigger_id: format!("trigger-{id}"),
                created_at: now,
                cancelled_at: None,
                last_triggered_at: None,
                trigger_count: 0,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();
    }
    agent.pending_wake_hint = None;
    storage.write_agent(&agent).unwrap();

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(projection.active_waiting_intents, 2);
    assert_eq!(projection.active_work_item_waiting_intents, 1);
    assert_eq!(projection.active_agent_waiting_intents, 1);
}

#[test]
fn scheduler_projection_filters_tasks_waiting_intents_and_timers_by_agent() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();
    let now = Utc::now();

    for (id, agent_id) in [("wait-current", "default"), ("wait-other", "other")] {
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: id.into(),
                agent_id: agent_id.into(),
                scope: ExternalTriggerScope::Agent,
                work_item_id: None,
                description: format!("{id} description"),
                source: "test".into(),
                resource: None,
                condition: None,
                delivery_mode: CallbackDeliveryMode::WakeHint,
                status: WaitingIntentStatus::Active,
                external_trigger_id: format!("trigger-{id}"),
                created_at: now,
                cancelled_at: None,
                last_triggered_at: None,
                trigger_count: 0,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();
    }
    for (id, agent_id) in [("timer-current", "default"), ("timer-other", "other")] {
        storage
            .append_timer(&TimerRecord {
                id: id.into(),
                agent_id: agent_id.into(),
                created_at: now,
                duration_ms: 1000,
                interval_ms: None,
                repeat: false,
                status: TimerStatus::Active,
                summary: None,
                next_fire_at: Some(now),
                last_fired_at: None,
                fire_count: 0,
            })
            .unwrap();
    }
    for (id, agent_id) in [("task-current", "default"), ("task-other", "other")] {
        storage
            .append_task(&TaskRecord {
                id: id.into(),
                agent_id: agent_id.into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: now,
                updated_at: now,
                parent_message_id: None,
                work_item_id: None,
                summary: Some(format!("{id} summary")),
                detail: Some(serde_json::json!({ "wait_policy": "blocking" })),
                recovery: None,
            })
            .unwrap();
    }

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(projection.active_tasks.len(), 1);
    assert_eq!(projection.active_tasks[0].id, "task-current");
    assert_eq!(projection.active_waiting_intents, 1);
    assert_eq!(projection.active_agent_waiting_intents, 1);
    assert_eq!(projection.active_timers, 1);
}

#[test]
fn idle_boundary_decision_prefers_controlled_status_over_wait_facts() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::Stopped;
    storage.write_agent(&agent).unwrap();
    storage
        .append_waiting_intent(&WaitingIntentRecord {
            id: "wait-current".into(),
            agent_id: "default".into(),
            scope: ExternalTriggerScope::Agent,
            work_item_id: None,
            description: "wait".into(),
            source: "test".into(),
            resource: None,
            condition: None,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            status: WaitingIntentStatus::Active,
            external_trigger_id: "trigger-wait-current".into(),
            created_at: Utc::now(),
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        })
        .unwrap();

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    let decision = scheduler::idle_boundary_decision(&projection, "fixture");
    assert_eq!(decision.kind, scheduler::SchedulerDecisionKind::Stop);
    assert_eq!(decision.reason, "stopped");
}

#[test]
fn decide_next_action_prioritizes_wake_hint_over_work_queue_but_not_wait_facts() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();
    let mut work_item = WorkItemRecord::new("default", "continue work", WorkItemState::Open);
    work_item.id = "work-1".into();
    work_item.revision = 7;
    storage.append_work_item(&work_item).unwrap();

    let pending = PendingWakeHint {
        reason: "external update".into(),
        description: None,
        source: Some("fixture".into()),
        scope: Some(ExternalTriggerScope::Agent),
        waiting_intent_id: None,
        external_trigger_id: Some("trigger-1".into()),
        resource: None,
        body: None,
        content_type: None,
        correlation_id: None,
        causation_id: None,
        created_at: Utc::now(),
    };
    agent.pending_wake_hint = Some(pending.clone());
    storage.write_agent(&agent).unwrap();

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    let wake_decision = scheduler::decide_next_action(
        &projection,
        scheduler::SchedulerBoundary::IdleTick,
        scheduler::SchedulerInput::IdleSignal(scheduler::SchedulerIdleSignal::WakeHint {
            pending: &pending,
            duplicate: None,
        }),
    );
    assert_eq!(
        wake_decision.kind,
        scheduler::SchedulerDecisionKind::EmitSystemTick
    );
    assert_eq!(wake_decision.reason, "wake_hint");

    storage
        .append_waiting_intent(&WaitingIntentRecord {
            id: "wait-current".into(),
            agent_id: "default".into(),
            scope: ExternalTriggerScope::Agent,
            work_item_id: None,
            description: "unrelated wait".into(),
            source: "test".into(),
            resource: None,
            condition: None,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            status: WaitingIntentStatus::Active,
            external_trigger_id: "trigger-wait-current".into(),
            created_at: Utc::now(),
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        })
        .unwrap();
    let blocked_projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    let work_queue_decision = scheduler::decide_next_action(
        &blocked_projection,
        scheduler::SchedulerBoundary::IdleTick,
        scheduler::SchedulerInput::IdleSignal(scheduler::SchedulerIdleSignal::ContinueActive {
            work_item: &work_item,
            suppressed_after_model_reentry_continuation: false,
            duplicate: None,
        }),
    );
    assert_eq!(blocked_projection.active_agent_waiting_intents, 1);
    assert_eq!(
        work_queue_decision.kind,
        scheduler::SchedulerDecisionKind::EmitSystemTick
    );
    assert_eq!(work_queue_decision.reason, "continue_active");
}

#[test]
fn queued_runnable_work_is_not_suppressed_by_unrelated_agent_waiting_intent() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();
    let now = Utc::now();

    let mut work_item = WorkItemRecord::new("default", "queued work", WorkItemState::Open);
    work_item.id = "work-queued".into();
    work_item.revision = 4;
    storage.append_work_item(&work_item).unwrap();
    storage
        .append_waiting_intent(&WaitingIntentRecord {
            id: "agent-wait".into(),
            agent_id: "default".into(),
            scope: ExternalTriggerScope::Agent,
            work_item_id: None,
            description: "unrelated external wait".into(),
            source: "github".into(),
            resource: None,
            condition: None,
            delivery_mode: CallbackDeliveryMode::WakeHint,
            status: WaitingIntentStatus::Active,
            external_trigger_id: "trigger-agent-wait".into(),
            created_at: now,
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        })
        .unwrap();

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(projection.active_agent_waiting_intents, 1);
    assert_eq!(
        projection
            .work_reactivation_signal()
            .as_ref()
            .map(|signal| { (signal.work_item_id.as_str(), signal.reactivation_mode,) }),
        Some(("work-queued", WorkReactivationMode::ActivateQueued))
    );

    let decision = scheduler::decide_next_action(
        &projection,
        scheduler::SchedulerBoundary::IdleTick,
        scheduler::SchedulerInput::IdleSignal(scheduler::SchedulerIdleSignal::QueuedAvailable {
            work_item: &work_item,
            duplicate: None,
        }),
    );

    assert_eq!(
        decision.kind,
        scheduler::SchedulerDecisionKind::EmitSystemTick
    );
    assert_eq!(decision.reason, "queued_available");
}

#[test]
fn background_work_item_task_does_not_block_runnable_work() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some("work-background".into());
    storage.write_agent(&agent).unwrap();
    let now = Utc::now();

    let mut work_item = WorkItemRecord::new("default", "runnable work", WorkItemState::Open);
    work_item.id = "work-background".into();
    storage.append_work_item(&work_item).unwrap();
    storage
        .append_task(&TaskRecord {
            id: "background-task".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: Some(work_item.id.clone()),
            summary: Some("background task".into()),
            detail: Some(serde_json::json!({ "wait_policy": "background" })),
            recovery: None,
        })
        .unwrap();

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert!(!projection.has_blocking_active_tasks);
    assert_eq!(
        projection.current_work_item_scheduling_state,
        Some(WorkItemSchedulingState::Runnable)
    );
    assert!(scheduler::wait_decision_for_projection(&projection).is_none());
}

#[test]
fn scheduling_diagnostics_detect_idle_posture_with_runnable_work() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    let work_item = WorkItemRecord::new("default", "runnable work", WorkItemState::Open);
    agent.current_work_item_id = Some(work_item.id.clone());
    storage.write_agent(&agent).unwrap();
    storage.append_work_item(&work_item).unwrap();

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    let work_queue = storage.work_queue_prompt_projection().unwrap();
    let posture = AgentPostureProjection {
        posture: AgentSchedulingPosture::Idle,
        reason: "fixture intentionally stale posture".into(),
        work_item_id: None,
        waiting_intent_id: None,
        task_id: None,
        run_id: None,
    };

    let diagnostics = scheduler::scheduling_diagnostics_for_facts(
        &agent,
        &projection,
        &posture,
        &work_queue,
        &[],
        &[],
    );

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, "idle_posture_has_runnable_work");
    assert_eq!(
        diagnostics[0].work_item_id.as_deref(),
        Some(work_item.id.as_str())
    );
}

#[test]
fn scheduling_diagnostics_detect_weak_external_wait_and_unrecoverable_blocker() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();
    let now = Utc::now();

    let mut external = WorkItemRecord::new("default", "external wait", WorkItemState::Open);
    external.blocked_by = Some("github".into());
    storage.append_work_item(&external).unwrap();
    storage
        .append_wait_condition(&WaitConditionRecord {
            id: "wait-weak".into(),
            agent_id: "default".into(),
            work_item_id: Some(external.id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("github".into()),
            subject_ref: Some("pr-1".into()),
            waiting_for: "merged".into(),
            wake_sources: vec![WakeSource::ExternalIngress {
                external_trigger_id: Some("trigger-1".into()),
            }],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
        })
        .unwrap();

    let mut blocked = WorkItemRecord::new("default", "blocked", WorkItemState::Open);
    blocked.blocked_by = Some("manual blocker".into());
    storage.append_work_item(&blocked).unwrap();

    let diagnostics = scheduler::scheduling_diagnostics(&storage, &agent).unwrap();
    let kinds = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.kind.as_str())
        .collect::<Vec<_>>();

    assert!(kinds.contains(&"external_wait_has_weak_recoverability"));
    assert!(kinds.contains(&"blocked_work_item_without_recheck_or_wait"));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == "external_wait_has_weak_recoverability"
            && diagnostic.wait_condition_id.as_deref() == Some("wait-weak")
            && diagnostic.work_item_id.as_deref() == Some(external.id.as_str())
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == "blocked_work_item_without_recheck_or_wait"
            && diagnostic.work_item_id.as_deref() == Some(blocked.id.as_str())
    }));
}

#[test]
fn scheduling_diagnostics_use_authoritative_queue_len() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();

    let diagnostics =
        scheduler::scheduling_diagnostics_with_queue_len(&storage, &agent, 1).unwrap();

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, "idle_posture_has_queued_input");
    assert!(diagnostics[0]
        .evidence
        .iter()
        .any(|entry| entry == "queue_len=1"));
}

#[test]
fn scheduling_diagnostics_do_not_warn_for_common_legal_waits() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();
    let now = Utc::now();

    let mut external =
        WorkItemRecord::new("default", "recoverable external wait", WorkItemState::Open);
    external.blocked_by = Some("github".into());
    storage.append_work_item(&external).unwrap();
    storage
        .append_wait_condition(&WaitConditionRecord {
            id: "wait-recoverable".into(),
            agent_id: "default".into(),
            work_item_id: Some(external.id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("github".into()),
            subject_ref: Some("pr-2".into()),
            waiting_for: "checks".into(),
            wake_sources: vec![WakeSource::Timer {
                wake_at: now + chrono::Duration::hours(1),
            }],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
        })
        .unwrap();

    let mut blocked_with_recheck =
        WorkItemRecord::new("default", "blocked with recheck", WorkItemState::Open);
    blocked_with_recheck.blocked_by = Some("manual blocker".into());
    blocked_with_recheck.recheck_at = Some(now + chrono::Duration::hours(1));
    storage.append_work_item(&blocked_with_recheck).unwrap();

    let diagnostics = scheduler::scheduling_diagnostics(&storage, &agent).unwrap();

    assert!(
        diagnostics.is_empty(),
        "expected no diagnostics for legal waits, got {diagnostics:?}"
    );
}

#[test]
fn scheduler_diagnostic_append_dedupes_interleaved_recent_events() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();
    let now = Utc::now();

    storage
        .append_wait_condition(&WaitConditionRecord {
            id: "wait-weak".into(),
            agent_id: "default".into(),
            work_item_id: None,
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("github".into()),
            subject_ref: Some("pr-1".into()),
            waiting_for: "review".into(),
            wake_sources: vec![WakeSource::ExternalIngress {
                external_trigger_id: Some("trigger-1".into()),
            }],
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
        })
        .unwrap();
    storage
        .append_waiting_intent(&WaitingIntentRecord {
            id: "intent-missing-trigger".into(),
            agent_id: "default".into(),
            scope: ExternalTriggerScope::Agent,
            work_item_id: None,
            description: "fixture waiting intent".into(),
            source: "github".into(),
            resource: Some("pr-1".into()),
            condition: Some("review".into()),
            delivery_mode: CallbackDeliveryMode::WakeHint,
            status: WaitingIntentStatus::Active,
            external_trigger_id: "".into(),
            created_at: now,
            cancelled_at: None,
            last_triggered_at: None,
            trigger_count: 0,
            correlation_id: None,
            causation_id: None,
        })
        .unwrap();

    let appended = scheduler::append_scheduling_diagnostics(&storage, &agent, 0).unwrap();
    assert!(appended >= 2);
    assert_eq!(
        scheduler::append_scheduling_diagnostics(&storage, &agent, 0).unwrap(),
        0
    );

    let events = storage.read_recent_events(10).unwrap();
    let diagnostic_kinds = events
        .iter()
        .filter(|event| event.kind == "scheduler_diagnostic")
        .map(|event| event.data["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(diagnostic_kinds.len(), appended);
    assert!(diagnostic_kinds.contains(&"external_wait_has_weak_recoverability"));
    assert!(diagnostic_kinds.contains(&"waiting_intent_without_external_trigger"));
}

#[test]
fn decide_next_action_records_duplicate_tick_evidence() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();
    let mut work_item = WorkItemRecord::new("default", "queued work", WorkItemState::Open);
    work_item.id = "work-queued".into();
    work_item.revision = 3;
    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();

    let decision = scheduler::decide_next_action(
        &projection,
        scheduler::SchedulerBoundary::IdleTick,
        scheduler::SchedulerInput::IdleSignal(scheduler::SchedulerIdleSignal::QueuedAvailable {
            work_item: &work_item,
            duplicate: Some(
                scheduler::SchedulerDuplicateEvidence::QueuedAvailableMessage("msg-1".into()),
            ),
        }),
    );

    assert_eq!(decision.kind, scheduler::SchedulerDecisionKind::Noop);
    assert_eq!(decision.reason, "duplicate_queued_available");
    assert!(decision
        .evidence
        .iter()
        .any(|entry| entry == "message_id=msg-1"));
}

#[test]
fn scheduler_decision_event_records_evidence_and_bindings() {
    let message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "work_queue".into(),
        },
        TrustLevel::TrustedSystem,
        Priority::Normal,
        MessageBody::Text {
            text: "continue".into(),
        },
    );
    let event = scheduler::scheduler_decision_event(
        &scheduler::SchedulerDecision::new(
            scheduler::SchedulerDecisionKind::StartModelTurn,
            "fixture",
        )
        .message(&message)
        .model_reentry(true)
        .work_item_id("work-1")
        .evidence("fixture_evidence"),
    );

    assert_eq!(event.kind, "scheduler_decision");
    assert_eq!(event.data["decision"].as_str(), Some("StartModelTurn"));
    assert_eq!(event.data["model_reentry"].as_bool(), Some(true));
    assert_eq!(event.data["work_item_id"].as_str(), Some("work-1"));
    assert!(event.data["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value.as_str() == Some("fixture_evidence")));
}

#[test]
fn legacy_child_agent_task_kinds_do_not_gate_scheduler_wait_for_task() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let agent = AgentState::new("default");
    storage.write_agent(&agent).unwrap();
    let now = Utc::now();

    for (id, kind, recovery) in [
        (
            "legacy-inherited",
            TaskKind::SubagentTask,
            TaskRecoverySpec::SubagentTask {
                summary: "legacy inherited".into(),
                prompt: "resume".into(),
                trust: TrustLevel::TrustedOperator,
            },
        ),
        (
            "legacy-worktree",
            TaskKind::WorktreeSubagentTask,
            TaskRecoverySpec::WorktreeSubagentTask {
                summary: "legacy worktree".into(),
                prompt: "resume".into(),
                trust: TrustLevel::TrustedOperator,
            },
        ),
    ] {
        storage
            .append_task(&TaskRecord {
                id: id.into(),
                agent_id: "default".into(),
                kind,
                status: TaskStatus::Running,
                created_at: now,
                updated_at: now,
                parent_message_id: None,
                work_item_id: None,
                summary: Some("legacy child task".into()),
                detail: Some(serde_json::json!({ "wait_policy": "blocking" })),
                recovery: Some(recovery),
            })
            .unwrap();
    }

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(projection.active_tasks.len(), 2);
    assert!(!projection.has_blocking_active_tasks);

    let decision = scheduler::decide_next_action(
        &projection,
        scheduler::SchedulerBoundary::RunLoopIdle,
        scheduler::SchedulerInput::Idle,
    );
    assert_ne!(decision.kind, scheduler::SchedulerDecisionKind::WaitForTask);
}

#[test]
fn scheduler_decision_append_dedupes_identical_latest_event() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let decision = scheduler::SchedulerDecision::new(
        scheduler::SchedulerDecisionKind::WaitForExternalChange,
        "active_waiting_intents",
    )
    .boundary("fixture")
    .liveness_only(true)
    .evidence("active_waiting_intents=1");

    assert!(scheduler::append_scheduler_decision(&storage, &decision).unwrap());
    assert!(!scheduler::append_scheduler_decision(&storage, &decision).unwrap());

    let events = storage.read_recent_events(10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "scheduler_decision");
    assert_eq!(events[0].data["boundary"].as_str(), Some("fixture"));
}

#[test]
fn operator_interjection_classifier_requires_trusted_operator_interjection_prompt() {
    let trusted_interjection = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Interject,
        MessageBody::Text {
            text: "interject".into(),
        },
    );
    assert!(scheduler::is_operator_interjection_message(
        &trusted_interjection
    ));

    let normal_operator = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "normal".into(),
        },
    );
    assert!(!scheduler::is_operator_interjection_message(
        &normal_operator
    ));

    let webhook_interjection = MessageEnvelope::new(
        "default",
        MessageKind::WebhookEvent,
        MessageOrigin::Webhook {
            source: "test".into(),
            event_type: None,
        },
        TrustLevel::TrustedIntegration,
        Priority::Interject,
        MessageBody::Text {
            text: "webhook".into(),
        },
    );
    assert!(!scheduler::is_operator_interjection_message(
        &webhook_interjection
    ));
}
