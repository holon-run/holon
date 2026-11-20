use anyhow::Result;
use holon_domain::*;
use rusqlite::{params, TransactionBehavior};

use super::registry_connection;
use crate::agent_registry::list::collect_agent_list_posture_rows_filtered;
use crate::agent_state::upsert_agent_state_at_tx;
use crate::codec::{enum_string, timestamp};
use crate::work_item::insert_new_work_item_tx;

#[test]
fn zero_history_window_skips_ranking_and_preserves_live_facts() -> Result<()> {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    let mut connection = registry_connection()?;
    let agent_id = "zero-window-agent";
    let now = chrono::Utc::now();
    let tx = connection.transaction()?;
    upsert_agent_state_at_tx(&tx, &AgentState::new(agent_id), now, |_| Ok(()), |_| Ok(()))?;
    let queued = QueueEntryRecord {
        message_id: "queued-input".into(),
        agent_id: agent_id.into(),
        priority: Priority::Normal,
        status: QueueEntryStatus::Queued,
        created_at: now,
        updated_at: now,
    };
    tx.execute(
        "INSERT INTO queue_entries (
            message_id, agent_id, priority, status, created_at, updated_at, payload_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        params![
            queued.message_id,
            queued.agent_id,
            enum_string(&queued.priority)?,
            enum_string(&queued.status)?,
            timestamp(now),
            serde_json::to_string(&queued)?,
        ],
    )?;
    let open = WorkItemRecord::new(agent_id, "open item", WorkItemState::Open);
    insert_new_work_item_tx(&tx, &open)?;
    insert_new_work_item_tx(
        &tx,
        &WorkItemRecord::new(agent_id, "completed item", WorkItemState::Completed),
    )?;
    let trigger = ExternalTriggerRecord {
        external_trigger_id: "active-trigger".into(),
        target_agent_id: agent_id.into(),
        scope: ExternalTriggerScope::Agent,
        delivery_mode: CallbackDeliveryMode::WakeHint,
        token: None,
        token_hash: "hash".into(),
        status: ExternalTriggerStatus::Active,
        created_at: now,
        revoked_at: None,
        last_delivered_at: None,
        delivery_count: 0,
    };
    tx.execute(
        "INSERT INTO external_triggers (
            external_trigger_id, target_agent_id, token_hash, status, created_at, payload_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            trigger.external_trigger_id,
            trigger.target_agent_id,
            trigger.token_hash,
            enum_string(&trigger.status)?,
            timestamp(now),
            serde_json::to_string(&trigger)?,
        ],
    )?;
    tx.commit()?;
    let agent_ids = [agent_id.to_owned()];
    connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Function { function_name }
            if function_name.eq_ignore_ascii_case("row_number") =>
        {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }));
    let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    for status in [RecordListStatus::All, RecordListStatus::Terminal] {
        let rows = collect_agent_list_posture_rows_filtered(&tx, &agent_ids, status, 0)?;
        let row = &rows.rows[0];
        assert!(row.agent_state_json.is_some());
        assert!(row.has_queued_input);
        let facts = row
            .facts
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        assert!(facts.has_queued_input);
        if status == RecordListStatus::All {
            assert_eq!(facts.open_work_items, vec![open.clone()]);
        } else {
            assert!(facts.open_work_items.is_empty());
        }
        assert!(facts.non_open_work_items.is_empty());
        assert_eq!(facts.external_triggers.len(), 1);
        assert_eq!(
            facts.external_triggers[0].external_trigger_id,
            "active-trigger"
        );
        // Nonzero history must still execute ranking, proving the guard detects it.
        let error = collect_agent_list_posture_rows_filtered(&tx, &agent_ids, status, 1)
            .err()
            .expect("history ranking must be denied");
        assert!(
            error
                .to_string()
                .to_ascii_lowercase()
                .contains("row_number"),
            "{error}"
        );
    }
    let rows =
        collect_agent_list_posture_rows_filtered(&tx, &agent_ids, RecordListStatus::Active, 1)?;
    let facts = rows.rows[0]
        .facts
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(facts.open_work_items, vec![open]);
    assert!(facts.non_open_work_items.is_empty());
    Ok(())
}
