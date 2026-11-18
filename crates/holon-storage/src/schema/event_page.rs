//! Indexed selection is an optimization, never authority to decode an Event.
macro_rules! event_page_kind_key {
    () => {
        r#"CASE WHEN json_valid(data_json) THEN
          CASE WHEN json_type(data_json,'$.kind')='text'
            AND json_extract(data_json,'$.kind')=kind
            AND json_type(data_json,'$.id')='text'
            AND json_extract(data_json,'$.id')=audit_event_id
            AND json_type(data_json,'$.event_seq')='integer'
            AND json_extract(data_json,'$.event_seq')=event_seq
            AND instr(data_json,'\u')=0
            AND json_remove(data_json,
              '$.id','$.event_seq','$.event_log_epoch','$.created_at','$.kind',
              '$.contract_version','$.payload_schema','$.payload_schema_version','$.data')
              =json_remove(json_remove(data_json,
                '$.id','$.event_seq','$.event_log_epoch','$.created_at','$.kind',
                '$.contract_version','$.payload_schema','$.payload_schema_version','$.data'),
                '$.id','$.event_seq','$.event_log_epoch','$.created_at','$.kind',
                '$.contract_version','$.payload_schema','$.payload_schema_version','$.data')
          THEN kind END
        END"#
    };
}

// SQLite accepts duplicate fields that AuditEvent's serde decoder rejects.
// Removing known fields twice must be idempotent; escaped keys conservatively
// use fallback because SQLite does not resolve them like serde does.
// NULL is the disjoint fallback stream: legacy/mismatched/malformed records
// still go through the canonical decoder and filter, not a normalized shortcut.
pub const EVENT_PAGE_KIND_KEY: &str = event_page_kind_key!();
pub const EVENT_PAGE_KIND_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS idx_audit_events_agent_canonical_kind_seq \
     ON audit_events(agent_id,(",
    event_page_kind_key!(),
    "),event_seq,created_at);"
);

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use rusqlite::{params, Connection};

    #[test]
    fn canonical_kind_index_preserves_legacy_mismatch_and_corrupt_fallback() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE audit_events(
               audit_event_id TEXT PRIMARY KEY,event_seq INTEGER,agent_id TEXT,
               kind TEXT NOT NULL,created_at TEXT NOT NULL,data_json TEXT NOT NULL
             );",
        )?;
        let bodies = [
            r#"{"id":"event-1","event_seq":1,"kind":"target"}"#,
            r#"{"id":"event-2","event_seq":2,"kind":"other"}"#,
            r#"{"id":"foreign","event_seq":3,"kind":"target"}"#,
            r#"{"id":"event-4","event_seq":999,"kind":"target"}"#,
            "{",
            r#"{"id":"event-6","event_seq":6,"kind":null}"#,
            r#"{"id":"event-7","kind":"target"}"#,
        ];
        for (index, body) in bodies.iter().enumerate() {
            connection.execute(
                "INSERT INTO audit_events VALUES(?1,?2,'owner','target','now',?3)",
                params![format!("event-{}", index + 1), index + 1, body],
            )?;
        }
        // Migration must tolerate already-stored malformed/legacy bodies.
        connection.execute_batch(EVENT_PAGE_KIND_INDEX)?;
        let sql = format!(
            "SELECT event_seq,({EVENT_PAGE_KIND_KEY}) FROM audit_events ORDER BY event_seq"
        );
        let selected = connection
            .prepare(&sql)?
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(selected[0], (1, Some("target".into())));
        assert!(selected[1..].iter().all(|(_, kind)| kind.is_none()));
        let query = format!(
            "SELECT audit_event_id FROM audit_events
             WHERE agent_id=?1 AND ({EVENT_PAGE_KIND_KEY})=?2
             ORDER BY event_seq DESC,created_at DESC"
        );
        let plan = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))?
            .query_map(["owner", "target"], |row| row.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(plan
            .iter()
            .any(|line| line.contains("idx_audit_events_agent_canonical_kind_seq")));
        assert!(plan.iter().all(|line| !line.contains("TEMP B-TREE")));
        let mut event = holon_domain::AuditEvent::legacy("target", serde_json::json!({}));
        event.id = "event-1".into();
        event.event_seq = 1;
        let body = serde_json::to_string(&event)?;
        for field in serde_json::from_str::<serde_json::Value>(&body)?
            .as_object()
            .unwrap()
            .keys()
        {
            for key in [
                field.to_owned(),
                format!(r"\u{:04x}{}", field.as_bytes()[0], &field[1..]),
            ] {
                let duplicate = format!(r#"{},"{key}":null}}"#, &body[..body.len() - 1]);
                connection.execute(
                    "UPDATE audit_events SET data_json=?1 WHERE event_seq=1",
                    [duplicate],
                )?;
                let selected: Option<String> = connection.query_row(
                    &format!("SELECT ({EVENT_PAGE_KIND_KEY}) FROM audit_events WHERE event_seq=1"),
                    [],
                    |row| row.get(0),
                )?;
                assert!(
                    selected.is_none(),
                    "duplicate field {key} must use fallback"
                );
            }
        }
        connection.execute(
            "UPDATE audit_events SET data_json=?1 WHERE event_seq=1",
            [body],
        )?;
        // An update invalidates the optimization key rather than hiding a row.
        connection.execute("UPDATE audit_events SET kind='other' WHERE event_seq=1", [])?;
        let selected: Option<String> = connection.query_row(
            &format!("SELECT ({EVENT_PAGE_KIND_KEY}) FROM audit_events WHERE event_seq=1"),
            [],
            |row| row.get(0),
        )?;
        assert!(selected.is_none());
        Ok(())
    }
}
