use super::*;
use crate::{AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority, TaskKind};

#[test]
fn legacy_payloads_default_to_no_projections() -> Result<()> {
    let now = "2026-11-26T00:00:00Z".parse()?;
    let task = TaskRecord {
        id: "task-a".into(),
        agent_id: "agent-a".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: now,
        updated_at: now,
        parent_message_id: Some("message-a".into()),
        work_item_id: Some("work-a".into()),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": "task-a",
            "rejoin_generation": 1,
            "parent_turn_id": "parent-turn",
        })),
        recovery: None,
    };
    let mut message = MessageEnvelope::new(
        "agent-a",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "result".into(),
        },
    );
    message.id = "message-a".into();
    message.work_item_id.clone_from(&task.work_item_id);

    let record = TaskResultSettlementRecord::pending(&task, &message, now)?.unwrap();
    assert_eq!(
        record.result_identity,
        "task_result_ac5b1154772651227407c7da"
    );
    let mut payload = serde_json::to_value(&record)?;
    let object = payload.as_object_mut().unwrap();
    object.remove("source_projection");
    object.remove("reply_projection");
    let decoded: TaskResultSettlementRecord = serde_json::from_value(payload)?;
    assert_eq!(decoded, record);
    assert!(decoded.source_projection.is_none());
    assert!(decoded.reply_projection.is_none());
    Ok(())
}
