//! Task row persistence and transition fences.

use anyhow::{Context, Result};
use holon_domain::{TaskRecord, TaskStatus};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::codec::{enum_string, timestamp};
use crate::reclamation::ensure_work_admission_tx;
use crate::RuntimeStateTransitionConflict;

pub const TASK_PAYLOAD_STRING_LIMIT: usize = 2048;
pub const TASK_PAYLOAD_ARRAY_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateTransitionOutcome {
    Applied,
    Idempotent,
}

#[derive(Clone, Copy)]
pub enum TaskRowWriteMode {
    Upsert,
    ExistingOriginalBinding,
    ExistingOriginalRunning,
}

pub fn upsert_task_tx(tx: &Transaction<'_>, record: &TaskRecord) -> Result<bool> {
    if is_active_task_status(&record.status) {
        ensure_work_admission_tx(tx, &record.agent_id)?;
        if let Some(child_id) = task_detail_string(&record.detail, "child_agent_id") {
            ensure_work_admission_tx(tx, &child_id)?;
        }
    }
    let existing = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&record.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_task_payload(&payload))
        .transpose()?;
    if let Some(existing) = existing.as_ref() {
        match task_transition(existing, record)? {
            StateTransitionOutcome::Applied => {}
            StateTransitionOutcome::Idempotent => return Ok(false),
        }
    }

    write_task_row_tx(tx, record)
}

pub fn write_task_row_tx(tx: &Transaction<'_>, record: &TaskRecord) -> Result<bool> {
    write_task_row_with_mode_tx(tx, record, TaskRowWriteMode::Upsert)
}

pub fn write_task_row_with_mode_tx(
    tx: &Transaction<'_>,
    record: &TaskRecord,
    mode: TaskRowWriteMode,
) -> Result<bool> {
    let kind = record.kind.as_str();
    let status = enum_string(&record.status)?;
    let status_phase = i64::from(task_status_phase(&record.status));
    let child_agent_id = task_detail_string(&record.detail, "child_agent_id");
    let parent_agent_id = child_agent_id.as_ref().map(|_| record.agent_id.clone());
    let input_target = task_detail_string(&record.detail, "input_target");
    let wait_policy = enum_string(&record.wait_policy())?;
    let output_path = task_detail_string(&record.detail, "output_path");
    let result_summary = task_detail_string(&record.detail, "output_summary")
        .map(|summary| truncate_task_payload_string(&summary));
    let exit_status = task_detail_i64(&record.detail, "exit_status");
    let terminal_reentry = i64::from(record.terminal_reentry());
    let completed_at =
        is_terminal_task_status(&record.status).then(|| timestamp(record.updated_at));
    let payload_json = serde_json::to_string(&slim_task_record_for_payload(record))?;
    let sql=match mode {TaskRowWriteMode::Upsert=>"INSERT INTO tasks (
            task_id, owner_agent_id, parent_agent_id, child_agent_id, kind, status,
            summary, input_target, wait_policy, output_path, result_summary,
            exit_status, terminal_reentry, revision, created_at, updated_at,
            completed_at, last_message_id, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT(task_id) DO UPDATE SET
            owner_agent_id = excluded.owner_agent_id,
            parent_agent_id = excluded.parent_agent_id,
            child_agent_id = excluded.child_agent_id,
            kind = excluded.kind,
            status = excluded.status,
            summary = excluded.summary,
            input_target = excluded.input_target,
            wait_policy = excluded.wait_policy,
            output_path = excluded.output_path,
            result_summary = excluded.result_summary,
            exit_status = excluded.exit_status,
            terminal_reentry = excluded.terminal_reentry,
            revision = excluded.revision,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            last_message_id = excluded.last_message_id,
            payload_json = excluded.payload_json
         WHERE excluded.revision >= tasks.revision
            AND ?20 >= CASE tasks.status
                WHEN 'queued' THEN 0
                WHEN 'running' THEN 1
                WHEN 'cancelling' THEN 2
                ELSE 3
            END",TaskRowWriteMode::ExistingOriginalBinding | TaskRowWriteMode::ExistingOriginalRunning=>"UPDATE tasks SET owner_agent_id=?2,parent_agent_id=?3,child_agent_id=?4,kind=?5,status=?6,
         summary=?7,input_target=?8,wait_policy=?9,output_path=?10,result_summary=?11,exit_status=?12,
         terminal_reentry=?13,revision=?14,created_at=?15,updated_at=?16,completed_at=?17,last_message_id=?18,payload_json=?19
         WHERE task_id=?1 AND ?14>=revision AND ?20>=CASE status
            WHEN 'queued' THEN 0 WHEN 'running' THEN 1 WHEN 'cancelling' THEN 2 ELSE 3 END"};
    let actual = tx.execute(
        sql,
        params![
            record.id,
            record.agent_id,
            parent_agent_id,
            child_agent_id,
            kind,
            status,
            record.summary,
            input_target,
            wait_policy,
            output_path,
            result_summary,
            exit_status,
            terminal_reentry,
            task_revision(record),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            completed_at,
            record.parent_message_id,
            payload_json,
            status_phase,
        ],
    )?;
    if matches!(
        mode,
        TaskRowWriteMode::ExistingOriginalBinding | TaskRowWriteMode::ExistingOriginalRunning
    ) {
        anyhow::ensure!(
            actual == 1,
            "existing child binding UPDATE did not affect its original Task"
        );
    }
    Ok(true)
}

pub fn task_status_phase(status: &TaskStatus) -> u8 {
    match status {
        TaskStatus::Queued => 0,
        TaskStatus::Running => 1,
        TaskStatus::Cancelling => 2,
        TaskStatus::Completed
        | TaskStatus::Failed
        | TaskStatus::Cancelled
        | TaskStatus::Interrupted => 3,
    }
}

pub fn task_transition(
    existing: &TaskRecord,
    incoming: &TaskRecord,
) -> Result<StateTransitionOutcome> {
    if existing == incoming {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    if let Some(holon_domain::TaskRecoverySpec::AgentMessageWait {
        message,
        target_agent_id,
        authority_class,
        delivery_id,
        after_delivery_rowid,
        reply_deadline_at,
        requester_incarnation,
        ..
    }) = existing.recovery.as_ref()
    {
        anyhow::ensure!(
            matches!(incoming.recovery.as_ref(),
                Some(holon_domain::TaskRecoverySpec::AgentMessageWait {
                    message: incoming_message,
                    target_agent_id: incoming_target,
                    authority_class: incoming_authority,
                    delivery_id: incoming_delivery,
                    after_delivery_rowid: incoming_boundary,
                    reply_deadline_at: incoming_deadline,
                    requester_incarnation: incoming_incarnation, ..
                }) if incoming_message == message
                    && incoming_target == target_agent_id
                    && incoming_authority == authority_class
                    && delivery_id.as_ref().is_none_or(|id| incoming_delivery.as_ref() == Some(id))
                    && after_delivery_rowid.is_none_or(|boundary| *incoming_boundary == Some(boundary))
                    && incoming_deadline == reply_deadline_at
                    && incoming_incarnation == requester_incarnation),
            "message_wait_binding_conflict: immutable request, receipt, deadline or requester incarnation"
        );
    }
    if existing
        .target()
        .is_some_and(|target| target.kind == holon_domain::TaskTargetKind::ChildAgent)
        && existing
            .detail
            .as_ref()
            .and_then(|detail| detail.get("source_message_id"))
            .is_some()
    {
        for key in [
            "source_message_id",
            "source_turn_id",
            "child_agent_id",
            "target",
        ] {
            anyhow::ensure!(
                existing.detail.as_ref().and_then(|detail| detail.get(key))
                    == incoming.detail.as_ref().and_then(|detail| detail.get(key)),
                "supervised_invocation_source_binding_conflict: {key}"
            );
        }
    }
    if existing.kind == holon_domain::TaskKind::AgentMessageWait
        && existing
            .detail
            .as_ref()
            .and_then(|detail| detail.get("request_delivery_id"))
            .is_some()
    {
        for key in [
            "target",
            "request_delivery_id",
            "message_wait_after_delivery_rowid",
        ] {
            anyhow::ensure!(
                existing.detail.as_ref().and_then(|detail| detail.get(key))
                    == incoming.detail.as_ref().and_then(|detail| detail.get(key)),
                "message_wait_binding_conflict: {key}"
            );
        }
    }
    if is_terminal_task_status(&existing.status) {
        if is_terminal_task_status(&incoming.status) && task_terminal_payload_eq(existing, incoming)
        {
            return Ok(StateTransitionOutcome::Idempotent);
        }
        return Err(RuntimeStateTransitionConflict::new(
            "task",
            &existing.id,
            enum_string(&existing.status)?,
            enum_string(&incoming.status)?,
        )
        .into());
    }
    if task_revision(incoming) < task_revision(existing)
        || task_status_phase(&incoming.status) < task_status_phase(&existing.status)
    {
        return Ok(StateTransitionOutcome::Idempotent);
    }
    Ok(StateTransitionOutcome::Applied)
}

fn task_terminal_payload_eq(existing: &TaskRecord, incoming: &TaskRecord) -> bool {
    let existing = canonical_task_terminal_payload(existing);
    let incoming = canonical_task_terminal_payload(incoming);
    existing.id == incoming.id
        && existing.agent_id == incoming.agent_id
        && existing.kind == incoming.kind
        && existing.status == incoming.status
        && existing.parent_message_id == incoming.parent_message_id
        && existing.work_item_id == incoming.work_item_id
        && existing.summary == incoming.summary
        && existing.detail == incoming.detail
        && existing.recovery == incoming.recovery
}

fn canonical_task_terminal_payload(record: &TaskRecord) -> TaskRecord {
    let mut canonical = record.clone();
    canonical.detail = canonical
        .detail
        .as_ref()
        .map(canonical_task_terminal_detail);
    canonical
}

fn canonical_task_terminal_detail(value: &serde_json::Value) -> serde_json::Value {
    // Terminal identity ignores derived observation fields even when storage retains them.
    match value {
        serde_json::Value::Object(map) => {
            let mut canonical = serde_json::Map::new();
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "initial_output"
                        | "output_summary"
                        | "output_preview"
                        | "terminal_snapshot_ready"
                ) {
                    continue;
                }
                canonical.insert(key.clone(), canonical_task_terminal_detail(value));
            }
            serde_json::Value::Object(canonical)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_task_terminal_detail).collect())
        }
        _ => value.clone(),
    }
}

pub fn decode_task_payload(payload: &str) -> Result<TaskRecord> {
    serde_json::from_str(payload).context("decoding task payload from runtime db")
}

pub fn slim_task_record_for_payload(record: &TaskRecord) -> TaskRecord {
    let mut slim = record.clone();
    slim.detail = slim.detail.as_ref().map(slim_task_detail_value);
    slim
}

pub fn slim_task_detail_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut slim = serde_json::Map::new();
            for (key, value) in map {
                if key == "initial_output" {
                    continue;
                }
                slim.insert(key.clone(), slim_task_detail_value(value));
            }
            serde_json::Value::Object(slim)
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .take(TASK_PAYLOAD_ARRAY_LIMIT)
                .map(slim_task_detail_value)
                .collect(),
        ),
        serde_json::Value::String(value) => {
            serde_json::Value::String(truncate_task_payload_string(value))
        }
        _ => value.clone(),
    }
}

pub fn truncate_task_payload_string(value: &str) -> String {
    value.chars().take(TASK_PAYLOAD_STRING_LIMIT).collect()
}

pub fn task_revision(record: &TaskRecord) -> i64 {
    record.updated_at.timestamp_millis()
}

pub fn task_detail_string(detail: &Option<serde_json::Value>, key: &str) -> Option<String> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

pub fn task_detail_i64(detail: &Option<serde_json::Value>, key: &str) -> Option<i64> {
    detail
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_i64())
}

pub fn is_active_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
    )
}

pub fn is_terminal_task_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}
