use super::*;

#[test]
fn slim_wait_keeps_timing_and_task_binding_without_internal_payloads() {
    let record: holon_domain::WaitConditionRecord = serde_json::from_value(serde_json::json!({
        "id": "wait-1", "agent_id": "agent-1", "work_item_id": "work-1",
        "kind": "task", "status": "active", "waiting_for": "task_completed",
        "created_at": "2026-09-16T00:00:00Z", "updated_at": "2026-09-16T00:01:00Z",
        "wake_sources": [{"kind": "task_result", "task_id": "task-1"}, {"kind": "operator_input"}],
        "continuation": {"private": "internal"}, "subject_ref": "private-callback"
    }))
    .unwrap();
    let value = serde_json::to_value(SlimWaitDto::from(record)).unwrap();
    assert_eq!(value["task_ids"], serde_json::json!(["task-1"]));
    assert_eq!(value["work_item_id"], "work-1");
    assert_eq!(value["created_at"], "2026-09-16T00:00:00Z");
    assert!(value.get("continuation").is_none());
    assert!(value.get("subject_ref").is_none());
    assert!(value.get("waiting_for").is_none());
}
