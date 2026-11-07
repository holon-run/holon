//! One original predicate selection, consumed in the same borrowed transaction.
//! This is a finite write footprint, not qualification or a commit receipt.
use super::*;
use anyhow::{ensure, Context};
use std::collections::BTreeSet;

pub(crate) const OWNER_SQL: &str =         "SELECT payload_json
         FROM task_result_settlements
         WHERE agent_id = ?1
           AND work_item_id IS ?2
           AND NOT EXISTS (
             SELECT 1 FROM messages
             WHERE messages.message_id = task_result_settlements.message_id
               AND json_extract(messages.payload_json, '$.metadata.task_kind') = 'agent_message_wait'
               AND json_extract(messages.payload_json, '$.metadata.task_status') = 'completed'
               AND json_extract(messages.payload_json, '$.metadata.task_detail.reply_content_source') = 'original_message'
               AND json_extract(messages.payload_json, '$.metadata.task_detail.message_id') IS NOT NULL
           )
           AND (
             state = 'persisted_pending'
             OR (
               state = 'caller_admitted'
               AND NOT EXISTS (
                 SELECT 1
                 FROM execution_protocol_attempts attempts
                 WHERE attempts.agent_id = task_result_settlements.agent_id
                   AND attempts.attempt_id = task_result_settlements.activation_id
                   AND attempts.lifecycle_state = 'open'
               )
             )
           )
         ORDER BY created_at ASC, result_identity ASC
         LIMIT ?3";
const SELECTED_OWNER_SQL: &str =         "SELECT payload_json
         FROM task_result_settlements
         WHERE agent_id = ?1
           AND (work_item_id IS ?2 OR result_identity = ?5)
           AND (?4 IS NULL OR result_identity IN (SELECT value FROM json_each(?4)))
           AND NOT EXISTS (
             SELECT 1 FROM messages
             WHERE messages.message_id = task_result_settlements.message_id
               AND json_extract(messages.payload_json, '$.metadata.task_kind') = 'agent_message_wait'
               AND json_extract(messages.payload_json, '$.metadata.task_status') = 'completed'
               AND json_extract(messages.payload_json, '$.metadata.task_detail.reply_content_source') = 'original_message'
               AND json_extract(messages.payload_json, '$.metadata.task_detail.message_id') IS NOT NULL
           )
           AND (
             state = 'persisted_pending'
             OR (
               state = 'caller_admitted'
               AND NOT EXISTS (
                 SELECT 1
                 FROM execution_protocol_attempts attempts
                 WHERE attempts.agent_id = task_result_settlements.agent_id
                   AND attempts.attempt_id = task_result_settlements.activation_id
                   AND attempts.lifecycle_state = 'open'
               )
             )
           )
         ORDER BY CASE WHEN result_identity = ?5 THEN 0 ELSE 1 END, created_at ASC, result_identity ASC
         LIMIT ?3";
pub(crate) const REPLY_SQL: &str = "SELECT settlements.payload_json FROM task_result_settlements settlements
                 JOIN messages ON messages.message_id = settlements.message_id
                 WHERE settlements.agent_id = ?1
                   AND (settlements.work_item_id IS ?2 OR settlements.task_id IN (
                     SELECT json_extract(wake.value, '$.task_id')
                     FROM wait_conditions AS waits, json_each(waits.wake_sources_json) AS wake
                     WHERE waits.agent_id = ?1 AND waits.work_item_id IS ?2
                       AND waits.status = 'resolved' AND waits.kind = 'task'
                       AND waits.trigger_message_id = ?3
                       AND json_extract(wake.value, '$.kind') = 'task_result'
                   ))
                   AND json_extract(messages.payload_json, '$.metadata.task_detail.message_id') = ?3
                   AND json_extract(messages.payload_json, '$.metadata.task_kind') = 'agent_message_wait'
                   AND json_extract(messages.payload_json, '$.metadata.task_status') = 'completed'
                   AND json_extract(messages.payload_json, '$.metadata.task_detail.reply_content_source') = 'original_message'
                   AND settlements.state != 'settled'
                 ORDER BY CASE settlements.state WHEN 'caller_admitted' THEN 0 ELSE 1 END,
                   CASE WHEN settlements.task_id IN (
                     SELECT json_extract(wake.value,'$.task_id')
                     FROM wait_conditions waits,json_each(waits.wake_sources_json) wake
                     WHERE waits.agent_id=?1 AND waits.work_item_id IS ?2
                       AND waits.status='resolved' AND waits.kind='task' AND waits.trigger_message_id=?3
                       AND json_extract(wake.value,'$.kind')='task_result'
                   ) THEN 0 ELSE 1 END,settlements.created_at,settlements.result_identity LIMIT 64";
const SELECTED_REPLY_SQL: &str = "SELECT settlements.payload_json FROM task_result_settlements settlements
                 JOIN messages ON messages.message_id = settlements.message_id
                 WHERE settlements.agent_id = ?1
                   AND (?4 IS NULL OR settlements.result_identity IN (SELECT value FROM json_each(?4)))
                   AND (settlements.work_item_id IS ?2 OR settlements.task_id IN (
                     SELECT json_extract(wake.value, '$.task_id')
                     FROM wait_conditions AS waits, json_each(waits.wake_sources_json) AS wake
                     WHERE waits.agent_id = ?1 AND waits.work_item_id IS ?2
                       AND waits.status = 'resolved' AND waits.kind = 'task'
                       AND waits.trigger_message_id = ?3
                       AND json_extract(wake.value, '$.kind') = 'task_result'
                   ) OR settlements.result_identity = ?5)
                   AND json_extract(messages.payload_json, '$.metadata.task_detail.message_id') = ?3
                   AND json_extract(messages.payload_json, '$.metadata.task_kind') = 'agent_message_wait'
                   AND json_extract(messages.payload_json, '$.metadata.task_status') = 'completed'
                   AND json_extract(messages.payload_json, '$.metadata.task_detail.reply_content_source') = 'original_message'
                   AND settlements.state != 'settled'
                 ORDER BY CASE WHEN settlements.result_identity = ?5 THEN 0 ELSE 1 END, CASE settlements.state WHEN 'caller_admitted' THEN 0 ELSE 1 END,
                   CASE WHEN settlements.task_id IN (
                     SELECT json_extract(wake.value,'$.task_id')
                     FROM wait_conditions waits,json_each(waits.wake_sources_json) wake
                     WHERE waits.agent_id=?1 AND waits.work_item_id IS ?2
                       AND waits.status='resolved' AND waits.kind='task' AND waits.trigger_message_id=?3
                       AND json_extract(wake.value,'$.kind')='task_result'
                   ) THEN 0 ELSE 1 END,settlements.created_at,settlements.result_identity LIMIT 64";
const ACTIVATION_SQL: &str = "SELECT payload_json
         FROM task_result_settlements
         WHERE agent_id = ?1 AND (
           (activation_id = ?2 AND state = 'caller_admitted')
           OR result_identity IN (
             SELECT pending.result_identity
             FROM task_result_settlements pending
             JOIN messages source ON source.message_id = pending.message_id
               AND source.agent_id = pending.agent_id
             WHERE ?3 AND pending.agent_id = ?1
               AND pending.state = 'persisted_pending'
               AND pending.recovery_paused_at IS NULL
               AND pending.recovery_epoch = 0 AND pending.recovery_attempts = 0
               AND pending.next_recheck_at > ?4
               AND json_extract(source.payload_json, '$.metadata.task_kind') = 'agent_message_wait'
               AND json_extract(source.payload_json, '$.metadata.task_status') = 'completed'
               AND json_extract(source.payload_json, '$.metadata.task_detail.reply_content_source') = 'original_message'
               AND EXISTS (
                 SELECT 1 FROM task_result_settlements admitted
                 WHERE admitted.agent_id = pending.agent_id
                   AND admitted.work_item_id IS pending.work_item_id
                   AND admitted.activation_id = ?2 AND admitted.state = 'caller_admitted'
                   AND json_extract(admitted.payload_json, '$.reply_projection.message_id') =
                     json_extract(source.payload_json, '$.metadata.task_detail.message_id')
               )
             ORDER BY pending.created_at, pending.result_identity
             LIMIT 64
           )
         )
         ORDER BY created_at ASC, result_identity ASC";
const WAKE_SQL: &str = "SELECT payload_json
                 FROM task_result_settlements
                 WHERE agent_id = ?1
                   AND work_item_id = ?2
                   AND state != 'settled'
                   AND recovery_paused_at IS NULL
                   AND recovery_epoch = 0 AND recovery_attempts = 0
                   AND next_recheck_at IS NOT NULL";

#[derive(Debug, thiserror::Error)]
#[error("TaskResult footprint ResourceExhausted: {records}/{record_limit} records, {bytes}/{byte_limit} bytes")]
pub(crate) struct TaskResultFootprintExhausted {
    pub records: usize,
    pub bytes: usize,
    pub record_limit: usize,
    pub byte_limit: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TaskResultWriteLimits {
    pub record_limit: usize,
    pub byte_limit: usize,
}
impl Default for TaskResultWriteLimits {
    fn default() -> Self {
        Self {
            record_limit: 65_536,
            byte_limit: 64 * 1024 * 1024,
        }
    }
}
impl TaskResultWriteLimits {
    fn check(self, count: usize, bytes: usize) -> Result<()> {
        ensure!(
            self.record_limit > 0 && self.byte_limit > 0,
            "zero TaskResult footprint limit"
        );
        if count > self.record_limit || bytes > self.byte_limit {
            return Err(TaskResultFootprintExhausted {
                records: count,
                bytes,
                record_limit: self.record_limit,
                byte_limit: self.byte_limit,
            }
            .into());
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub(crate) enum ResultAdmissionProjections<'a> {
    Snapshot(&'a BTreeMap<String, IdleInputBatchItem>),
    Message {
        message_id: &'a str,
        projection: &'a IdleInputBatchItem,
    },
}
impl ResultAdmissionProjections<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Snapshot(items) => items.len(),
            Self::Message { .. } => 1,
        }
    }
    fn snapshot(&self) -> Option<&BTreeMap<String, IdleInputBatchItem>> {
        match self {
            Self::Snapshot(items) => Some(items),
            _ => None,
        }
    }
    fn get(&self, id: &str) -> Option<&IdleInputBatchItem> {
        match self {
            Self::Snapshot(items) => items.get(id),
            Self::Message {
                message_id,
                projection,
            } if *message_id == id => Some(projection),
            _ => None,
        }
    }
    fn original_material_bytes_tx(&self, tx: &Transaction<'_>) -> Result<usize> {
        let original_bytes = |message_id: &str| -> Result<usize> {
            Ok(tx
                .query_row(
                    "SELECT length(CAST(payload_json AS BLOB)) FROM messages WHERE message_id = ?1",
                    [message_id],
                    |row| row.get::<_, usize>(0),
                )
                .optional()?
                .unwrap_or_default())
        };
        match self {
            Self::Snapshot(items) => items.keys().try_fold(0usize, |bytes, message_id| {
                bytes
                    .checked_add(original_bytes(message_id)?)
                    .context("Result projection original material overflow")
            }),
            Self::Message { message_id, .. } => original_bytes(message_id),
        }
    }
}
#[derive(Debug)]
struct SelectedResult {
    raw: String,
    record: TaskResultSettlementRecord,
}
#[derive(Debug)]
enum Operation {
    EnsurePending {
        task_id: String,
        task: Option<TaskRecord>,
        pending: Option<TaskResultSettlementRecord>,
        blocked: bool,
    },
    UpsertPending {
        task: TaskRecord,
        record: TaskResultSettlementRecord,
    },
    AdmitUnsettled {
        agent: String,
        activation: String,
        anchor: Option<String>,
        identities: Option<BTreeSet<String>>,
        now: DateTime<Utc>,
    },
    AdmitReply {
        agent: String,
        owner: Option<String>,
        activation: String,
        identities: Option<BTreeSet<String>>,
        now: DateTime<Utc>,
    },
    AdmitMessage {
        agent: String,
        activation: String,
        now: DateTime<Utc>,
    },
    SettleActivation {
        disposition: TaskResultSettlementDisposition,
        now: DateTime<Utc>,
    },
    SettleInvalidOnly {
        now: DateTime<Utc>,
    },
    SettleMessage {
        disposition: TaskResultSettlementDisposition,
        now: DateTime<Utc>,
    },
    Defer {
        reason: String,
        now: DateTime<Utc>,
        deadline: DateTime<Utc>,
    },
    MarkRecoveryDiagnostic {
        epoch: u64,
        now: DateTime<Utc>,
    },
    Wake {
        now: DateTime<Utc>,
    },
}
/// No Clone: selected rows and operation clocks belong to this original transaction.
/// Caller prepares the Task proof before execute; it must not mutate predicate
/// dependencies between preparation and execution. No await/other connection occurs.
pub(crate) struct PreparedTaskResultMutation<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    operation: Operation,
    selected: Vec<SelectedResult>,
    observed_message: Option<(String, Option<String>)>,
    candidates: BTreeSet<String>,
    tasks: BTreeSet<String>,
    limits: TaskResultWriteLimits,
    admission_projections: Option<ResultAdmissionProjections<'tx>>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum TaskResultWriteOutcome {
    Record(Option<TaskResultSettlementRecord>),
    Records(Vec<TaskResultSettlementRecord>),
    Changed(bool),
    Count(usize),
    Unit,
}
#[derive(Debug)]
pub(crate) struct TaskResultWriteAck {
    pub outcome: TaskResultWriteOutcome,
    /// Only IDs whose original INSERT/UPDATE returned affected == 1.
    /// This includes invalid-to-Settled rows absent from the legacy result value.
    pub actual_result_ids: BTreeSet<String>,
}
impl TaskResultWriteAck {
    #[cfg(test)]
    pub(crate) fn into_record(self) -> Result<Option<TaskResultSettlementRecord>> {
        match self.outcome {
            TaskResultWriteOutcome::Record(x) => Ok(x),
            _ => bail!("TaskResult outcome type mismatch"),
        }
    }
    #[cfg(test)]
    pub(crate) fn into_records(self) -> Result<Vec<TaskResultSettlementRecord>> {
        match self.outcome {
            TaskResultWriteOutcome::Records(x) => Ok(x),
            _ => bail!("TaskResult outcome type mismatch"),
        }
    }
    #[cfg(test)]
    pub(crate) fn into_changed(self) -> Result<bool> {
        match self.outcome {
            TaskResultWriteOutcome::Changed(x) => Ok(x),
            _ => bail!("TaskResult outcome type mismatch"),
        }
    }
    pub(crate) fn into_count(self) -> Result<usize> {
        match self.outcome {
            TaskResultWriteOutcome::Count(x) => Ok(x),
            _ => bail!("TaskResult outcome type mismatch"),
        }
    }
    #[cfg(test)]
    pub(crate) fn into_unit(self) -> Result<()> {
        match self.outcome {
            TaskResultWriteOutcome::Unit => Ok(()),
            _ => bail!("TaskResult outcome type mismatch"),
        }
    }
}
fn selected_tx<P: rusqlite::Params>(
    tx: &Transaction<'_>,
    sql: &str,
    params: P,
    limits: TaskResultWriteLimits,
) -> Result<Vec<SelectedResult>> {
    limits.check(0, 0)?;
    let mut stmt = tx.prepare(sql)?;
    let rows = stmt.query_map(params, |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for row in rows {
        let raw = row?;
        bytes = bytes
            .checked_add(raw.len())
            .context("TaskResult footprint bytes overflow")?;
        limits.check(out.len() + 1, bytes)?;
        let record = serde_json::from_str(&raw)?;
        out.push(SelectedResult { raw, record });
    }
    Ok(out)
}
fn single_tx(
    tx: &Transaction<'_>,
    message: &str,
    limits: TaskResultWriteLimits,
) -> Result<Vec<SelectedResult>> {
    selected_tx(
        tx,
        "SELECT payload_json FROM task_result_settlements WHERE message_id=?1",
        [message],
        limits,
    )
}
fn plan<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    operation: Operation,
    selected: Vec<SelectedResult>,
) -> PreparedTaskResultMutation<'tx, 'connection> {
    let candidates = selected
        .iter()
        .map(|x| x.record.result_identity.clone())
        .collect();
    // Invalid targets are predicate observations, never Task body/write roots.
    let tasks = if matches!(operation, Operation::SettleInvalidOnly { .. }) {
        BTreeSet::new()
    } else {
        selected.iter().map(|x| x.record.task_id.clone()).collect()
    };
    PreparedTaskResultMutation {
        tx,
        operation,
        selected,
        observed_message: None,
        candidates,
        tasks,
        limits: TaskResultWriteLimits::default(),
        admission_projections: None,
    }
}
fn single_plan<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    operation: Operation,
    message: &str,
    limits: TaskResultWriteLimits,
) -> Result<PreparedTaskResultMutation<'tx, 'connection>> {
    let selected = single_tx(tx, message, limits)?;
    let observed = selected.first().map(|x| x.raw.clone());
    let mut plan = plan(tx, operation, selected);
    plan.observed_message = Some((message.to_owned(), observed));
    Ok(plan)
}
#[cfg(test)]
pub(crate) fn prepare_admit_message_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    message: &str,
    activation: &str,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<PreparedTaskResultMutation<'tx, 'connection>> {
    select_admit_message_tx(tx, agent, message, activation, now, limits)?.materialize(limits)
}
pub(crate) fn prepare_settle_activation_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    activation: &str,
    disposition: TaskResultSettlementDisposition,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<PreparedTaskResultMutation<'tx, 'connection>> {
    select_settle_activation_tx(tx, agent, activation, disposition, now, limits)?
        .materialize(limits)
}
#[cfg(test)]
pub(crate) fn prepare_settle_owner_unavailable_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    message: &str,
    disposition: TaskResultSettlementDisposition,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<PreparedTaskResultMutation<'tx, 'connection>> {
    select_settle_owner_unavailable_tx(tx, message, disposition, now, limits)?.materialize(limits)
}
#[cfg(test)]
pub(crate) fn prepare_mark_deferred_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    message: &str,
    reason: &str,
    now: DateTime<Utc>,
    deadline: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<PreparedTaskResultMutation<'tx, 'connection>> {
    select_mark_deferred_tx(tx, message, reason, now, deadline, limits)?.materialize(limits)
}
fn task_tx(
    tx: &Transaction<'_>,
    id: &str,
    limits: TaskResultWriteLimits,
) -> Result<Option<TaskRecord>> {
    let bytes: Option<usize> = tx
        .query_row(
            "SELECT length(CAST(payload_json AS BLOB)) FROM tasks WHERE task_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    limits.check(usize::from(bytes.is_some()), bytes.unwrap_or_default())?;
    tx.query_row(
        "SELECT payload_json FROM tasks WHERE task_id=?1",
        [id],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|raw| super::super::repositories::decode_task_payload(&raw))
    .transpose()
}
pub(crate) fn prepare_ensure_pending_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    task_id: &str,
    message: &MessageEnvelope,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<PreparedTaskResultMutation<'tx, 'connection>> {
    let material = ensure_pending_material_estimate_tx(tx, task_id, &message.id)?;
    limits.check(
        material.records,
        material.source_material_bytes + material.metadata_estimate_bytes,
    )?;
    let task = task_tx(tx, task_id, limits)?;
    let pending = task
        .as_ref()
        .map(|x| TaskResultSettlementRecord::pending(x, message, now))
        .transpose()?
        .flatten();
    let selected = if let Some(record) = &pending {
        selected_tx(tx,"SELECT payload_json FROM task_result_settlements WHERE task_id=?1 AND rejoin_generation=?2 ORDER BY created_at ASC,result_identity ASC LIMIT 1",params![record.task_id,record.rejoin_generation],limits)?
    } else {
        Vec::new()
    };
    let blocked = pending.as_ref().is_some_and(|record| {
        selected
            .first()
            .is_some_and(|old| old.record.message_id != record.message_id)
    });
    let mut prepared = plan(
        tx,
        Operation::EnsurePending {
            task_id: task_id.to_owned(),
            task: task.clone(),
            pending: pending.clone(),
            blocked,
        },
        selected,
    );
    prepared.limits = limits;
    prepared.candidates.clear();
    if let Some(record) = &pending {
        if !blocked {
            prepared.candidates.insert(record.result_identity.clone());
        }
    }
    if let Some(task) = task {
        prepared.tasks.insert(task.id);
    }
    if !blocked && pending.is_some() {
        prepared.observed_message = Some((
            message.id.clone(),
            single_tx(tx, &message.id, limits)?
                .first()
                .map(|x| x.raw.clone()),
        ));
    }
    Ok(prepared)
}
/// The Task command may be new: no pre-SQL canonical Task is required.
/// Execution occurs after its original upsert and verifies the actual slim Task.
pub(crate) fn prepare_upsert_pending_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    task: &TaskRecord,
    record: &TaskResultSettlementRecord,
    limits: TaskResultWriteLimits,
) -> Result<PreparedTaskResultMutation<'tx, 'connection>> {
    let material = upsert_pending_material_estimate_tx(tx, task, record)?;
    limits.check(
        material.records,
        material.source_material_bytes + material.metadata_estimate_bytes,
    )?;
    ensure!(
        task.id == record.task_id && task.agent_id == record.agent_id,
        "TaskResult pending command Task mismatch"
    );
    let mut prepared = single_plan(
        tx,
        Operation::UpsertPending {
            task: super::super::repositories::slim_task_record_for_payload(task),
            record: record.clone(),
        },
        &record.message_id,
        limits,
    )?;
    prepared.limits = limits;
    prepared.candidates.insert(record.result_identity.clone());
    prepared.tasks.insert(task.id.clone());
    Ok(prepared)
}
/// Closed UpsertPending operation only; cannot be constructed by another family.
pub(crate) struct PreparedUpsertPendingView<'a, 'connection> {
    tx: &'a Transaction<'connection>,
    task: &'a TaskRecord,
    record: &'a TaskResultSettlementRecord,
}
impl<'connection> PreparedUpsertPendingView<'_, 'connection> {
    pub(crate) fn transaction(&self) -> &Transaction<'connection> {
        self.tx
    }
    pub(crate) fn task(&self) -> &TaskRecord {
        self.task
    }
    pub(crate) fn record(&self) -> &TaskResultSettlementRecord {
        self.record
    }
}

impl PreparedTaskResultMutation<'_, '_> {
    /// Closed admit and Invalid-only cleanup operations inspect the original predicate.
    pub(in crate::runtime_db) fn original_predicate_rows_tx(
        &self,
        tx: &Transaction<'_>,
    ) -> Result<impl Iterator<Item = (&TaskResultSettlementRecord, &str)>> {
        ensure!(
            std::ptr::eq(tx, self.tx),
            "TaskResult predicate belongs to another transaction"
        );
        ensure!(
            matches!(
                self.operation,
                Operation::AdmitUnsettled { .. }
                    | Operation::AdmitReply { .. }
                    | Operation::AdmitMessage { .. }
                    | Operation::SettleInvalidOnly { .. }
            ),
            "original predicate proof requires a closed predicate operation"
        );
        Ok(self
            .selected
            .iter()
            .map(|row| (&row.record, row.raw.as_str())))
    }

    pub(crate) fn execute_with_original_predicate(
        self,
        proof: super::super::task_snapshot::OriginalResultPredicateProof<'_, '_>,
    ) -> Result<(
        TaskResultWriteAck,
        super::super::task_snapshot::OriginalResultPredicateRetention,
    )> {
        proof.validate_for_tx(self.tx, self.original_predicate_rows_tx(self.tx)?)?;
        let ack = self.execute_inner(None, Some(&proof))?;
        Ok((ack, proof.into_retention()))
    }

    pub(crate) fn upsert_pending_view(&self) -> Result<PreparedUpsertPendingView<'_, '_>> {
        let Operation::UpsertPending { task, record } = &self.operation else {
            bail!("original cleanup requires closed UpsertPending operation");
        };
        Ok(PreparedUpsertPendingView {
            tx: self.tx,
            task,
            record,
        })
    }

    pub(crate) fn candidate_result_ids(&self) -> &BTreeSet<String> {
        &self.candidates
    }
    pub(crate) fn dependency_task_ids(&self) -> &BTreeSet<String> {
        &self.tasks
    }
    pub(crate) fn execute_original_task_cleanup(
        self,
        proof: &super::super::task_snapshot::OriginalTaskResultCleanupProof<'_, '_, '_>,
    ) -> Result<TaskResultWriteAck> {
        // Reject every non-UpsertPending family before any original business SQL.
        self.upsert_pending_view()?;
        self.execute_inner(Some(proof), None)
    }
    pub(crate) fn execute(self) -> Result<TaskResultWriteAck> {
        self.execute_inner(None, None)
    }
    fn execute_inner(
        self,
        cleanup: Option<&super::super::task_snapshot::OriginalTaskResultCleanupProof<'_, '_, '_>>,
        predicate: Option<&super::super::task_snapshot::OriginalResultPredicateProof<'_, '_>>,
    ) -> Result<TaskResultWriteAck> {
        ensure!(
            !matches!(self.operation, Operation::SettleInvalidOnly { .. }) || predicate.is_some(),
            "Invalid-only cleanup requires its original transaction predicate proof"
        );
        for selected in &self.selected {
            let current: Option<String> = self
                .tx
                .query_row(
                    "SELECT payload_json FROM task_result_settlements WHERE result_identity=?1",
                    [&selected.record.result_identity],
                    |r| r.get(0),
                )
                .optional()?;
            ensure!(
                current.as_deref() == Some(selected.raw.as_str()),
                "TaskResult prepared selection changed in original transaction"
            );
        }
        if let Some((message, expected)) = &self.observed_message {
            let current: Option<String> = self
                .tx
                .query_row(
                    "SELECT payload_json FROM task_result_settlements WHERE message_id=?1",
                    [message],
                    |r| r.get(0),
                )
                .optional()?;
            ensure!(
                &current == expected,
                "TaskResult message address changed after preparation"
            );
        }
        let tx = self.tx;
        let mut actual = BTreeSet::new();
        let outcome = match self.operation {
            Operation::EnsurePending {
                task_id,
                task,
                pending,
                blocked,
            } => {
                ensure!(
                    task_tx(tx, &task_id, self.limits)? == task,
                    "TaskResult ensurePending actual Task/fence changed after preparation"
                );
                if blocked {
                    TaskResultWriteOutcome::Record(None)
                } else if let Some(record) = pending {
                    if upsert_pending_tx(tx, &record)? {
                        actual.insert(record.result_identity.clone());
                    }
                    TaskResultWriteOutcome::Record(latest_for_message_tx(tx, &record.message_id)?)
                } else {
                    TaskResultWriteOutcome::Record(None)
                }
            }
            Operation::UpsertPending { task, record } => {
                let actual_task = task_tx(tx, &task.id, self.limits)?
                    .context("TaskResult pending Task was not written by original compound")?;
                ensure!(
                    actual_task == task,
                    "TaskResult pending actual Task differs from frozen command"
                );
                ensure!(
                    record_matches_durable_task_tx(tx, &record)?,
                    "TaskResult pending original Task/fence mismatch"
                );
                let changed = match cleanup {
                    Some(proof) => upsert_pending_original_cleanup_tx(tx, &record, proof)?,
                    None => upsert_pending_tx(tx, &record)?,
                };
                if changed {
                    actual.insert(record.result_identity);
                }
                TaskResultWriteOutcome::Changed(changed)
            }
            Operation::AdmitUnsettled {
                agent,
                activation,
                anchor,
                identities,
                now,
            } => {
                let (already, anchor_admitted): (usize, bool) = tx.query_row(
                    "SELECT COUNT(*),COALESCE(MAX(message_id IS ?3),0)
                     FROM task_result_settlements
                     WHERE agent_id=?1 AND activation_id=?2 AND state='caller_admitted'",
                    params![agent, activation, anchor],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let recovering_anchor = self
                    .selected
                    .iter()
                    .find(|row| {
                        Some(row.record.message_id.as_str()) == anchor.as_deref()
                            && identities
                                .as_ref()
                                .is_none_or(|ids| ids.contains(&row.record.result_identity))
                    })
                    .map(|row| {
                        interrupted_predecessor_owns_record_tx(
                            tx,
                            &row.record,
                            &activation,
                            &row.record.message_id,
                        )
                    })
                    .transpose()?
                    .unwrap_or(false);
                let mut remaining = TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT.saturating_sub(
                    already
                        + usize::from(anchor.is_some() && !anchor_admitted && !recovering_anchor),
                );
                let mut admitted = Vec::with_capacity(self.selected.len());
                for selected in self.selected {
                    let mut record = selected.record;
                    if record.created_at > now
                        || (record.state == TaskResultSettlementState::CallerAdmitted
                            && record.activation_id.as_deref() == Some(activation.as_str()))
                    {
                        continue;
                    }
                    if !match &predicate {
                        Some(proof) => proof.matches(&record)?,
                        None => record_matches_durable_task_tx(tx, &record)?,
                    } {
                        if settle_invalid_or_stale_tx(tx, &mut record, now)? {
                            actual.insert(record.result_identity.clone());
                        }
                        continue;
                    }
                    if (anchor.as_deref() == Some(record.message_id.as_str()) && !recovering_anchor)
                        || identities
                            .as_ref()
                            .is_some_and(|ids| !ids.contains(&record.result_identity))
                    {
                        continue;
                    }
                    if remaining == 0 {
                        break;
                    }
                    if !charge_selected_recovery_tx(tx, &mut record, &activation, now)? {
                        continue;
                    }
                    set_admission_projections_tx(
                        tx,
                        &mut record,
                        self.admission_projections
                            .as_ref()
                            .and_then(ResultAdmissionProjections::snapshot),
                    )?;
                    record.admitted_at.get_or_insert(now);
                    admit(&mut record, &activation, now);
                    write_ack(tx, &record, false, &mut actual)?;
                    admitted.push(record);
                    remaining -= 1;
                }
                TaskResultWriteOutcome::Records(admitted)
            }
            Operation::AdmitReply {
                agent,
                owner,
                activation,
                identities,
                now,
            } => {
                let snapshot = identities.is_some();
                let (valid, anchor_reserved) =
                    reply_admission_policy_tx(tx, &agent, owner.as_deref(), &activation)?;
                let already: usize = tx.query_row(
                    "SELECT COUNT(*) FROM task_result_settlements
                     WHERE agent_id=?1 AND activation_id=?2 AND state='caller_admitted'",
                    params![agent, activation],
                    |row| row.get(0),
                )?;
                let mut remaining = TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT
                    .saturating_sub(already + usize::from(anchor_reserved));
                let mut admitted = Vec::new();
                for selected in self.selected {
                    let mut record = selected.record;
                    if (snapshot && !valid)
                        || record.created_at > now
                        || identities
                            .as_ref()
                            .is_some_and(|ids| !ids.contains(&record.result_identity))
                        || (record.state == TaskResultSettlementState::CallerAdmitted
                            && record.activation_id.as_deref() == Some(activation.as_str()))
                    {
                        continue;
                    }
                    if snapshot && remaining == 0 {
                        break;
                    }
                    if !match &predicate {
                        Some(proof) => proof.matches(&record)?,
                        None => record_matches_durable_task_tx(tx, &record)?,
                    } {
                        continue;
                    }
                    if record.state == TaskResultSettlementState::CallerAdmitted
                        && record.activation_id.as_deref() != Some(activation.as_str())
                    {
                        if snapshot {
                            let interrupted =
                                interrupted_predecessor_matches_tx(tx, &record, &activation, None)?;
                            let recovery_wake = if interrupted {
                                false
                            } else if let Some(current) =
                                reply_admission_attempt_tx(tx, &agent, &activation)?
                            {
                                obligation::recovery_wake_admits_tx(
                                    tx,
                                    &record,
                                    &current,
                                    owner.as_deref(),
                                )?
                            } else {
                                false
                            };
                            if !interrupted && !recovery_wake {
                                continue;
                            }
                        } else if let Some(id) = &record.activation_id {
                            let open=tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_protocol_attempts WHERE agent_id = ?1 AND attempt_id = ?2 AND lifecycle_state = 'open')",params![agent,id],|r|r.get::<_,bool>(0))?;
                            if open {
                                continue;
                            }
                        }
                    }
                    if !charge_selected_recovery_tx(tx, &mut record, &activation, now)? {
                        continue;
                    }
                    set_admission_projections_tx(
                        tx,
                        &mut record,
                        self.admission_projections
                            .as_ref()
                            .and_then(ResultAdmissionProjections::snapshot),
                    )?;
                    record.admitted_at = Some(now);
                    admit(&mut record, &activation, now);
                    write_ack(tx, &record, false, &mut actual)?;
                    admitted.push(record);
                    remaining = remaining.saturating_sub(1);
                }
                if snapshot {
                    TaskResultWriteOutcome::Records(admitted)
                } else {
                    TaskResultWriteOutcome::Unit
                }
            }
            Operation::AdmitMessage {
                agent,
                activation,
                now,
            } => {
                if let Some(selected) = self.selected.into_iter().next() {
                    let mut record = selected.record;
                    ensure!(
                        record.agent_id == agent,
                        "task result settlement agent mismatch for message {}",
                        record.message_id
                    );
                    if !match &predicate {
                        Some(proof) => proof.matches(&record)?,
                        None => record_matches_durable_task_tx(tx, &record)?,
                    } {
                        if settle_invalid_or_stale_tx(tx, &mut record, now)? {
                            actual.insert(record.result_identity.clone());
                        }
                        TaskResultWriteOutcome::Record(None)
                    } else {
                        match record.state {
                            TaskResultSettlementState::PersistedPending => {
                                if let Some(projection) = self
                                    .admission_projections
                                    .as_ref()
                                    .and_then(|items| items.get(&record.message_id))
                                {
                                    validate_projection_tx(
                                        tx,
                                        &record.message_id,
                                        &agent,
                                        projection,
                                    )?;
                                    record.source_projection = Some(projection.clone());
                                }
                                record.admitted_at = Some(now);
                                admit(&mut record, &activation, now);
                                write_ack(tx, &record, false, &mut actual)?;
                                TaskResultWriteOutcome::Record(Some(record))
                            }
                            TaskResultSettlementState::CallerAdmitted
                                if record.activation_id.as_deref() == Some(activation.as_str()) =>
                            {
                                TaskResultWriteOutcome::Record(Some(record))
                            }
                            TaskResultSettlementState::CallerAdmitted
                                if interrupted_predecessor_owns_record_tx(
                                    tx,
                                    &record,
                                    &activation,
                                    &record.message_id,
                                )? =>
                            {
                                record.activation_id = Some(activation);
                                record.updated_at = now;
                                record.deferred_reason = None;
                                record.deferred_at = None;
                                write_ack(tx, &record, false, &mut actual)?;
                                TaskResultWriteOutcome::Record(Some(record))
                            }
                            _ => TaskResultWriteOutcome::Record(None),
                        }
                    }
                } else {
                    TaskResultWriteOutcome::Record(None)
                }
            }
            Operation::SettleInvalidOnly { now } => {
                let proof = predicate.context("Invalid-only cleanup omitted original predicate")?;
                if let Some(selected) = self.selected.into_iter().next() {
                    let mut record = selected.record;
                    let changed = record.state != TaskResultSettlementState::Settled
                        && !proof.matches(&record)?
                        && settle_invalid_or_stale_tx(tx, &mut record, now)?;
                    if changed {
                        actual.insert(record.result_identity);
                    }
                    TaskResultWriteOutcome::Changed(changed)
                } else {
                    TaskResultWriteOutcome::Changed(false)
                }
            }
            Operation::SettleActivation { disposition, now } => {
                let mut count = 0;
                for selected in self.selected {
                    let mut record = selected.record;
                    if record.state == TaskResultSettlementState::CallerAdmitted {
                        settle(&mut record, disposition, now);
                        write_ack(tx, &record, false, &mut actual)?;
                        count += 1;
                    } else {
                        ensure!(
                            disposition == TaskResultSettlementDisposition::ModelDelivered
                                && record.state == TaskResultSettlementState::PersistedPending,
                            "batch progress requires a pending shared-reply remainder"
                        );
                        // Progress only advances a probe; the next batch still
                        // rechecks every original source before admission.
                        record.next_recheck_at = Some(now);
                        record.updated_at = now;
                        write_ack(tx, &record, true, &mut actual)?;
                    }
                }
                TaskResultWriteOutcome::Count(count)
            }
            Operation::SettleMessage { disposition, now } => {
                if let Some(selected) = self.selected.into_iter().next() {
                    let mut record = selected.record;
                    if record.state == TaskResultSettlementState::Settled {
                        TaskResultWriteOutcome::Changed(false)
                    } else {
                        settle(&mut record, disposition, now);
                        record.next_recheck_at = None;
                        write_ack(tx, &record, false, &mut actual)?;
                        TaskResultWriteOutcome::Changed(true)
                    }
                } else {
                    TaskResultWriteOutcome::Changed(false)
                }
            }
            Operation::Defer {
                reason,
                now,
                deadline,
            } => {
                if let Some(selected) = self.selected.into_iter().next() {
                    let mut record = selected.record;
                    if record.state != TaskResultSettlementState::Settled
                        && record.recovery_paused_at.is_none()
                    {
                        record.deferred_reason = Some(reason);
                        record.deferred_at = Some(now);
                        record.next_recheck_at =
                            Some(record.next_recheck_at.map_or(deadline, |old| {
                                if record.recovery_attempts > 0 {
                                    old.max(deadline)
                                } else {
                                    deadline
                                }
                            }));
                        record.updated_at = now;
                        write_ack(tx, &record, true, &mut actual)?;
                    }
                    TaskResultWriteOutcome::Record(Some(record))
                } else {
                    TaskResultWriteOutcome::Record(None)
                }
            }
            Operation::MarkRecoveryDiagnostic { epoch, now } => {
                if let Some(selected) = self.selected.into_iter().next() {
                    let mut record = selected.record;
                    if record.recovery_epoch == epoch
                        && record.recovery_paused_at.is_some()
                        && record.recovery_diagnostic_at.is_none()
                    {
                        record.recovery_diagnostic_at = Some(now);
                        record.updated_at = now;
                        write_ack(tx, &record, false, &mut actual)?;
                    }
                }
                TaskResultWriteOutcome::Unit
            }
            Operation::Wake { now } => {
                let count = self.selected.len();
                for selected in self.selected {
                    let mut record = selected.record;
                    record.next_recheck_at = Some(now);
                    record.updated_at = now;
                    write_ack(tx, &record, true, &mut actual)?;
                }
                TaskResultWriteOutcome::Count(count)
            }
        };
        ensure!(
            actual.is_subset(&self.candidates),
            "TaskResult actual write exceeded original selection"
        );
        Ok(TaskResultWriteAck {
            outcome,
            actual_result_ids: actual,
        })
    }
}
fn admit(record: &mut TaskResultSettlementRecord, activation: &str, now: DateTime<Utc>) {
    record.state = TaskResultSettlementState::CallerAdmitted;
    record.activation_id = Some(activation.into());
    record.updated_at = now;
    record.deferred_reason = None;
    record.deferred_at = None;
}
fn settle(
    record: &mut TaskResultSettlementRecord,
    disposition: TaskResultSettlementDisposition,
    now: DateTime<Utc>,
) {
    record.state = TaskResultSettlementState::Settled;
    record.disposition = Some(disposition);
    record.settled_at = Some(now);
    record.updated_at = now;
    // Activation settlement preserves its original deadline; the owner-closed
    // and invalid/stale operations explicitly clear theirs.
}

fn write_ack(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    deferred: bool,
    actual: &mut BTreeSet<String>,
) -> Result<()> {
    let changed = if deferred {
        defer_update_tx(tx, record)?
    } else {
        update_tx(tx, record)?
    };
    if changed {
        actual.insert(record.result_identity.clone());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TaskResultMaterialEstimate {
    pub records: usize,
    /// Source payload + complete projection lengths, before Rust decode.
    pub source_material_bytes: usize,
    /// Conservative simultaneous ownership allowance: raw/typed/selected,
    /// Task proof before, actual readback, final loader/delta and legacy outcome.
    pub payload_bytes: usize,
    pub metadata_estimate_bytes: usize,
}
struct ResultAddress {
    id: String,
    task: String,
    bytes: usize,
}
/// Only finite normalized addresses and SQL lengths; no Result/Task body decode.
/// Keep the caller's Candidate guard alive through materialize/proof/execute/finalizers.
pub(crate) struct TaskResultSelection<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    operation: Operation,
    addresses: Vec<ResultAddress>,
    message: Option<String>,
    material: TaskResultMaterialEstimate,
    admission_projections: Option<ResultAdmissionProjections<'tx>>,
}
impl<'tx, 'connection> TaskResultSelection<'tx, 'connection> {
    pub(crate) fn with_admission_projections(
        mut self,
        projections: Option<ResultAdmissionProjections<'tx>>,
        limits: TaskResultWriteLimits,
    ) -> Result<Self> {
        if let Some(projections) = &projections {
            let bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
                projections,
            )?)?;
            self.material.metadata_estimate_bytes = self
                .material
                .metadata_estimate_bytes
                .checked_add(
                    bytes
                        .checked_mul(4)
                        .context("Result projections material overflow")?,
                )
                .context("Result projections material overflow")?;
            self.material.records = self
                .material
                .records
                .checked_add(projections.len())
                .context("Result projections records overflow")?;
            limits.check(
                self.material.records,
                self.material.source_material_bytes + self.material.metadata_estimate_bytes,
            )?;
            // Reserve immutable originals before validation decodes or renders them.
            let original_bytes = projections.original_material_bytes_tx(self.tx)?;
            self.material.source_material_bytes = self
                .material
                .source_material_bytes
                .checked_add(original_bytes)
                .context("Result projection original material overflow")?;
            self.material.payload_bytes = self
                .material
                .payload_bytes
                .checked_add(
                    bytes
                        .checked_add(original_bytes)
                        .context("Result projections material overflow")?
                        .checked_mul(12)
                        .context("Result projections material overflow")?,
                )
                .context("Result projections material overflow")?;
            limits.check(
                self.material.records,
                self.material
                    .source_material_bytes
                    .checked_add(self.material.metadata_estimate_bytes)
                    .context("Result projections material overflow")?,
            )?;
        }
        self.admission_projections = projections;
        Ok(self)
    }
    pub(crate) fn material_estimate(&self) -> TaskResultMaterialEstimate {
        self.material
    }
    pub(crate) fn materialize(
        self,
        limits: TaskResultWriteLimits,
    ) -> Result<PreparedTaskResultMutation<'tx, 'connection>> {
        limits.check(
            self.material.records,
            self.material.source_material_bytes + self.material.metadata_estimate_bytes,
        )?;
        let mut selected = Vec::with_capacity(self.addresses.len());
        for address in self.addresses {
            let raw: String = self.tx.query_row(
                "SELECT payload_json FROM task_result_settlements WHERE result_identity=?1",
                [&address.id],
                |r| r.get(0),
            )?;
            ensure!(
                raw.len() <= address.bytes,
                "TaskResult selected material grew before decode"
            );
            let record: TaskResultSettlementRecord = serde_json::from_str(&raw)?;
            ensure!(
                record.result_identity == address.id && record.task_id == address.task,
                "TaskResult selected normalized address mismatch"
            );
            selected.push(SelectedResult { raw, record });
        }
        let observed = self.message.map(|id| {
            let raw = selected.first().map(|x| x.raw.clone());
            (id, raw)
        });
        let mut prepared = plan(self.tx, self.operation, selected);
        prepared.observed_message = observed;
        prepared.limits = limits;
        prepared.admission_projections = self.admission_projections;
        Ok(prepared)
    }
}
fn inventory<'tx, 'connection, P: rusqlite::Params>(
    tx: &'tx Transaction<'connection>,
    operation: Operation,
    sql: &str,
    params: P,
    message: Option<&str>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    let prefix = if sql.starts_with("SELECT settlements.payload_json") {
        "settlements."
    } else {
        ""
    };
    let projection=format!("{prefix}result_identity,{prefix}agent_id,{prefix}task_id,{prefix}message_id,{prefix}work_item_id,{prefix}rejoin_generation,{prefix}parent_turn_id,{prefix}state,{prefix}activation_id,{prefix}disposition,{prefix}created_at,{prefix}updated_at,{prefix}admitted_at,{prefix}settled_at,{prefix}deferred_reason,{prefix}deferred_at,{prefix}next_recheck_at");
    let projection = if crate::runtime_db::command_task_creation::has_result_source_column(tx)? {
        format!("{projection},{prefix}command_creation_id")
    } else {
        projection
    };
    let replacement=format!("SELECT {prefix}result_identity,{prefix}task_id,length(CAST({prefix}payload_json AS BLOB))+length(CAST(json_array({projection}) AS BLOB))");
    let from = if prefix.is_empty() {
        "SELECT payload_json"
    } else {
        "SELECT settlements.payload_json"
    };
    let sql = sql.replacen(from, &replacement, 1);
    let mut statement = tx.prepare(&sql)?;
    let mut addresses = Vec::new();
    let mut tasks = BTreeSet::new();
    let mut bytes = match &operation {
        Operation::AdmitUnsettled {
            agent,
            activation,
            anchor,
            identities,
            ..
        } => {
            agent.len()
                + activation.len()
                + anchor.as_ref().map_or(0, String::len)
                + identities
                    .as_ref()
                    .map_or(0, |ids| ids.iter().map(|id| id.len() + 128).sum::<usize>())
        }
        Operation::AdmitReply {
            agent,
            activation,
            owner,
            identities,
            ..
        } => {
            agent.len()
                + activation.len()
                + owner.as_ref().map_or(0, String::len)
                + identities
                    .as_ref()
                    .map_or(0, |ids| ids.iter().map(|id| id.len() + 128).sum::<usize>())
        }
        Operation::AdmitMessage {
            agent, activation, ..
        } => agent
            .len()
            .checked_add(activation.len())
            .context("TaskResult operation material overflow")?,
        Operation::Defer { reason, .. } => reason.len(),
        _ => 0,
    };
    limits.check(0, bytes)?;
    for row in statement.query_map(params, |r| {
        Ok(ResultAddress {
            id: r.get(0)?,
            task: r.get(1)?,
            bytes: r.get(2)?,
        })
    })? {
        let address = row?;
        bytes = bytes
            .checked_add(address.bytes)
            .and_then(|n| n.checked_add(address.id.len() + address.task.len() + 256))
            .context("TaskResult material overflow")?;
        limits.check(addresses.len() + 1, bytes)?;
        tasks.insert(address.task.clone());
        addresses.push(address);
    }
    for task in tasks
        .iter()
        .filter(|_| !matches!(operation, Operation::SettleInvalidOnly { .. }))
    {
        let n:Option<usize>=tx.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(json_array(task_id,owner_agent_id,parent_agent_id,child_agent_id,kind,status,summary,input_target,wait_policy,output_path,result_summary,exit_status,terminal_reentry,revision,created_at,updated_at,completed_at,last_message_id,last_turn_id,causation_id,correlation_id) AS BLOB)) FROM tasks WHERE task_id=?1",[task],|r|r.get(0)).optional()?;
        bytes = bytes
            .checked_add(n.unwrap_or_default())
            .context("TaskResult Task material overflow")?;
        limits.check(addresses.len() + tasks.len(), bytes)?;
    }
    let material = TaskResultMaterialEstimate {
        records: addresses.len() + tasks.len(),
        source_material_bytes: bytes,
        payload_bytes: bytes
            .checked_mul(12)
            .context("TaskResult owned material overflow")?,
        metadata_estimate_bytes: (addresses.len() + tasks.len()) * 256,
    };
    limits.check(
        material.records,
        material.source_material_bytes + material.metadata_estimate_bytes,
    )?;
    Ok(TaskResultSelection {
        tx,
        operation,
        addresses,
        message: message.map(str::to_owned),
        material,
        admission_projections: None,
    })
}
#[cfg(test)]
pub(crate) fn select_admit_unsettled_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    owner: Option<&str>,
    activation: &str,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    select_admit_unsettled_snapshot_tx(tx, agent, owner, activation, None, None, now, limits)
}
pub(crate) fn select_admit_unsettled_snapshot_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    owner: Option<&str>,
    activation: &str,
    anchor: Option<&str>,
    identities: Option<&BTreeSet<String>>,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    let exact = exact_waiter::candidate_tx(tx, agent, owner, activation)?;
    inventory(
        tx,
        Operation::AdmitUnsettled {
            agent: agent.into(),
            activation: activation.into(),
            anchor: anchor.map(str::to_owned),
            identities: identities.cloned(),
            now,
        },
        SELECTED_OWNER_SQL,
        params![
            agent,
            owner,
            if identities.is_some() {
                64
            } else {
                TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT as i64
            },
            identities.map(serde_json::to_string).transpose()?,
            exact.as_ref().map(|record| record.result_identity.as_str())
        ],
        None,
        limits,
    )
}
#[cfg(test)]
pub(crate) fn select_admit_reply_message_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    owner: Option<&str>,
    message: &str,
    activation: &str,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    select_admit_reply_snapshot_tx(tx, agent, owner, message, activation, None, now, limits)
}
pub(crate) fn select_admit_reply_snapshot_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    owner: Option<&str>,
    message: &str,
    activation: &str,
    identities: Option<&BTreeSet<String>>,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    let exact = exact_waiter::candidate_tx(tx, agent, owner, activation)?;
    inventory(
        tx,
        Operation::AdmitReply {
            agent: agent.into(),
            owner: owner.map(str::to_owned),
            activation: activation.into(),
            identities: identities.cloned(),
            now,
        },
        SELECTED_REPLY_SQL,
        params![
            agent,
            owner,
            message,
            identities.map(serde_json::to_string).transpose()?,
            exact.as_ref().map(|record| record.result_identity.as_str())
        ],
        None,
        limits,
    )
}
pub(crate) fn select_admit_message_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    message: &str,
    activation: &str,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    inventory(
        tx,
        Operation::AdmitMessage {
            agent: agent.into(),
            activation: activation.into(),
            now,
        },
        "SELECT payload_json FROM task_result_settlements WHERE message_id=?1",
        [message],
        Some(message),
        limits,
    )
}
pub(crate) fn select_settle_activation_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    activation: &str,
    disposition: TaskResultSettlementDisposition,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    inventory(
        tx,
        Operation::SettleActivation { disposition, now },
        ACTIVATION_SQL,
        params![
            agent,
            activation,
            disposition == TaskResultSettlementDisposition::ModelDelivered,
            timestamp(now)
        ],
        None,
        limits,
    )
}
/// Integer-only estimate must be charged before reading Result/Task address Strings.
/// This operation never loads a raw target Task body; its opaque predicate does
/// the separate qualified observation and material accounting.
pub(crate) fn settle_invalid_only_material_estimate_tx(
    tx: &Transaction<'_>,
    agent: &str,
    message: &str,
) -> Result<TaskResultMaterialEstimate> {
    let (count, bytes): (usize, usize) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload_json AS BLOB))+length(CAST(json_array(result_identity,agent_id,task_id,message_id,work_item_id,rejoin_generation,parent_turn_id,state,activation_id,disposition,created_at,updated_at,admitted_at,settled_at,deferred_reason,deferred_at,next_recheck_at,command_creation_id) AS BLOB))+length(CAST(result_identity AS BLOB))+length(CAST(task_id AS BLOB))+256),0) FROM task_result_settlements WHERE agent_id=?1 AND message_id=?2",
        params![agent, message], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        count <= 1,
        "Invalid-only Result message address is not unique"
    );
    let records = count
        .checked_mul(2)
        .context("Invalid-only Result count overflow")?;
    Ok(TaskResultMaterialEstimate {
        records,
        source_material_bytes: bytes,
        payload_bytes: bytes
            .checked_mul(12)
            .context("Invalid-only Result material overflow")?,
        metadata_estimate_bytes: records
            .checked_mul(256)
            .context("Invalid-only metadata overflow")?,
    })
}
pub(crate) fn select_settle_invalid_only_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    message: &str,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    let material = settle_invalid_only_material_estimate_tx(tx, agent, message)?;
    limits.check(
        material.records,
        material
            .payload_bytes
            .checked_add(material.metadata_estimate_bytes)
            .context("Invalid-only material overflow")?,
    )?;
    inventory(
        tx,
        Operation::SettleInvalidOnly { now },
        "SELECT payload_json FROM task_result_settlements WHERE agent_id=?1 AND message_id=?2",
        params![agent, message],
        None,
        limits,
    )
}
pub(crate) fn select_settle_owner_unavailable_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    message: &str,
    disposition: TaskResultSettlementDisposition,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    inventory(
        tx,
        Operation::SettleMessage { disposition, now },
        "SELECT payload_json FROM task_result_settlements WHERE message_id=?1",
        [message],
        Some(message),
        limits,
    )
}
pub(crate) fn select_mark_deferred_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    message: &str,
    reason: &str,
    now: DateTime<Utc>,
    deadline: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    limits.check(0, reason.len())?;
    inventory(
        tx,
        Operation::Defer {
            reason: reason.into(),
            now,
            deadline,
        },
        "SELECT payload_json FROM task_result_settlements WHERE message_id=?1",
        [message],
        Some(message),
        limits,
    )
}
pub(crate) fn select_mark_recovery_diagnostic_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    result_identity: &str,
    epoch: u64,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    limits.check(0, result_identity.len())?;
    inventory(
        tx,
        Operation::MarkRecoveryDiagnostic { epoch, now },
        "SELECT payload_json FROM task_result_settlements
         WHERE agent_id=?1 AND result_identity=?2",
        params![agent, result_identity],
        None,
        limits,
    )
}
pub(crate) fn select_wake_deferred_for_owner_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    agent: &str,
    owner: &str,
    now: DateTime<Utc>,
    limits: TaskResultWriteLimits,
) -> Result<TaskResultSelection<'tx, 'connection>> {
    inventory(
        tx,
        Operation::Wake { now },
        WAKE_SQL,
        params![agent, owner],
        None,
        limits,
    )
}
/// ensurePending needs the actual Task fence before result identity can be derived.
/// Run this lengths-only preflight, register the shared ledger guard, then prepare.
/// The SQL observes the same first generation candidate without decoding Task JSON.
pub(crate) fn ensure_pending_material_estimate_tx(
    tx: &Transaction<'_>,
    task_id: &str,
    message_id: &str,
) -> Result<TaskResultMaterialEstimate> {
    let task:Option<usize>=tx.query_row("SELECT length(CAST(payload_json AS BLOB))+length(CAST(json_array(task_id,owner_agent_id,parent_agent_id,child_agent_id,kind,status,summary,input_target,wait_policy,output_path,result_summary,exit_status,terminal_reentry,revision,created_at,updated_at,completed_at,last_message_id,last_turn_id,causation_id,correlation_id) AS BLOB)) FROM tasks WHERE task_id=?1",[task_id],|r|r.get(0)).optional()?;
    // The actual fence/generation is not decoded yet. A scalar MAX over the
    // finite task/message address covers whichever original first row applies.
    let result:Option<usize>=tx.query_row("SELECT MAX(length(CAST(payload_json AS BLOB))+length(CAST(json_array(result_identity,agent_id,task_id,message_id,work_item_id,rejoin_generation,parent_turn_id,state,activation_id,disposition,created_at,updated_at,admitted_at,settled_at,deferred_reason,deferred_at,next_recheck_at,command_creation_id) AS BLOB))) FROM task_result_settlements WHERE task_id=?1 OR message_id=?2",params![task_id,message_id],|r|r.get(0))?;
    let bytes = task
        .unwrap_or_default()
        .checked_add(message_id.len())
        .context("TaskResult message address overflow")?
        .checked_add(
            result
                .unwrap_or_default()
                .checked_mul(2)
                .context("TaskResult generation material overflow")?,
        )
        .context("TaskResult source material overflow")?;
    Ok(TaskResultMaterialEstimate {
        records: usize::from(task.is_some()) + 2 * usize::from(result.is_some()),
        source_material_bytes: bytes,
        payload_bytes: bytes
            .checked_mul(12)
            .context("TaskResult owned material overflow")?,
        metadata_estimate_bytes: 768,
    })
}
pub(crate) fn upsert_pending_material_estimate_tx(
    tx: &Transaction<'_>,
    task: &TaskRecord,
    record: &TaskResultSettlementRecord,
) -> Result<TaskResultMaterialEstimate> {
    let mut estimate = ensure_pending_material_estimate_tx(tx, &task.id, &record.message_id)?;
    let owned = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(&(
        task, record,
    ))?)?;
    estimate.source_material_bytes = estimate
        .source_material_bytes
        .checked_add(owned)
        .context("TaskResult command material overflow")?;
    estimate.payload_bytes = estimate
        .source_material_bytes
        .checked_mul(12)
        .context("TaskResult owned material overflow")?;
    estimate.records += 2;
    estimate.metadata_estimate_bytes += 512;
    Ok(estimate)
}
