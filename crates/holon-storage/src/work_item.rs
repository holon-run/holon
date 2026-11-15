//! WorkItem row persistence and revision checks.

use anyhow::{Context, Result};
use holon_domain::{WorkItemRecord, WorkItemState};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::codec::{enum_string, timestamp};
use crate::RuntimeStateTransitionConflict;

pub enum WorkImportOutcome {
    ActualWrite,
    LegacyEqual,
    Ignored,
}

pub fn insert_new_work_item_tx(tx: &Transaction<'_>, record: &WorkItemRecord) -> Result<bool> {
    if record.revision != 1 {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "invalid_initial_revision",
            Some(1),
            Some(record.revision),
            false,
        )
        .into());
    }
    let actual_revision = tx
        .query_row(
            "SELECT revision FROM work_items WHERE work_item_id = ?1",
            [&record.id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|revision| u64::try_from(revision).context("stored work item revision is negative"))
        .transpose()?;
    if let Some(actual_revision) = actual_revision {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "record_exists",
            None,
            Some(actual_revision),
            false,
        )
        .into());
    }
    import_work_item_tx(tx, record)?;
    Ok(true)
}

pub fn update_expected_work_item_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
    expected_revision: u64,
) -> Result<bool> {
    let next_revision = expected_revision.checked_add(1).ok_or_else(|| {
        RuntimeStateTransitionConflict::revision(
            &record.id,
            "invalid_revision_transition",
            Some(expected_revision),
            Some(record.revision),
            false,
        )
    })?;
    if record.revision != next_revision {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "invalid_revision_transition",
            Some(expected_revision),
            Some(record.revision),
            false,
        )
        .into());
    }

    let existing = tx
        .query_row(
            "SELECT revision, payload_json FROM work_items WHERE work_item_id = ?1",
            [&record.id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((actual_revision, existing_payload)) = existing else {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "not_found",
            Some(expected_revision),
            None,
            false,
        )
        .into());
    };
    let actual_revision =
        u64::try_from(actual_revision).context("stored work item revision is negative")?;
    let payload_json = serde_json::to_string(record)?;
    if actual_revision == next_revision {
        if existing_payload == payload_json {
            return Ok(false);
        }
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "same_revision_payload_conflict",
            Some(expected_revision),
            Some(actual_revision),
            false,
        )
        .into());
    }
    if actual_revision != expected_revision {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "revision_conflict",
            Some(expected_revision),
            Some(actual_revision),
            true,
        )
        .into());
    }

    update_work_item_row_tx(tx, record, &payload_json, expected_revision)?;
    Ok(true)
}

fn update_work_item_row_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
    payload_json: &str,
    expected_revision: u64,
) -> Result<()> {
    let state = enum_string(&record.state)?;
    let plan_status = enum_string(&record.plan_status)?;
    let completed_at =
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at));
    let plan_artifact_path = record
        .plan_artifact
        .as_ref()
        .map(|artifact| artifact.path.display().to_string());
    let changed = tx.execute(
        "UPDATE work_items SET
            agent_id = ?1,
            state = ?2,
            objective = ?3,
            plan_status = ?4,
            revision = ?5,
            created_at = ?6,
            updated_at = ?7,
            completed_at = ?8,
            plan_artifact_path = ?9,
            last_turn_id = ?10,
            payload_json = ?11,
            blocked_by = ?12,
            recheck_at = ?13,
            recheck_consumed_at = ?14
         WHERE work_item_id = ?15 AND revision = ?16",
        params![
            record.agent_id,
            state,
            record.objective,
            plan_status,
            record.revision as i64,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            plan_artifact_path,
            record.turn_id,
            payload_json,
            record.blocked_by,
            record.recheck_at.map(timestamp),
            record.recheck_consumed_at.map(timestamp),
            record.id,
            expected_revision as i64,
        ],
    )?;
    if changed != 1 {
        return Err(RuntimeStateTransitionConflict::revision(
            &record.id,
            "revision_conflict",
            Some(expected_revision),
            None,
            true,
        )
        .into());
    }
    Ok(())
}

pub fn import_work_item_tx(
    tx: &Transaction<'_>,
    record: &WorkItemRecord,
) -> Result<WorkImportOutcome> {
    if record.state != WorkItemState::Completed {
        super::reclamation::ensure_work_admission_tx(tx, &record.agent_id)?;
    }
    let existing: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM work_items WHERE work_item_id=?1",
            [&record.id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(payload) = existing {
        let existing: WorkItemRecord = decode_work_item_payload(&payload)?;
        if existing == *record && work_import_projection_equal_tx(tx, record)? {
            return Ok(WorkImportOutcome::LegacyEqual);
        }
    }
    let payload_json = serde_json::to_string(record)?;
    let state = enum_string(&record.state)?;
    let plan_status = enum_string(&record.plan_status)?;
    let completed_at =
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at));
    let plan_artifact_path = record
        .plan_artifact
        .as_ref()
        .map(|artifact| artifact.path.display().to_string());
    let blocked_by = record.blocked_by.clone();
    let recheck_at = record.recheck_at.map(|t| timestamp(t));
    let recheck_consumed_at = record.recheck_consumed_at.map(|t| timestamp(t));
    let changed = tx.execute(
        "INSERT INTO work_items (
            work_item_id, agent_id, state, objective, plan_status,
            revision, current_focus, created_at, updated_at, completed_at,
            plan_artifact_path, last_turn_id, payload_json,
            blocked_by, recheck_at, recheck_consumed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(work_item_id) DO UPDATE SET
            agent_id = excluded.agent_id,
            state = excluded.state,
            objective = excluded.objective,
            plan_status = excluded.plan_status,
            revision = excluded.revision,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            plan_artifact_path = excluded.plan_artifact_path,
            last_turn_id = excluded.last_turn_id,
            payload_json = excluded.payload_json,
            blocked_by = excluded.blocked_by,
            recheck_at = excluded.recheck_at,
            recheck_consumed_at = excluded.recheck_consumed_at
         WHERE excluded.revision >= work_items.revision",
        params![
            record.id,
            record.agent_id,
            state,
            record.objective,
            plan_status,
            record.revision as i64,
            0_i64,
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            plan_artifact_path,
            record.turn_id,
            payload_json,
            blocked_by,
            recheck_at,
            recheck_consumed_at,
        ],
    )?;
    Ok(if changed == 1 {
        WorkImportOutcome::ActualWrite
    } else {
        WorkImportOutcome::Ignored
    })
}

fn work_import_projection_equal_tx(tx: &Transaction<'_>, record: &WorkItemRecord) -> Result<bool> {
    let actual: String = tx.query_row(
        "SELECT json_array(agent_id,state,objective,plan_status,revision,created_at,updated_at,completed_at,plan_artifact_path,last_turn_id,blocked_by,recheck_at,recheck_consumed_at) FROM work_items WHERE work_item_id=?1",
        [&record.id], |row| row.get(0),
    )?;
    let expected = serde_json::json!([
        record.agent_id,
        record.state,
        record.objective,
        record.plan_status,
        record.revision,
        timestamp(record.created_at),
        timestamp(record.updated_at),
        (record.state == WorkItemState::Completed).then(|| timestamp(record.updated_at)),
        record
            .plan_artifact
            .as_ref()
            .map(|artifact| artifact.path.display().to_string()),
        record.turn_id,
        record.blocked_by,
        record.recheck_at.map(timestamp),
        record.recheck_consumed_at.map(timestamp)
    ]);
    Ok(serde_json::from_str::<serde_json::Value>(&actual)? == expected)
}

pub fn decode_work_item_payload(payload: &str) -> Result<WorkItemRecord> {
    serde_json::from_str(payload).context("decoding work item payload from runtime db")
}
