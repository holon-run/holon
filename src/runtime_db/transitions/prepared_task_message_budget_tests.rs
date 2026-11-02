//! Message proof material belongs to the original Task transaction budget.
use super::*;

#[test]
fn task_message_existing_body_is_reserved_before_duplicate_validation() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let mut queue = f.queue()?;
    queue.command.message_evidence[0].body = MessageBody::Text {
        text: "旧正文".repeat(64 * 1024),
    };
    let source = queue.command.message_evidence[0].clone();
    let admitted =
        PreparedInputTransition::queue(f.scope.clone(), "large-old-source".into(), f.at, queue)?;
    drop(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&admitted)?,
    );
    let old_payload: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM messages WHERE message_id=?1",
        [&source.id],
        |r| r.get(0),
    )?;
    let mut next = command(&f, "task:duplicate-source", TaskStatus::Running);
    let mut small = source;
    small.body = MessageBody::Text {
        text: "small conflicting duplicate".into(),
    };
    next.message_evidence.push(small);
    let original = freeze(&f, "task:duplicate-source", next)?;
    let mut request = load_request();
    request.byte_limit = 1024 * 1024;
    let error = execute(&f, &original, &request, &ledger).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(
                "original writer material"
            ))
        ),
        "{error:#}"
    );
    assert!(f.db.tasks().latest("task:duplicate-source")?.is_none());
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT payload_json FROM messages WHERE message_id=?1",
            [&original_message_id()],
            |r| r.get::<_, String>(0),
        )?,
        old_payload
    );
    assert_eq!(f.receipt_count()?, 1);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
fn original_message_id() -> String {
    "message:prepared-input".into()
}

#[test]
fn task_new_message_frozen_body_is_reserved_before_any_original_insert() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let mut queue = f.queue()?;
    let mut source = queue.command.message_evidence.remove(0);
    source.body = MessageBody::Text {
        text: "x".repeat(256 * 1024),
    };
    let mut next = command(&f, "task:new-large-source", TaskStatus::Running);
    next.message_evidence.push(source);
    let original = freeze(&f, "task:new-large-source", next)?;
    let mut request = load_request();
    request.byte_limit = 1024 * 1024;
    let error = execute(&f, &original, &request, &ledger).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(
                "original writer material"
            ))
        ),
        "{error:#}"
    );
    assert!(f.db.tasks().latest("task:new-large-source")?.is_none());
    assert_eq!(
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r
                .get::<_, usize>(0))?,
        0
    );
    assert_eq!(f.receipt_count()?, 0);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn task_message_actual_assignment_length_is_reserved_before_projection_read() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let queue = f.queue()?;
    let source = queue.command.message_evidence[0].clone();
    let admitted = PreparedInputTransition::queue(
        f.scope.clone(),
        "small-assigned-source".into(),
        f.at,
        queue,
    )?;
    drop(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&admitted)?,
    );
    let large_turn = "x".repeat(256 * 1024);
    f.db.connection()?.execute(
        "INSERT INTO conversation_input_assignments(message_id,agent_id,turn_id,revision,assigned_at) VALUES(?1,?2,?3,1,?4)",
        params![source.id, f.scope.agent_id, large_turn, crate::runtime_db::repositories::timestamp(f.at)],
    )?;
    let mut next = command(&f, "task:assignment-material", TaskStatus::Running);
    next.message_evidence.push(source);
    let original = freeze(&f, "task:assignment-material", next)?;
    let mut request = load_request();
    request.byte_limit = 1024 * 1024;
    let error = execute(&f, &original, &request, &ledger).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(
                "original writer material"
            ))
        ),
        "{error:#}"
    );
    assert!(f.db.tasks().latest("task:assignment-material")?.is_none());
    assert_eq!(f.receipt_count()?, 1);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(f.db.connection()?.query_row(
        "SELECT length(CAST(turn_id AS BLOB)) FROM conversation_input_assignments WHERE message_id='message:prepared-input'",[],|r|r.get::<_,usize>(0))?,large_turn.len());
    Ok(())
}

#[test]
fn task_message_canonical_id_length_survives_normalized_message_id_drift() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = ledger()?;
    let mut queue = f.queue()?;
    queue.command.message_evidence[0].body = MessageBody::Text {
        text: "x".repeat(256 * 1024),
    };
    let mut source = queue.command.message_evidence[0].clone();
    let admitted = PreparedInputTransition::queue(
        f.scope.clone(),
        "canonical-old-source".into(),
        f.at,
        queue,
    )?;
    drop(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&admitted)?,
    );
    f.db.connection()?.execute(
        "UPDATE messages SET message_id=NULL WHERE evidence_id=?1",
        [&source.id],
    )?;
    source.body = MessageBody::Text {
        text: "small".into(),
    };
    let mut next = command(&f, "task:canonical-id-material", TaskStatus::Running);
    next.message_evidence.push(source);
    let original = freeze(&f, "task:canonical-id-material", next)?;
    let mut request = load_request();
    request.byte_limit = 1024 * 1024;
    let error = execute(&f, &original, &request, &ledger).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(
                "original writer material"
            ))
        ),
        "{error:#}"
    );
    assert!(f.db.tasks().latest("task:canonical-id-material")?.is_none());
    assert_eq!(f.receipt_count()?, 1);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        f.db.connection()?.query_row(
            "SELECT message_id FROM messages WHERE evidence_id='message:prepared-input'",
            [],
            |r| r.get::<_, Option<String>>(0)
        )?,
        None
    );
    Ok(())
}
