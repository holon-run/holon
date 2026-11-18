use holon_domain::work_item::{WorkItemPlanStatus, WorkItemRecord, WorkItemSchedulingState};
use holon_domain::AGENT_HOME_WORKSPACE_ID;

#[test]
fn legacy_work_item_defaults_and_wire_names_remain_stable() {
    let record: WorkItemRecord = serde_json::from_value(serde_json::json!({
        "id": "work_legacy",
        "session_id": "default",
        "objective": "ship",
        "state": "open",
        "plan_status": "needs_input",
        "created_at": "2026-04-20T00:00:00Z",
        "updated_at": "2026-04-20T00:00:00Z"
    }))
    .unwrap();

    assert_eq!(record.workspace_id, AGENT_HOME_WORKSPACE_ID);
    assert_eq!(record.revision, 1);
    assert_eq!(record.plan_status, WorkItemPlanStatus::NeedsInput);
    let serialized = serde_json::to_value(&record).unwrap();
    assert_eq!(serialized["agent_id"], "default");
    assert!(serialized.get("session_id").is_none());
    assert_eq!(
        serde_json::to_value(WorkItemSchedulingState::YieldedToWorkItem).unwrap(),
        serde_json::json!("yielded_to_work_item")
    );
}
