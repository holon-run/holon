use super::super::*;
use super::support::*;
use crate::types::WorkItemPlanStatus;
use serde::Deserialize;

#[derive(Deserialize)]
struct AgentFixture {
    current_work_item_id: Option<String>,
    active_task_ids: Vec<String>,
    pending_wake_hint_reason: Option<String>,
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
}

#[test]
fn scheduler_projection_replays_fixture_facts() {
    let agent_fixture: AgentFixture = serde_json::from_str(include_str!(
        "../../../tests/fixtures/scheduler/basic/agent.json"
    ))
    .unwrap();
    let work_items: Vec<WorkItemFixture> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/scheduler/basic/ledger/work_items.json"
    ))
    .unwrap();
    let tasks: Vec<TaskFixture> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/scheduler/basic/ledger/tasks.json"
    ))
    .unwrap();
    let expected: ExpectedFixture = serde_json::from_str(include_str!(
        "../../../tests/fixtures/scheduler/basic/expected.json"
    ))
    .unwrap();
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();

    let mut agent = AgentState::new("default");
    agent.current_work_item_id = agent_fixture.current_work_item_id;
    agent.active_task_ids = agent_fixture.active_task_ids;
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

    let projection = scheduler::SchedulerProjection::from_state(&storage, &agent).unwrap();
    assert_eq!(
        projection
            .current_work_item
            .as_ref()
            .map(|item| item.id.as_str()),
        expected.current_work_item_id.as_deref()
    );
    assert_eq!(
        projection
            .current_work_item
            .as_ref()
            .map(|item| item.revision),
        expected.current_work_item_revision
    );
    assert_eq!(projection.queued_work_items, expected.queued_work_items);
    assert_eq!(projection.active_tasks.len(), expected.active_tasks);
    assert_eq!(
        projection.has_blocking_active_tasks,
        expected.has_blocking_active_tasks
    );
    assert_eq!(projection.pending_wake_hint, expected.pending_wake_hint);
    assert_eq!(
        projection.active_waiting_intents,
        expected.active_waiting_intents
    );
    assert_eq!(
        projection.active_work_item_waiting_intents,
        expected.active_work_item_waiting_intents
    );
    assert_eq!(
        projection.active_agent_waiting_intents,
        expected.active_agent_waiting_intents
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
