use super::*;

#[test]
fn offline_event_after_history_schema_has_original_ack() -> Result<()> {
    let f = Fixture::new()?;
    let after = AuditEvent::legacy("offline-after-history", serde_json::json!({}));
    f.db.append_offline_observation_event(&f.scope.agent_id, &after)?;
    let acked: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM event_history_acks WHERE audit_event_id=?1",
        [&after.id],
        |row| row.get(0),
    )?;
    assert_eq!(acked, 1);
    Ok(())
}

#[test]
fn offline_event_missing_installed_history_table_rolls_back() -> Result<()> {
    let f = Fixture::new()?;
    f.db.connection()?
        .execute_batch("DROP TABLE event_history_acks;")?;
    let event = AuditEvent::legacy("offline-missing-history", serde_json::json!({}));
    assert!(f
        .db
        .append_offline_observation_event(&f.scope.agent_id, &event)
        .is_err());
    let count: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE audit_event_id=?1",
        [&event.id],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    Ok(())
}
