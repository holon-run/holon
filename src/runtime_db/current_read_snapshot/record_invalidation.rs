//! Source-table triggers are installed only when the original table exists.
//! Missing historical sources remain an explicit fixed-unavailable observation.
use super::*;
const QUEUE: &str = r#"CREATE TRIGGER IF NOT EXISTS current_read_queue_insert AFTER INSERT ON queue_entries
 BEGIN UPDATE current_read_current SET freshness='records_invalidated' WHERE agent_id=NEW.agent_id; END;
CREATE TRIGGER IF NOT EXISTS current_read_queue_update AFTER UPDATE ON queue_entries
 WHEN OLD.message_id IS NOT NEW.message_id OR OLD.agent_id IS NOT NEW.agent_id OR OLD.priority IS NOT NEW.priority OR OLD.status IS NOT NEW.status OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at OR OLD.payload_json IS NOT NEW.payload_json
 BEGIN UPDATE current_read_current SET freshness='records_invalidated' WHERE agent_id IN(OLD.agent_id,NEW.agent_id); END;
CREATE TRIGGER IF NOT EXISTS current_read_queue_delete AFTER DELETE ON queue_entries
 BEGIN UPDATE current_read_current SET freshness='records_invalidated' WHERE agent_id=OLD.agent_id; END;
"#;
const EVENT: &str = r#"CREATE TRIGGER IF NOT EXISTS current_read_event_insert AFTER INSERT ON audit_events
 BEGIN UPDATE current_read_current SET freshness='records_invalidated' WHERE agent_id=NEW.agent_id; END;
CREATE TRIGGER IF NOT EXISTS current_read_event_update AFTER UPDATE ON audit_events
 WHEN OLD.audit_event_id IS NOT NEW.audit_event_id OR OLD.event_seq IS NOT NEW.event_seq OR OLD.agent_id IS NOT NEW.agent_id OR OLD.kind IS NOT NEW.kind OR OLD.created_at IS NOT NEW.created_at OR OLD.data_json IS NOT NEW.data_json
 BEGIN UPDATE current_read_current SET freshness='records_invalidated' WHERE agent_id IN(OLD.agent_id,NEW.agent_id); END;
CREATE TRIGGER IF NOT EXISTS current_read_event_delete AFTER DELETE ON audit_events
 BEGIN UPDATE current_read_current SET freshness='records_invalidated' WHERE agent_id=OLD.agent_id; END;
"#;
pub(super) fn install_tx(tx: &Transaction<'_>) -> Result<()> {
    for (table, triggers) in [("queue_entries", QUEUE), ("audit_events", EVENT)] {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        if exists {
            tx.execute_batch(triggers)?;
        }
    }
    Ok(())
}
