use super::super::*;
use super::support::*;
use crate::types::{
    AgentPostureProjection, AgentSchedulingPosture, ToolExecutionStatus, WaitConditionKind,
    WaitConditionRecord, WaitConditionStatus, WaitingIntentScope, WakeSource, WorkItemPlanStatus,
    WorkItemSchedulingState, WorkReactivationMode,
};
use chrono::DateTime;
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
    #[allow(dead_code)]
    scope: WaitingIntentScope,
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

fn append_active_external_wait_condition(
    storage: &AppStorage,
    id: &str,
    agent_id: &str,
    work_item_id: Option<&str>,
) {
    if let Some(work_item_id) = work_item_id {
        let mut work_item = WorkItemRecord::new(
            agent_id,
            format!("{work_item_id} work"),
            WorkItemState::Open,
        );
        work_item.id = work_item_id.into();
        storage.append_work_item(&work_item).unwrap();
    }
    storage
        .append_wait_condition(&WaitConditionRecord {
            id: id.into(),
            agent_id: agent_id.into(),
            work_item_id: work_item_id.map(ToString::to_string),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: Some("test".into()),
            subject_ref: None,
            waiting_for: format!("{id} wait"),
            wake_sources: vec![WakeSource::ExternalIngress {
                external_trigger_id: Some(format!("trigger-{id}")),
            }],
            continuation: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
        })
        .unwrap();
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
            turn_id: "test-turn".into(),
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
            .append_wait_condition(&WaitConditionRecord {
                id: intent.id.clone(),
                agent_id: "default".into(),
                work_item_id: intent.work_item_id,
                kind: WaitConditionKind::External,
                source: Some("fixture".into()),
                subject_ref: None,
                waiting_for: format!("fixture wait condition {}", intent.id),
                wake_sources: vec![WakeSource::ExternalIngress {
                    external_trigger_id: Some(format!("trigger-{}", intent.id)),
                }],
                continuation: None,
                status: WaitConditionStatus::Active,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,
                turn_id: None,
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
            AuthorityClass::IntegrationSignal,
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
                turn_index: 0,
                turn_id: None,
                tool_name: tool.tool_name,
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
                duration_ms: 1,
                authority_class: AuthorityClass::OperatorInstruction,
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
    let snapshot = storage.recovery_snapshot(&agent.id).unwrap();
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
    append_active_external_wait_condition(&storage, "work-wait", "default", Some("work-1"));
    append_active_external_wait_condition(&storage, "agent-wait", "default", None);
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
        append_active_external_wait_condition(&storage, id, agent_id, None);
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
    append_active_external_wait_condition(&storage, "wait-current", "default", None);

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    let decision = scheduler::idle_boundary_decision(&projection, "fixture");
    assert_eq!(decision.kind, scheduler::SchedulerDecisionKind::Stop);
    assert_eq!(decision.reason, "stopped");
}

#[test]
fn idle_boundary_decision_inspects_wait_facts_while_asleep() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::Asleep;
    storage.write_agent(&agent).unwrap();
    append_active_external_wait_condition(&storage, "wait-current", "default", None);

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    let decision = scheduler::idle_boundary_decision(&projection, "fixture");
    assert_eq!(
        decision.kind,
        scheduler::SchedulerDecisionKind::WaitForExternalChange
    );
    assert_eq!(decision.reason, "active_agent_waiting_intents");
}

#[test]
fn idle_boundary_decision_waits_for_non_current_work_item_wait() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::Asleep;
    storage.write_agent(&agent).unwrap();
    let mut waiting = WorkItemRecord::new("default", "waiting work", WorkItemState::Open);
    waiting.id = "work-waiting".into();
    waiting.blocked_by = Some("task result".into());
    storage.append_work_item(&waiting).unwrap();
    storage
        .append_wait_condition(&WaitConditionRecord {
            id: "wait-task".into(),
            agent_id: "default".into(),
            work_item_id: Some(waiting.id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Task,
            source: None,
            subject_ref: None,
            waiting_for: "task result".into(),
            wake_sources: vec![WakeSource::TaskResult {
                task_id: "task-1".into(),
            }],
            continuation: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
        })
        .unwrap();

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(projection.current_work_item_scheduling_state, None);
    assert_eq!(
        projection.waiting_work_item_scheduling_state,
        Some(WorkItemSchedulingState::WaitingTask)
    );
    let decision = scheduler::idle_boundary_decision(&projection, "fixture");
    assert_eq!(decision.kind, scheduler::SchedulerDecisionKind::WaitForTask);
    assert_eq!(decision.reason, "work_item_task_wait");
    assert_eq!(decision.work_item_id.as_deref(), Some("work-waiting"));
}

#[test]
fn idle_boundary_decision_does_not_treat_asleep_with_queued_input_as_idle() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::Asleep;
    agent.pending = 1;
    storage.write_agent(&agent).unwrap();

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    let decision = scheduler::idle_boundary_decision(&projection, "fixture");
    assert_eq!(decision.kind, scheduler::SchedulerDecisionKind::Noop);
    assert_eq!(decision.reason, "queue_not_empty");
}

#[test]
fn idle_boundary_decision_reactivates_runnable_work_while_asleep() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.status = AgentStatus::Asleep;
    agent.current_work_item_id = Some("work-current".into());
    storage.write_agent(&agent).unwrap();
    let mut work_item = WorkItemRecord::new("default", "continue work", WorkItemState::Open);
    work_item.id = "work-current".into();
    work_item.plan_status = WorkItemPlanStatus::Ready;
    storage.append_work_item(&work_item).unwrap();

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    let decision = scheduler::idle_boundary_decision(&projection, "fixture");
    assert_eq!(
        decision.kind,
        scheduler::SchedulerDecisionKind::EmitSystemTick
    );
    assert_eq!(decision.reason, "runnable_work");
    assert_eq!(decision.work_item_id.as_deref(), Some("work-current"));
    assert!(decision.model_reentry);
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

    append_active_external_wait_condition(&storage, "wait-current", "default", None);
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

    let mut work_item = WorkItemRecord::new("default", "queued work", WorkItemState::Open);
    work_item.id = "work-queued".into();
    work_item.revision = 4;
    storage.append_work_item(&work_item).unwrap();
    append_active_external_wait_condition(&storage, "agent-wait", "default", None);

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

            turn_id: None,
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

            turn_id: None,
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

            turn_id: None,
        })
        .unwrap();
    let appended = scheduler::append_scheduling_diagnostics(&storage, &agent, 0).unwrap();
    assert_eq!(appended, 1);
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
        AuthorityClass::RuntimeInstruction,
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
                authority_class: AuthorityClass::OperatorInstruction,
            },
        ),
        (
            "legacy-worktree",
            TaskKind::WorktreeSubagentTask,
            TaskRecoverySpec::WorktreeSubagentTask {
                summary: "legacy worktree".into(),
                prompt: "resume".into(),
                authority_class: AuthorityClass::OperatorInstruction,
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
        AuthorityClass::OperatorInstruction,
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
        AuthorityClass::OperatorInstruction,
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
        AuthorityClass::IntegrationSignal,
        Priority::Interject,
        MessageBody::Text {
            text: "webhook".into(),
        },
    );
    assert!(!scheduler::is_operator_interjection_message(
        &webhook_interjection
    ));
}

// --- WaitingOperator recheck tests (#1989) ---
//
// When an agent uses WaitFor(wake=operator_input, recheck_after_ms=...), the
// work item enters WaitingOperator with a recheck_at deadline. If recheck_at
// expires and has not been consumed, the scheduler must NOT block on
// WaitForOperator — it should return None so the agent wakes up to
// re-evaluate. If recheck_at was already consumed, or has not yet expired,
// the scheduler returns WaitForOperator as usual.

fn setup_waiting_operator_work_item(
    storage: &AppStorage,
    agent_id: &str,
    work_item_id: &str,
    recheck_at: Option<DateTime<Utc>>,
    recheck_consumed_at: Option<DateTime<Utc>>,
) {
    let mut work_item = WorkItemRecord::new(
        agent_id,
        "operator-input wait with recheck",
        WorkItemState::Open,
    );
    work_item.id = work_item_id.into();
    work_item.recheck_at = recheck_at;
    work_item.recheck_consumed_at = recheck_consumed_at;
    storage.append_work_item(&work_item).unwrap();

    // Attach an active Operator wait condition so the scheduler classifies
    // the work item scheduling_state as WaitingOperator.
    storage
        .append_wait_condition(&WaitConditionRecord {
            id: format!("wait-{work_item_id}"),
            agent_id: agent_id.into(),
            work_item_id: Some(work_item_id.into()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Operator,
            source: None,
            subject_ref: None,
            waiting_for: "operator input".into(),
            wake_sources: vec![],
            continuation: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
        })
        .unwrap();
}

#[test]
fn waiting_operator_with_expired_unconsumed_recheck_lets_agent_wake() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some("work-recheck-expired".into());
    storage.write_agent(&agent).unwrap();

    setup_waiting_operator_work_item(
        &storage,
        "default",
        "work-recheck-expired",
        // recheck_at in the past → expired
        Some(Utc::now() - chrono::Duration::seconds(30)),
        // not consumed
        None,
    );

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();

    // Sanity: the work item is indeed WaitingOperator
    assert_eq!(
        projection.waiting_work_item_scheduling_state,
        Some(WorkItemSchedulingState::WaitingOperator)
    );

    // The fix: expired + unconsumed recheck means the scheduler should NOT
    // block — it returns None so the agent wakes up.
    assert!(scheduler::wait_decision_for_projection(&projection).is_none());
}

#[test]
fn waiting_operator_with_expired_but_consumed_recheck_still_waits() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some("work-recheck-consumed".into());
    storage.write_agent(&agent).unwrap();

    let recheck_at = Utc::now() - chrono::Duration::seconds(30);
    setup_waiting_operator_work_item(
        &storage,
        "default",
        "work-recheck-consumed",
        Some(recheck_at),
        // consumed after recheck_at → no re-pending
        Some(recheck_at + chrono::Duration::seconds(5)),
    );

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(
        projection.waiting_work_item_scheduling_state,
        Some(WorkItemSchedulingState::WaitingOperator)
    );

    let decision = scheduler::wait_decision_for_projection(&projection);
    assert_eq!(
        decision.unwrap().kind,
        scheduler::SchedulerDecisionKind::WaitForOperator
    );
}

#[test]
fn waiting_operator_with_future_recheck_still_waits() {
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some("work-recheck-future".into());
    storage.write_agent(&agent).unwrap();

    setup_waiting_operator_work_item(
        &storage,
        "default",
        "work-recheck-future",
        // recheck_at in the future → not yet expired
        Some(Utc::now() + chrono::Duration::hours(1)),
        None,
    );

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(
        projection.waiting_work_item_scheduling_state,
        Some(WorkItemSchedulingState::WaitingOperator)
    );

    let decision = scheduler::wait_decision_for_projection(&projection);
    assert_eq!(
        decision.unwrap().kind,
        scheduler::SchedulerDecisionKind::WaitForOperator
    );
}
