//! Turn row persistence and input assignment.

use anyhow::{Context, Result};
use holon_domain::TurnRecord;
use rusqlite::{params, OptionalExtension, Transaction};

use crate::codec::{enum_string, timestamp};
use crate::conversation::{assigned_turn_id_tx, RevisionWrites};
use crate::RuntimeStateTransitionConflict;

pub fn upsert_turn_record_tx(
    tx: &Transaction<'_>,
    record: &TurnRecord,
    revisions: &RevisionWrites,
) -> Result<()> {
    if let Some(owner) = record.owner.as_ref() {
        let owner_work_item_id = owner.work_item_id();
        if record.current_work_item_id.as_deref() != owner_work_item_id {
            return Err(RuntimeStateTransitionConflict::new(
                "turn owner",
                &record.turn_id,
                owner_work_item_id.unwrap_or("unbound"),
                record.current_work_item_id.as_deref().unwrap_or("unbound"),
            )
            .into());
        }
    }
    let existing_payload = tx
        .query_row(
            "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
            [&record.turn_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let existing = existing_payload
        .as_deref()
        .map(decode_turn_record_payload)
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        validate_turn_record_identity(existing, record)?;
    }
    let payload_json = serde_json::to_string(record)?;
    let terminal_kind = record
        .terminal
        .as_ref()
        .map(|terminal| enum_string(&terminal.kind))
        .transpose()?;
    let completed_at = record
        .terminal
        .as_ref()
        .map(|terminal| timestamp(terminal.completed_at));
    let (owner_kind, owner_id) = match record.owner.as_ref() {
        Some(owner) => {
            let (kind, id) = owner.index_parts();
            (Some(kind), id)
        }
        None => (None, None),
    };
    let has_owner_columns = tx.query_row(
        "SELECT EXISTS(
            SELECT 1
              FROM pragma_table_info('turn_records')
             WHERE name = 'owner_kind'
        )",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0;
    let has_replay_source_column = tx.query_row(
        "SELECT EXISTS(
            SELECT 1
              FROM pragma_table_info('turn_records')
             WHERE name = 'replay_source_turn_id'
        )",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0;
    if has_owner_columns && has_replay_source_column {
        tx.execute(
            "INSERT INTO turn_records (
                turn_id, turn_index, agent_id, run_id, current_work_item_id,
                owner_kind, owner_id, trigger_message_id, replay_source_turn_id,
                terminal_kind, created_at, completed_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(turn_id) DO UPDATE SET
                owner_kind = COALESCE(turn_records.owner_kind, excluded.owner_kind),
                owner_id = COALESCE(turn_records.owner_id, excluded.owner_id),
                replay_source_turn_id = excluded.replay_source_turn_id,
                terminal_kind = excluded.terminal_kind,
                completed_at = excluded.completed_at,
                payload_json = excluded.payload_json
             WHERE COALESCE(excluded.completed_at, excluded.created_at) >= COALESCE(turn_records.completed_at, turn_records.created_at)",
            params![
                record.turn_id,
                record.turn_index as i64,
                record.agent_id,
                record.run_id,
                record.current_work_item_id,
                owner_kind,
                owner_id,
                record
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref()),
                record
                    .replay
                    .as_ref()
                    .map(|replay| replay.source_turn_id.as_str()),
                terminal_kind,
                timestamp(record.created_at),
                completed_at,
                payload_json,
            ],
        )?;
    } else if has_owner_columns {
        tx.execute(
            "INSERT INTO turn_records (
                turn_id, turn_index, agent_id, run_id, current_work_item_id,
                owner_kind, owner_id, trigger_message_id, terminal_kind, created_at,
                completed_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(turn_id) DO UPDATE SET
                owner_kind = COALESCE(turn_records.owner_kind, excluded.owner_kind),
                owner_id = COALESCE(turn_records.owner_id, excluded.owner_id),
                terminal_kind = excluded.terminal_kind,
                completed_at = excluded.completed_at,
                payload_json = excluded.payload_json
             WHERE COALESCE(excluded.completed_at, excluded.created_at) >= COALESCE(turn_records.completed_at, turn_records.created_at)",
            params![
                record.turn_id,
                record.turn_index as i64,
                record.agent_id,
                record.run_id,
                record.current_work_item_id,
                owner_kind,
                owner_id,
                record
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref()),
                terminal_kind,
                timestamp(record.created_at),
                completed_at,
                payload_json,
            ],
        )?;
    } else {
        tx.execute(
            "INSERT INTO turn_records (
                turn_id, turn_index, agent_id, run_id, current_work_item_id,
                trigger_message_id, terminal_kind, created_at, completed_at, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(turn_id) DO UPDATE SET
                terminal_kind = excluded.terminal_kind,
                completed_at = excluded.completed_at,
                payload_json = excluded.payload_json
             WHERE COALESCE(excluded.completed_at, excluded.created_at) >= COALESCE(turn_records.completed_at, turn_records.created_at)",
            params![
                record.turn_id,
                record.turn_index as i64,
                record.agent_id,
                record.run_id,
                record.current_work_item_id,
                record
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref()),
                terminal_kind,
                timestamp(record.created_at),
                completed_at,
                payload_json,
            ],
        )?;
    }
    let stored_payload = tx.query_row(
        "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
        [&record.turn_id],
        |row| row.get::<_, String>(0),
    )?;
    let stored = decode_turn_record_payload(&stored_payload)?;
    let changed = existing_payload.as_deref() != Some(stored_payload.as_str());
    if changed {
        let _ = revisions.bump_turn_revision_tx(
            tx,
            &stored.agent_id,
            &stored.turn_id,
            true,
            false,
            stored
                .terminal
                .as_ref()
                .map_or(stored.created_at, |terminal| terminal.completed_at),
        )?;
    } else {
        let _ = revisions.ensure_turn_revision_tx(
            tx,
            &stored.agent_id,
            &stored.turn_id,
            stored.created_at,
        )?;
    }
    if stored
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.no_brief_reason.as_ref())
        .is_some()
    {
        let updated_at = stored
            .terminal
            .as_ref()
            .map_or(stored.created_at, |terminal| terminal.completed_at);
        let _ =
            revisions.settle_turn_result_tx(tx, &stored.agent_id, &stored.turn_id, updated_at)?;
    }
    for message_id in &stored.input_message_ids {
        let existing_assignment = assigned_turn_id_tx(tx, &stored.agent_id, message_id)?;
        let assignment_turn_id = if let Some(existing_turn_id) = existing_assignment.as_deref() {
            existing_turn_id
        } else if let Some(replay) = stored
            .replay
            .as_ref()
            .filter(|replay| replay.source_message_id == *message_id)
        {
            let source_owns_input = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1
                    FROM turn_records AS source
                    WHERE source.agent_id = ?1
                      AND source.turn_id = ?2
                      AND EXISTS (
                        SELECT 1
                        FROM json_each(source.payload_json, '$.input_message_ids') AS input
                        WHERE input.type = 'text' AND input.value = ?3
                      )
                 )",
                params![stored.agent_id, replay.source_turn_id, message_id],
                |row| row.get::<_, i64>(0),
            )? != 0;
            if source_owns_input {
                replay.source_turn_id.as_str()
            } else {
                stored.turn_id.as_str()
            }
        } else {
            stored.turn_id.as_str()
        };
        let assignment_target_exists = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM turn_records
                WHERE agent_id = ?1 AND turn_id = ?2
             )",
            params![stored.agent_id, assignment_turn_id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        anyhow::ensure!(
            assignment_target_exists,
            "conversation input assignment target is not persisted: agent_id={}, message_id={}, turn_id={}",
            stored.agent_id,
            message_id,
            assignment_turn_id
        );
        let _ = revisions.assign_input_to_turn_tx(
            tx,
            &stored.agent_id,
            message_id,
            assignment_turn_id,
            stored.created_at,
        )?;
    }
    Ok(())
}

pub fn validate_turn_record_identity(existing: &TurnRecord, record: &TurnRecord) -> Result<()> {
    let replay_identity_matches = match (&existing.replay, &record.replay) {
        (Some(existing), Some(record)) => {
            existing.source_message_id == record.source_message_id
                && existing.source_turn_id == record.source_turn_id
                && existing.reason == record.reason
        }
        (None, None) => true,
        _ => false,
    };
    let identity_matches = existing.agent_id == record.agent_id
        && existing.turn_index == record.turn_index
        && existing.run_id == record.run_id
        && existing.current_work_item_id == record.current_work_item_id
        && (existing.owner == record.owner || existing.owner.is_none())
        && (existing.attribution == record.attribution || existing.attribution.is_none())
        && existing
            .trigger
            .as_ref()
            .and_then(|trigger| trigger.message_id.as_deref())
            == record
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
        && existing.created_at == record.created_at
        && replay_identity_matches;
    if !identity_matches {
        return Err(RuntimeStateTransitionConflict::new(
            "turn identity",
            &record.turn_id,
            "immutable",
            "conflicting",
        )
        .into());
    }
    Ok(())
}

pub fn decode_turn_record_payload(payload: &str) -> Result<TurnRecord> {
    serde_json::from_str(payload).context("decoding turn record payload from runtime db")
}
