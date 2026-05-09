use super::super::*;
use super::support::*;
use crate::types::WorkItemPlanStatus;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct AgentFixture {
    #[serde(default)]
    current_work_item_id: Option<String>,
    #[serde(default)]
    active_task_ids: Vec<String>,
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

fn build_scheduler_fixture(name: &str) -> (tempfile::TempDir, AppStorage, AgentState) {
    let agent_fixture: AgentFixture = read_scheduler_fixture(name, "agent.json");
    let work_items: Vec<WorkItemFixture> =
        read_optional_scheduler_fixture(name, "ledger/work_items.json");
    let tasks: Vec<TaskFixture> = read_optional_scheduler_fixture(name, "ledger/tasks.json");
    let waiting_intents: Vec<WaitingIntentFixture> =
        read_optional_scheduler_fixture(name, "ledger/waiting_intents.json");
    let timers: Vec<TimerFixture> = read_optional_scheduler_fixture(name, "ledger/timers.json");
    let messages: Vec<MessageFixture> =
        read_optional_scheduler_fixture(name, "ledger/messages.json");
    let queue_entries: Vec<QueueEntryFixture> =
        read_optional_scheduler_fixture(name, "ledger/queue_entries.json");
    let events: Vec<EventFixture> = read_optional_scheduler_fixture(name, "ledger/events.json");
    let briefs: Vec<BriefFixture> = read_optional_scheduler_fixture(name, "ledger/briefs.json");
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();

    let mut agent = AgentState::new("default");
    agent.current_work_item_id = agent_fixture.current_work_item_id;
    agent.active_task_ids = agent_fixture.active_task_ids;
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
    if !expected.replay_message_ids.is_empty() {
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
    }
    if let Some(expected_decision) = expected.decision {
        let decision = scheduler::idle_boundary_decision(&projection, "fixture");
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
        "baseline_over_budget_terminal",
    ] {
        assert_scheduler_fixture(name);
    }
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
fn scheduler_projection_filters_waiting_intents_and_timers_by_agent() {
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

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
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
        &scheduler::message_processing_decision(&message, true, true)
            .work_item_id("work-1")
            .evidence("fixture_evidence"),
    );

    assert_eq!(event.kind, "scheduler_decision");
    assert_eq!(event.data["decision"].as_str(), Some("StartModelTurn"));
    assert_eq!(event.data["model_visible"].as_bool(), Some(true));
    assert_eq!(event.data["work_item_id"].as_str(), Some("work-1"));
    assert!(event.data["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value.as_str() == Some("fixture_evidence")));
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
