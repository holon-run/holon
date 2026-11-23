use super::*;

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch("CREATE TABLE agent_states (agent_id TEXT, payload_json TEXT)")?;
    Ok(connection)
}

fn insert(connection: &Connection, id: &str, payload: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO agent_states (agent_id, payload_json) VALUES (?1, ?2)",
        [id, payload],
    )?;
    Ok(())
}

#[test]
fn scalar_scan_preserves_json_identity_and_malformed_payload_semantics() -> Result<()> {
    let connection = connection()?;
    for (id, payload) in [
        ("one", r#"{"id":"one","unknown":{"nested":[1,true]}}"#),
        ("路\"{}", r#"{"id":"\u8def\"{}"}"#),
        ("", r#"{"id":""}"#),
        ("last", r#"{"id":"wrong","id":"last"}"#),
        ("different", r#"{"id":"other"}"#),
        ("missing", "{}"),
        ("number", r#"{"id":42}"#),
        ("null-id", r#"{"id":null}"#),
        ("root-null", "null"),
        ("array", r#"[{"id":"array"}]"#),
        ("string", r#""string""#),
        ("malformed", r#"{"id":"malformed","unfinished":["#),
        ("last-wrong", r#"{"id":"last-wrong","id":"other"}"#),
    ] {
        insert(&connection, id, payload)?;
    }
    assert_eq!(SYNC.count_agent_state_identity_mismatches(&connection)?, 9);
    assert_eq!(SYNC.count_agent_state_identity_mismatches(&connection)?, 9);
    assert!(connection.is_autocommit());
    Ok(())
}
