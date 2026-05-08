use super::super::*;
use super::support::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct SchedulerFixture {
    agent: AgentFixture,
    work_items: Vec<WorkItemFixture>,
    tasks: Vec<TaskFixture>,
}

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
    revision: u64,
}

#[derive(Deserialize)]
struct TaskFixture {
    id: String,
    status: TaskStatus,
    wait_policy: String,
    work_item_id: Option<String>,
}

#[test]
fn scheduler_projection_replays_fixture_facts() {
    let fixture: SchedulerFixture =
        serde_json::from_str(include_str!("../../../tests/fixtures/scheduler/basic.json")).unwrap();
    let dir = tempdir().unwrap();
    let storage = AppStorage::new(dir.path()).unwrap();

    let mut agent = AgentState::new("default");
    agent.current_work_item_id = fixture.agent.current_work_item_id;
    agent.active_task_ids = fixture.agent.active_task_ids;
    if let Some(reason) = fixture.agent.pending_wake_hint_reason {
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

    for item in fixture.work_items {
        let mut record = WorkItemRecord::new("default", item.objective, item.state);
        record.id = item.id;
        record.revision = item.revision;
        storage.append_work_item(&record).unwrap();
    }
    for task in fixture.tasks {
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
        Some("work-active")
    );
    assert_eq!(
        projection
            .current_work_item
            .as_ref()
            .map(|item| item.revision),
        Some(3)
    );
    assert_eq!(projection.queued_work_items, 1);
    assert_eq!(projection.active_tasks.len(), 1);
    assert!(projection.has_blocking_active_tasks);
    assert!(projection.pending_wake_hint);
}
