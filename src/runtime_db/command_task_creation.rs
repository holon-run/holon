//! Original creator authority. Ordinary Task detail is only a reference to it.
use crate::{
    domain::execution_protocol::TaskRejoinSource,
    runtime::resident_input_evidence::InputEvidenceScope,
    types::{TaskKind, TaskRecord, TaskStatus},
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

pub(super) const VERSION: i64 = 102;
pub(super) const SCHEMA: &str = include_str!("command_task_creation/schema.sql");

pub(in crate::runtime_db) fn has_result_source_column(
    connection: &rusqlite::Connection,
) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('task_result_settlements')
         WHERE name='command_creation_id')",
        [],
        |r| r.get(0),
    )?)
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct OriginalCommandTaskCreation {
    scope: InputEvidenceScope,
    original: TaskRecord,
    task: TaskRecord,
}

impl OriginalCommandTaskCreation {
    /// Only the original Command creator may call this before its first write.
    pub(crate) fn new(scope: InputEvidenceScope, original: TaskRecord) -> Result<Self> {
        ensure!(
            scope.agent_id == original.agent_id && scope.identity_incarnation > 0,
            "Command creation scope differs from original owner"
        );
        validate_original(&original)?;
        let mut task = original.clone();
        task.detail
            .as_mut()
            .and_then(serde_json::Value::as_object_mut)
            .context("Command creation detail is not an object")?
            .insert("command_creation_id".into(), serde_json::json!(original.id));
        Ok(Self {
            scope,
            original,
            task,
        })
    }

    pub(crate) fn task(&self) -> &TaskRecord {
        &self.task
    }

    pub(in crate::runtime_db) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }

    pub(in crate::runtime_db) fn original_bytes(&self) -> Result<usize> {
        struct Counter(usize);
        impl std::io::Write for Counter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .ok_or_else(|| std::io::Error::other("Command creation size overflow"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut counter = Counter(256);
        serde_json::to_writer(&mut counter, &self.original)?;
        Ok(counter.0)
    }

    pub(in crate::runtime_db) fn write_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        validate_original(&self.original)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1)
             OR EXISTS(SELECT 1 FROM command_task_creation_sources WHERE task_id=?1)",
            [&self.task.id],
            |r| r.get(0),
        )?;
        ensure!(
            !exists,
            "Command creation cannot adopt an existing Task or source"
        );
        let turnless: bool = tx.query_row(
            "SELECT COALESCE(json_type(payload_json,'$.current_turn_id'),'null')='null'
             FROM agent_states WHERE agent_id=?1",
            [&self.scope.agent_id],
            |r| r.get(0),
        )?;
        ensure!(
            turnless,
            "turnless Command creator cannot omit the current parent Turn"
        );
        let identity: (u64, String) = tx.query_row(
            "SELECT json_extract(payload_json,'$.incarnation'),status FROM agent_identities WHERE agent_id=?1",
            [&self.scope.agent_id], |r| Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            identity.0 == self.scope.identity_incarnation && identity.1 == "active",
            "Command creator identity is not the original active incarnation"
        );
        let original = serde_json::to_string(&self.original)?;
        let generation = generation(&self.original)?;
        tx.execute(
            "INSERT INTO command_task_creation_sources
            (command_creation_id,agent_id,identity_incarnation,task_id,rejoin_generation,
             original_task_json,original_digest)
            VALUES (?1,?2,?3,?1,?4,?5,?6)",
            params![
                self.task.id,
                self.scope.agent_id,
                self.scope.identity_incarnation,
                generation,
                original,
                digest(&original)
            ],
        )?;
        Ok(())
    }
}

fn generation(task: &TaskRecord) -> Result<u64> {
    let detail = task
        .detail
        .as_ref()
        .context("Command creator detail missing")?;
    ensure!(
        detail.get("rejoin_obligation_id").and_then(|v| v.as_str()) == Some(task.id.as_str()),
        "Command creator obligation differs from Task"
    );
    detail
        .get("rejoin_generation")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
        .context("Command creator generation missing")
}

fn validate_original(task: &TaskRecord) -> Result<()> {
    ensure!(
        task.kind == TaskKind::CommandTask
            && task.status == TaskStatus::Queued
            && !task.id.is_empty(),
        "Command source requires the original queued Command"
    );
    generation(task)?;
    let detail = task
        .detail
        .as_ref()
        .context("Command creator detail missing")?;
    ensure!(
        detail.get("parent_turn_id").is_none()
            && detail.get("command_creation_id").is_none()
            && detail.get("result_execution_turn_id").is_none(),
        "Command source requires original CreatorPartial, not a bound or result Task"
    );
    Ok(())
}

fn digest(body: &str) -> String {
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

/// Charge before decoding. The source is rechecked on every ordinary read.
pub(in crate::runtime_db) fn validate_source_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    task_id: &str,
    generation: u64,
    source: &TaskRejoinSource,
    work_item_id: Option<&str>,
    task: Option<&TaskRecord>,
    mut charge: impl FnMut(usize, usize) -> Result<()>,
) -> Result<()> {
    let Some(id) = source.command_creation_id() else {
        return Ok(());
    };
    ensure!(
        id == task_id,
        "Command creation reference differs from Task"
    );
    let bytes: Option<usize> = tx
        .query_row(
            "SELECT length(CAST(original_task_json AS BLOB))+length(original_digest)+256
         FROM command_task_creation_sources
         WHERE command_creation_id=?1 AND agent_id=?2 AND identity_incarnation=?3
           AND task_id=?4 AND rejoin_generation=?5",
            params![
                id,
                scope.agent_id,
                scope.identity_incarnation,
                task_id,
                generation
            ],
            |r| r.get(0),
        )
        .optional()?;
    charge(
        bytes.context("Command creation source is missing or from another scope/generation")?,
        1,
    )?;
    let (raw, expected): (String, String) = tx.query_row(
        "SELECT original_task_json,original_digest FROM command_task_creation_sources
         WHERE command_creation_id=?1 AND agent_id=?2 AND identity_incarnation=?3
           AND task_id=?4 AND rejoin_generation=?5",
        params![
            id,
            scope.agent_id,
            scope.identity_incarnation,
            task_id,
            generation
        ],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        digest(&raw) == expected,
        "Command creation source digest mismatch"
    );
    let original: TaskRecord = serde_json::from_str(&raw)?;
    validate_original(&original)?;
    ensure!(
        original.id == task_id
            && original.agent_id == scope.agent_id
            && self::generation(&original)? == generation
            && original.work_item_id.as_deref() == work_item_id,
        "Command creation source differs from original Task/result binding"
    );
    if let Some(task) = task {
        ensure!(
            task.kind == original.kind
                && task.created_at == original.created_at
                && task.agent_id == original.agent_id,
            "Command Task changed its immutable creation identity"
        );
    }
    Ok(())
}

pub(in crate::runtime_db) fn current_source_exists_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    task_id: &str,
    generation: u64,
    source: &TaskRejoinSource,
    work_item_id: Option<&str>,
) -> Result<bool> {
    let Some(id) = source.command_creation_id() else {
        return Ok(true);
    };
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM command_task_creation_sources c
         JOIN agent_identities i ON i.agent_id=c.agent_id
           AND json_extract(i.payload_json,'$.incarnation')=c.identity_incarnation
         WHERE c.command_creation_id=?1 AND c.task_id=?2 AND c.agent_id=?3
           AND c.rejoin_generation=?4
           AND json_extract(c.original_task_json,'$.id')=c.task_id
           AND json_extract(c.original_task_json,'$.agent_id')=c.agent_id
           AND json_extract(c.original_task_json,'$.kind')='command_task'
           AND json_extract(c.original_task_json,'$.status')='queued'
           AND json_extract(c.original_task_json,'$.work_item_id') IS ?5
           AND json_extract(c.original_task_json,'$.detail.rejoin_obligation_id')=c.task_id
           AND json_extract(c.original_task_json,'$.detail.rejoin_generation')=c.rejoin_generation
           AND json_type(c.original_task_json,'$.detail.parent_turn_id') IS NULL
           AND json_type(c.original_task_json,'$.detail.command_creation_id') IS NULL)",
        params![id, task_id, agent_id, generation, work_item_id],
        |r| r.get(0),
    )?)
}
