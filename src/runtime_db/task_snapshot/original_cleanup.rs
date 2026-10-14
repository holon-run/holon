//! Finite Result creation from an original admitted Task cleanup, never admission.
use super::*;
#[cfg(test)]
use crate::runtime_db::control_transition::OriginalControlTaskCleanupView;
use crate::{
    runtime_db::{
        task_result_settlement::{PreparedTaskResultMutation, TaskResultSettlementRecord},
        transitions::OriginalTaskCleanupView,
    },
    types::{MessageEnvelope, TaskRecord},
};

/// Both views have private construction in the actual original command module.
pub(crate) enum OriginalTaskResultCleanupSource<'a> {
    Task(OriginalTaskCleanupView<'a>),
    #[cfg(test)]
    Control(OriginalControlTaskCleanupView<'a>),
    Stop(crate::runtime_db::task_stop::OriginalTaskStopResultView<'a>),
}
impl OriginalTaskResultCleanupSource<'_> {
    fn task(&self) -> &TaskRecord {
        match self {
            Self::Task(v) => v.task(),
            #[cfg(test)]
            Self::Control(v) => v.task(),
            Self::Stop(v) => v.task(),
        }
    }
    fn message(&self) -> &MessageEnvelope {
        match self {
            Self::Task(v) => v.message(),
            #[cfg(test)]
            Self::Control(v) => v.message(),
            Self::Stop(v) => v.message(),
        }
    }
    fn validate(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        before: &TaskRecord,
        result: &TaskResultSettlementRecord,
    ) -> Result<()> {
        match self {
            Self::Stop(v) => v.validate_tx(tx, scope, before)?,
            Self::Task(v) => {
                ensure!(
                    v.descriptor().scopes().contains(scope),
                    "Task cleanup original scope missing"
                );
                ensure!(
                    v.result() == result,
                    "Task cleanup differs from original pending Result"
                );
            }
            #[cfg(test)]
            Self::Control(v) => {
                ensure!(
                    v.key().agent_id == scope.agent_id
                        && v.key().identity_incarnation == scope.identity_incarnation,
                    "Control cleanup original incarnation mismatch"
                );
                ensure!(
                    &repositories::slim_task_record_for_payload(v.before()) == before,
                    "Control cleanup original Task snapshot changed"
                );
                ensure!(
                    v.command_digest().len() == 64
                        && !v.key().command_id.is_empty()
                        && result.created_at == v.prepared_at(),
                    "Control cleanup original address/time mismatch"
                );
            }
        }
        let final_task = self.task();
        ensure!(
            before.id == final_task.id
                && before.agent_id == scope.agent_id
                && final_task.agent_id == before.agent_id
                && before.kind == final_task.kind
                && before.created_at == final_task.created_at
                && before.work_item_id == final_task.work_item_id,
            "Task cleanup replaced original admitted Task identity"
        );
        ensure!(
            repositories::is_terminal_task_status(&final_task.status),
            "Task cleanup cannot admit Running/Queued work"
        );
        let original = before.rejoin_fence().map_err(anyhow::Error::msg)?;
        let final_fence = final_task.rejoin_fence().map_err(anyhow::Error::msg)?;
        ensure!(
            original.obligation_id == final_fence.obligation_id
                && original.generation == final_fence.generation
                && original.parent_turn_id == final_fence.parent_turn_id,
            "Task cleanup changed original admitted generation"
        );
        ensure!(
            TaskResultSettlementRecord::pending(final_task, self.message(), result.created_at)?
                .as_ref()
                == Some(result),
            "Task cleanup Result is not the original terminal derivation"
        );
        Ok(())
    }
}

/// Borrows the original Tx and command. It cannot authorize a different Result,
/// another Task generation, Message admission, or physical process completion.
pub(crate) struct OriginalTaskResultCleanupProof<'tx, 'connection, 'command> {
    maintenance: ExistingTaskMaintenanceProof<'tx, 'connection>,
    source: OriginalTaskResultCleanupSource<'command>,
    result: TaskResultSettlementRecord,
}

pub(crate) fn original_task_result_cleanup_material_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    keys: &BTreeSet<TaskKey>,
    pending: &PreparedTaskResultMutation<'_, '_>,
) -> Result<TaskMaintenanceMaterial> {
    let view = pending.upsert_pending_view()?;
    ensure!(
        std::ptr::eq(tx, view.transaction()),
        "pending command belongs to another transaction"
    );
    let mut material = task_maintenance_material_tx(tx, scope, keys)?;
    let added = read_preparation::encoded_metadata_bytes(&(view.task(), view.record()))?
        .checked_add(view.record().result_identity.len() + 512)
        .context("Task cleanup material overflow")?;
    material.bytes = material
        .bytes
        .checked_add(
            added
                .checked_mul(8)
                .context("Task cleanup copies overflow")?,
        )
        .context("Task cleanup material overflow")?;
    material.records = material
        .records
        .checked_add(16)
        .context("Task cleanup record overflow")?;
    bound(material.records, material.bytes, None)?;
    Ok(material)
}

pub(crate) fn prepare_original_task_result_cleanup_tx<'tx, 'connection, 'command>(
    tx: &'tx Transaction<'connection>,
    maintenance: ExistingTaskMaintenanceProof<'tx, 'connection>,
    source: OriginalTaskResultCleanupSource<'command>,
    pending: &PreparedTaskResultMutation<'_, '_>,
) -> Result<OriginalTaskResultCleanupProof<'tx, 'connection, 'command>> {
    ensure!(
        std::ptr::eq(tx, maintenance.tx),
        "Task cleanup proof belongs to another transaction"
    );
    let view = pending.upsert_pending_view()?;
    ensure!(
        std::ptr::eq(tx, view.transaction()),
        "Task cleanup pending belongs to another transaction"
    );
    let task_key = TaskKey::new(TaskFamily::Task, &source.task().id);
    let before = maintenance
        .before
        .get(&task_key)
        .filter(|value| value.qualified)
        .and_then(|value| value.before.as_ref())
        .context("Task cleanup has no original qualified Task")?;
    let CanonicalTaskFact::Task(before) = &before.fact else {
        anyhow::bail!("Task cleanup root is not Task")
    };
    let result = view.record();
    source.validate(tx, &maintenance.scope, before, result)?;
    ensure!(
        view.task() == &repositories::slim_task_record_for_payload(source.task()),
        "Task cleanup closed pending Task differs from original command"
    );
    let result_key = TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity);
    ensure!(
        !result.result_identity.is_empty(),
        "empty original Task Result identity"
    );
    if let Some(existing) = maintenance
        .before
        .get(&result_key)
        .filter(|value| value.before.is_some())
    {
        ensure!(
            existing.qualified,
            "Task cleanup existing Result is not qualified"
        );
        let Some(StoredTask {
            fact: CanonicalTaskFact::ResultSettlement(old),
            ..
        }) = &existing.before
        else {
            anyhow::bail!("Task cleanup existing Result missing")
        };
        ensure!(
            old.result_identity == result.result_identity
                && old.agent_id == result.agent_id
                && old.task_id == result.task_id
                && old.message_id == result.message_id
                && old.work_item_id == result.work_item_id
                && old.rejoin_generation == result.rejoin_generation
                && old.parent_turn_id == result.parent_turn_id,
            "Task cleanup existing Result address changed"
        );
    } else {
        ensure!(
            checkpoint_material_size_on(tx, &result_key)?.is_none()
                && !qualification::known_anywhere(tx, &result_key)?
                && !tx.query_row("SELECT EXISTS(SELECT 1 FROM task_domain_records WHERE family='result_settlement' AND record_id=?1)", [&result_key.id], |r|r.get::<_,bool>(0))?,
            "Task cleanup cannot adopt/revive an existing Result ID"
        );
    }
    Ok(OriginalTaskResultCleanupProof {
        maintenance,
        source,
        result: result.clone(),
    })
}
impl OriginalTaskResultCleanupProof<'_, '_, '_> {
    /// This view derives no Input permission itself. D3 must validate the finite
    /// Message/queue shape and actual readback in the same original transaction.
    pub(in crate::runtime_db) fn original_task_input_tx(
        &self,
        tx: &Transaction<'_>,
    ) -> Result<(
        &InputEvidenceScope,
        &crate::runtime_db::input_mutation_receipts::PreparedInputMutation,
        &MessageEnvelope,
        &TaskResultSettlementRecord,
    )> {
        ensure!(
            std::ptr::eq(tx, self.maintenance.tx),
            "Task input cleanup used in another transaction"
        );
        maintenance::require_existing_maintenance_scope_tx(tx, &self.maintenance.scope)?;
        let (descriptor, message) = match &self.source {
            OriginalTaskResultCleanupSource::Task(view) => (view.descriptor(), view.message()),
            OriginalTaskResultCleanupSource::Stop(view) => {
                let before = self
                    .maintenance
                    .before
                    .get(&TaskKey::new(TaskFamily::Task, &self.result.task_id))
                    .and_then(|entry| entry.before.as_ref())
                    .context("Stop Result original Task missing")?;
                let CanonicalTaskFact::Task(before) = &before.fact else {
                    unreachable!()
                };
                view.validate_tx(tx, &self.maintenance.scope, before)?;
                (view.descriptor(), view.message())
            }
            #[cfg(test)]
            OriginalTaskResultCleanupSource::Control(_) => {
                anyhow::bail!("Control Result requires its original Control input proof")
            }
        };
        let key = TaskKey::new(TaskFamily::Task, &self.result.task_id);
        let before = self
            .maintenance
            .before
            .get(&key)
            .and_then(|entry| entry.before.as_ref())
            .context("original Task input root missing")?;
        ensure!(
            stored_on(tx, &self.maintenance.scope.agent_id, &key)?.as_ref() == Some(before),
            "Task input proof must precede original Task writes"
        );
        Ok((&self.maintenance.scope, descriptor, message, &self.result))
    }

    pub(crate) fn result_key(&self) -> TaskKey {
        TaskKey::new(TaskFamily::ResultSettlement, &self.result.result_identity)
    }
    /// Called only by the closed UpsertPending executor after original Task SQL.
    pub(in crate::runtime_db) fn validate_pending_insert_tx(
        &self,
        tx: &Transaction<'_>,
        record: &TaskResultSettlementRecord,
    ) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.maintenance.tx),
            "Task cleanup used in another transaction"
        );
        maintenance::require_existing_maintenance_scope_tx(tx, &self.maintenance.scope)?;
        ensure!(
            record == &self.result,
            "Task cleanup used for another Result"
        );
        let task_key = TaskKey::new(TaskFamily::Task, &record.task_id);
        let stored = stored_on(tx, &self.maintenance.scope.agent_id, &task_key)?
            .context("Task cleanup actual terminal Task missing")?;
        ensure!(
            stored.fact
                == CanonicalTaskFact::Task(repositories::slim_task_record_for_payload(
                    self.source.task()
                )),
            "Task cleanup actual terminal Task differs from original command"
        );
        Ok(())
    }
    fn validate_actual_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        self.validate_pending_insert_tx(tx, &self.result)?;
        let key = self.result_key();
        let stored = stored_on(tx, &self.maintenance.scope.agent_id, &key)?
            .context("Task cleanup actual Result missing")?;
        let CanonicalTaskFact::ResultSettlement(actual) = &stored.fact else {
            unreachable!()
        };
        if !self
            .maintenance
            .before
            .get(&key)
            .is_some_and(|value| value.before.is_some())
        {
            ensure!(
                actual == &self.result,
                "new original Result differs from exact pending command"
            );
        }
        ensure!(
            actual.result_identity == self.result.result_identity
                && actual.agent_id == self.result.agent_id
                && actual.task_id == self.result.task_id
                && actual.message_id == self.result.message_id
                && actual.work_item_id == self.result.work_item_id
                && actual.rejoin_generation == self.result.rejoin_generation
                && actual.parent_turn_id == self.result.parent_turn_id,
            "Task cleanup actual Result changed original address"
        );
        Ok(())
    }
    /// Full-State callers use this once, then the ordinary original-recovery
    /// finalizer. Raw neighbors remain excluded from the qualified write proof.
    pub(crate) fn into_task_write_proof(
        self,
        tx: &Transaction<'_>,
        actual_result_ids: &BTreeSet<String>,
    ) -> Result<TaskWriteProof> {
        self.validate_actual_tx(tx)?;
        let result_key = self.result_key();
        ensure!(
            self.maintenance
                .before
                .get(&result_key)
                .is_some_and(|value| value.before.is_some())
                || actual_result_ids.contains(&self.result.result_identity),
            "new original Result lacks actual INSERT acknowledgement"
        );
        let mut before: BTreeMap<_, _> = self
            .maintenance
            .before
            .into_iter()
            .filter(|(_, v)| v.qualified)
            .map(|(key, v)| (key, v.before))
            .collect();
        before
            .entry(TaskKey::new(
                TaskFamily::ResultSettlement,
                &self.result.result_identity,
            ))
            .or_insert(None);
        Ok(writes::original_result_cleanup_proof(
            self.maintenance.scope,
            self.maintenance.base_revision,
            before,
        ))
    }
}
/// NoState/current-incomplete callers keep a sparse ACK, not fake TaskReady.
pub(crate) fn register_original_task_result_cleanup_tx(
    tx: &Transaction<'_>,
    mut proof: OriginalTaskResultCleanupProof<'_, '_, '_>,
    actual_keys: &BTreeSet<TaskKey>,
) -> Result<TaskMaintenanceAck> {
    proof.validate_actual_tx(tx)?;
    let key = proof.result_key();
    let mut derived = BTreeSet::new();
    if !proof
        .maintenance
        .before
        .get(&key)
        .is_some_and(|value| value.before.is_some())
    {
        ensure!(
            actual_keys.contains(&key),
            "new original Result lacks actual INSERT acknowledgement"
        );
        proof.maintenance.before.insert(
            key.clone(),
            maintenance::ExistingTarget {
                before: None,
                qualified: true,
            },
        );
        derived.insert(key);
    }
    maintenance::register_with_derived_tx(tx, proof.maintenance, actual_keys, &derived)
}
