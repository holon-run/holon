use super::*;
use rusqlite::params;

fn insert_linked_brief(
    connection: &Connection,
    agent_id: &str,
    brief_id: &str,
    event_id: &str,
    event_seq: i64,
) -> Result<()> {
    connection.execute(
        "INSERT INTO briefs (
           evidence_id, agent_id, created_at, kind, preview, payload_json, created_event_seq
         ) VALUES (?1, ?2, '2026-01-01T00:00:00.000Z', 'result', 'preview', '{}', ?3)",
        params![brief_id, agent_id, event_seq],
    )?;
    connection.execute(
        "INSERT INTO audit_events (
           audit_event_id, event_seq, agent_id, kind, created_at, data_json
         ) VALUES (
           ?1, ?2, ?3, 'brief_created', '2026-01-01T00:00:00.000Z',
           json_object('data', json_object('agent_id', ?3, 'brief_id', ?4))
         )",
        params![event_id, event_seq, agent_id, brief_id],
    )?;
    Ok(())
}

#[test]
fn brief_atomic_linkage_scopes_shared_sequences_by_agent() -> Result<()> {
    let connection = super::migrated_connection()?;

    insert_linked_brief(&connection, "alpha", "brief_alpha", "event_alpha", 1)?;
    insert_linked_brief(&connection, "beta", "brief_beta", "event_beta", 1)?;

    assert!(verify_brief_atomic_linkage(&connection)?);
    Ok(())
}

#[test]
fn brief_atomic_linkage_rejects_same_agent_shared_sequence() -> Result<()> {
    let connection = super::migrated_connection()?;
    connection.execute_batch(
        "DROP INDEX briefs_agent_created_event_seq;
         DROP INDEX idx_audit_events_agent_event_seq_unique;",
    )?;

    insert_linked_brief(&connection, "alpha", "brief_one", "event_one", 1)?;
    insert_linked_brief(&connection, "alpha", "brief_two", "event_two", 1)?;

    assert!(!verify_brief_atomic_linkage(&connection)?);
    Ok(())
}
