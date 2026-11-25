use holon_domain::{
    task_generation::{
        command_terminal_rejoin_matches, control_original_rejoin_matches,
        control_terminal_rejoin_matches, same_task_rejoin_generation, TaskRejoinGeneration,
        CONTROL_ORIGINAL_REJOIN, CONTROL_TERMINAL_REJOIN,
    },
    AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority, TaskKind,
    TaskRecord, TaskStatus,
};

#[test]
fn turnless_command_preserves_only_its_original_creation_source() {
    let at = chrono::Utc::now();
    let before = TaskRecord {
        id: "command".into(),
        agent_id: "owner".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: Some("work".into()),
        summary: None,
        detail: Some(serde_json::json!({
            "rejoin_obligation_id": "command",
            "rejoin_generation": 1,
            "command_creation_id": "command",
        })),
        recovery: None,
    };
    let mut message = MessageEnvelope::new(
        "owner",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: before.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "completed".into(),
        },
    );
    message.task_id = Some(before.id.clone());
    message.work_item_id = before.work_item_id.clone();
    message.turn_id = Some("result-turn".into());
    let mut terminal = before.clone();
    terminal.status = TaskStatus::Completed;
    terminal.parent_message_id = Some(message.id.clone());
    terminal.detail.as_mut().unwrap()["result_execution_turn_id"] =
        serde_json::json!("result-turn");
    assert!(command_terminal_rejoin_matches(
        &before,
        &terminal,
        Some(&message)
    ));
    assert!(same_task_rejoin_generation(&before, &terminal));
    assert!(!control_original_rejoin_matches(
        &before,
        &serde_json::json!({"rejoin": null}),
    ));
    assert!(!command_terminal_rejoin_matches(&before, &terminal, None));

    for (field, value) in [
        ("rejoin_obligation_id", serde_json::json!("another-command")),
        ("rejoin_generation", serde_json::json!(2)),
        ("command_creation_id", serde_json::json!("another-command")),
        ("parent_turn_id", serde_json::json!("another-turn")),
    ] {
        let mut other = terminal.clone();
        other.detail.as_mut().unwrap()[field] = value;
        assert!(!command_terminal_rejoin_matches(
            &before,
            &other,
            Some(&message)
        ));
    }
    for field in ["parent_message_id", "work_item_id"] {
        let mut other = terminal.clone();
        if field == "parent_message_id" {
            other.parent_message_id = Some("another-result".into());
        } else {
            other.work_item_id = Some("another-work".into());
        }
        assert!(!command_terminal_rejoin_matches(
            &before,
            &other,
            Some(&message)
        ));
    }
    for field in ["task_id", "agent_id", "work_item_id", "origin"] {
        let mut other = message.clone();
        match field {
            "task_id" => other.task_id = Some("another-command".into()),
            "agent_id" => other.agent_id = "another-owner".into(),
            "work_item_id" => other.work_item_id = Some("another-work".into()),
            _ => {
                other.origin = MessageOrigin::Task {
                    task_id: "another-command".into(),
                }
            }
        }
        assert!(!command_terminal_rejoin_matches(
            &before,
            &terminal,
            Some(&other)
        ));
    }
    let mut rebound = terminal.clone();
    let mut next = message.clone();
    next.turn_id = Some("next-result-turn".into());
    rebound.detail.as_mut().unwrap()["parent_turn_id"] = serde_json::json!("next-result-turn");
    assert!(!command_terminal_rejoin_matches(
        &terminal,
        &rebound,
        Some(&next)
    ));

    let mut partial = before.clone();
    partial
        .detail
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("command_creation_id");
    let mut synthesized = partial.clone();
    synthesized.detail.as_mut().unwrap()["parent_turn_id"] = serde_json::json!("result-turn");
    assert!(!same_task_rejoin_generation(&partial, &before));
    assert!(!command_terminal_rejoin_matches(
        &partial,
        &synthesized,
        Some(&message)
    ));

    for invalid_source in [
        serde_json::Value::Null,
        serde_json::json!("foreign"),
        serde_json::json!(""),
    ] {
        let mut invalid = before.clone();
        invalid.detail.as_mut().unwrap()["command_creation_id"] = invalid_source;
        assert!(TaskRejoinGeneration::of(&invalid).is_none());
    }
    let mut non_command = before.clone();
    non_command.kind = TaskKind::ActorInvocation;
    assert!(TaskRejoinGeneration::of(&non_command).is_none());

    let mut unbound = before.clone();
    unbound.detail = Some(serde_json::json!({"command_creation_id": "command"}));
    assert!(TaskRejoinGeneration::of(&unbound).is_none());

    let original = TaskRejoinGeneration::of(&before).unwrap();
    assert_eq!(
        serde_json::to_value(original).unwrap(),
        serde_json::json!("749d4097cbe4e58184ad96992fec68f97bb5872028223115ce138b00ba0c2f15"),
    );
    let mut control = serde_json::json!({
        (CONTROL_ORIGINAL_REJOIN): original,
        (CONTROL_TERMINAL_REJOIN): TaskRejoinGeneration::of(&terminal).unwrap(),
    });
    assert!(control_original_rejoin_matches(&before, &control));
    assert!(control_terminal_rejoin_matches(&terminal, &control));
    control[CONTROL_ORIGINAL_REJOIN] =
        serde_json::to_value(TaskRejoinGeneration::of(&partial).unwrap()).unwrap();
    assert!(!control_original_rejoin_matches(&before, &control));
}
