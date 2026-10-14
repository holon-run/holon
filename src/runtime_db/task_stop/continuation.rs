//! The first Stop ACK's immutable Result address, independent of physical phases.
use super::*;
use crate::runtime_db::task_result_settlement::TaskResultSettlementRecord;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TaskStopResultAddress {
    original: PreparedInputMutation,
    binding: TaskStopReceiptBinding,
}
impl TaskStopResultAddress {
    pub(crate) fn from_record(record: &TaskStopRecord) -> Result<Self> {
        ensure!(
            record.effect_seq > 0 && record.intent.result.is_some(),
            "Stop has no frozen Result responsibility"
        );
        Ok(Self {
            original: record.prepared.clone(),
            binding: record.receipt_binding(),
        })
    }
    pub(crate) fn scope(&self) -> &InputEvidenceScope {
        &self.binding.scope
    }
    fn matches(&self, record: &TaskStopRecord) -> bool {
        self.original == record.prepared && self.binding == record.receipt_binding()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStopResultUnavailable {
    OwnerNoState,
    ScopeRetired,
    TaskGenerationChanged,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum TaskStopResultOutcome {
    Delivered {
        message_id: String,
        message_seq: u64,
        result_identity: Option<String>,
    },
    Unavailable {
        reason: TaskStopResultUnavailable,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskStopResultAck {
    pub phase_nonce: String,
    pub outcome: TaskStopResultOutcome,
}
impl TaskStopResultAck {
    pub(super) fn validate(
        &self,
        prepared: &PreparedInputMutation,
        progress: &TaskStopProgress,
    ) -> Result<()> {
        ensure!(
            self.phase_nonce == prepared.command_nonce(),
            "Stop Result ACK is not its frozen phase"
        );
        match &self.outcome {
            TaskStopResultOutcome::Delivered {
                message_id,
                message_seq,
                result_identity,
            } => ensure!(
                progress.result == TaskStopResultState::Completed
                    && !message_id.is_empty()
                    && *message_seq > 0
                    && result_identity.as_ref().is_none_or(|id| !id.is_empty()),
                "Stop Result ACK omits actual delivery"
            ),
            TaskStopResultOutcome::Unavailable { .. } => ensure!(
                progress.result == TaskStopResultState::NeedsAttention,
                "Stop unavailable Result ACK differs"
            ),
        }
        Ok(())
    }
}

pub(crate) struct SelectedTaskStopResult<'tx, 'connection> {
    tx: &'tx Transaction<'connection>,
    source: PreparedTaskStops,
}
impl SelectedTaskStopResult<'_, '_> {
    pub(crate) fn record(&self) -> &TaskStopRecord {
        &self.source.records[0]
    }
    pub(crate) fn budget_view(&self) -> Arc<BudgetViewGuard> {
        self.source._budget.clone()
    }
    /// The only constructor borrows this same original transaction's verified
    /// initial whole receipt and frozen non-Command Result template.
    pub(crate) fn original_cleanup_view<'a>(
        &'a self,
        descriptor: &'a PreparedInputMutation,
    ) -> Result<OriginalTaskStopResultView<'a>> {
        let record = self.record();
        let plan = record
            .intent
            .result
            .as_ref()
            .context("Stop Result template missing")?;
        ensure!(
            descriptor.family() == InputMutationFamily::Task
                && descriptor.scopes() == std::slice::from_ref(&record.intent.task.scope)
                && descriptor.command_nonce() == plan.phase_nonce
                && descriptor.prepared_at() == plan.message.created_at,
            "Stop Result phase replaced its original address/time"
        );
        Ok(OriginalTaskStopResultView {
            tx: self.tx,
            record,
            descriptor,
        })
    }
}

/// Authentic original Stop view. No Clone, serde or public construction.
/// Its transaction borrow cannot be replaced by a raw Task command or hint.
pub(crate) struct OriginalTaskStopResultView<'a> {
    tx: &'a Transaction<'a>,
    record: &'a TaskStopRecord,
    descriptor: &'a PreparedInputMutation,
}
impl OriginalTaskStopResultView<'_> {
    pub(crate) fn descriptor(&self) -> &PreparedInputMutation {
        self.descriptor
    }
    pub(crate) fn task(&self) -> &TaskRecord {
        &self.record.ack_task
    }
    pub(crate) fn message(&self) -> &MessageEnvelope {
        &self
            .record
            .intent
            .result
            .as_ref()
            .expect("verified template")
            .message
    }
    pub(crate) fn validate_tx(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        before: &TaskRecord,
    ) -> Result<()> {
        ensure!(
            std::ptr::eq(tx, self.tx) && scope == &self.record.intent.task.scope,
            "Stop Result cleanup crossed original transaction/scope"
        );
        ensure!(
            before
                == &super::super::repositories::slim_task_record_for_payload(&self.record.ack_task),
            "Stop Result current Task is not the original ACK generation"
        );
        let plan = self
            .record
            .intent
            .result
            .as_ref()
            .context("Stop Result template missing")?;
        ensure!(
            self.descriptor.command_nonce() == plan.phase_nonce
                && self.descriptor.prepared_at() == plan.message.created_at,
            "Stop Result cleanup changed frozen continuation"
        );
        Ok(())
    }
}

pub(crate) fn prepare_task_stop_result_tx<'tx, 'connection>(
    tx: &'tx Transaction<'connection>,
    address: &TaskStopResultAddress,
    plan: &TaskStopResultPlan,
    charge: impl FnMut(usize, usize) -> Result<Arc<BudgetViewGuard>>,
) -> Result<SelectedTaskStopResult<'tx, 'connection>> {
    let source = observe_task_stops_tx(
        tx,
        address.scope(),
        Some(address.binding.effect_seq),
        false,
        charge,
        None,
    )?;
    ensure!(
        source.records.len() == 1 && address.matches(&source.records[0]),
        "Stop Result original whole address differs"
    );
    ensure!(
        source.records[0].intent.result.as_ref() == Some(plan),
        "Stop Result replaced frozen Message/nonce/time"
    );
    Ok(SelectedTaskStopResult { tx, source })
}

/// Actual Result SQL and Stop progress are both validated before this non-Clone
/// same-transaction capability can admit the final domain observation.
pub(crate) struct TaskStopResultObservation<'tx, 'connection> {
    actual: ActualTaskStopProgress<'tx, 'connection>,
    ack: TaskStopResultAck,
}
impl TaskStopResultObservation<'_, '_> {
    pub(crate) fn actual(&self) -> &ActualTaskStopProgress<'_, '_> {
        &self.actual
    }
    pub(crate) fn result_ack(&self) -> &TaskStopResultAck {
        &self.ack
    }
    pub(in crate::runtime_db) fn validate_presence_tx(&self, tx: &Transaction<'_>) -> Result<()> {
        self.actual.validate_presence_tx(tx)?;
        validate_actual_result_ack_tx(tx, self.actual.record(), &self.ack)
    }
    pub(in crate::runtime_db) fn permits(
        &self,
        tx: &Transaction<'_>,
        record: &TaskStopRecord,
    ) -> Result<bool> {
        self.validate_presence_tx(tx)?;
        Ok(record == self.actual.record())
    }
}

pub(crate) fn acknowledge_task_stop_result_tx<'tx, 'connection>(
    selected: SelectedTaskStopResult<'tx, 'connection>,
    outcome: TaskStopResultOutcome,
) -> Result<TaskStopResultObservation<'tx, 'connection>> {
    let current = selected.record();
    let plan = current
        .intent
        .result
        .as_ref()
        .context("Stop frozen Result missing")?;
    ensure!(
        current.progress.result == TaskStopResultState::Pending,
        "Stop Result terminal without its original phase receipt"
    );
    let ack = TaskStopResultAck {
        phase_nonce: plan.phase_nonce.clone(),
        outcome,
    };
    validate_actual_result_ack_tx(selected.tx, current, &ack)?;
    let mut next = current.clone();
    match &ack.outcome {
        TaskStopResultOutcome::Delivered { .. } => {
            next.progress.result = TaskStopResultState::Completed;
            next.result_nonce = Some(plan.phase_nonce.clone());
        }
        TaskStopResultOutcome::Unavailable { reason } => {
            next.progress.result = TaskStopResultState::NeedsAttention;
            next.progress.reason = Some(format!("original Result unavailable: {reason:?}"));
        }
    }
    current
        .progress
        .validate_successor(&next.progress, &current.intent)?;
    next.revision = next
        .revision
        .checked_add(1)
        .context("Stop Result revision overflow")?;
    // Independent physical phases may already have a later clock. Never rewrite
    // the frozen business clock or move the sidecar's monotonic clock backwards.
    next.updated_at = current.updated_at.max(
        DateTime::parse_from_rfc3339(&super::super::migrations::timestamp(
            plan.message.created_at,
        ))?
        .with_timezone(&Utc),
    );
    next.validate()?;
    let changed = selected.tx.execute("UPDATE task_stop_effects SET revision=?1,result_state=?2,result_nonce=?3,progress_json=?4,updated_at=?5 WHERE effect_seq=?6 AND agent_id=?7 AND identity_incarnation=?8 AND intent_digest=?9 AND generation_digest=?10 AND ack_task_digest=?11 AND revision=?12 AND progress_json=?13",
        params![next.revision,state_name(next.progress.result)?,next.result_nonce,serde_json::to_string(&next.progress)?,
            super::super::migrations::timestamp(next.updated_at),next.effect_seq,next.intent.task.scope.agent_id,next.intent.task.scope.identity_incarnation,
            next.intent_digest,next.generation_digest,next.ack_task_digest,current.revision,serde_json::to_string(&current.progress)?])?;
    ensure!(changed == 1, "Stop Result original progress conflict");
    let actual = super::phase::actual_result_progress_tx(
        selected.tx,
        Arc::new(next),
        selected.source._budget,
    )?;
    let observation = TaskStopResultObservation { actual, ack };
    observation.validate_presence_tx(selected.tx)?;
    Ok(observation)
}

fn validate_actual_result_ack_tx(
    tx: &Transaction<'_>,
    record: &TaskStopRecord,
    ack: &TaskStopResultAck,
) -> Result<()> {
    let plan = record
        .intent
        .result
        .as_ref()
        .context("Stop frozen Result missing")?;
    ensure!(
        ack.phase_nonce == plan.phase_nonce,
        "Stop Result ACK changed frozen nonce"
    );
    match &ack.outcome {
        TaskStopResultOutcome::Delivered {
            message_id,
            message_seq,
            result_identity,
        } => {
            let expected = TaskResultSettlementRecord::pending(
                &record.ack_task,
                &plan.message,
                plan.message.created_at,
            )?;
            ensure!(
                message_id == &plan.message.id
                    && *message_seq > 0
                    && result_identity.as_deref()
                        == expected.as_ref().map(|r| r.result_identity.as_str()),
                "Stop Result ACK changed frozen address"
            );
            let actual:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM messages m JOIN input_evidence_current a ON a.family='message' AND a.record_id=m.evidence_id AND a.agent_id=m.agent_id JOIN queue_entries q ON q.message_id=m.evidence_id AND q.agent_id=m.agent_id WHERE m.evidence_id=?1 AND m.agent_id=?2 AND m.message_seq=?3 AND a.identity_incarnation=?4 AND a.availability='present' AND q.status='queued')",
                params![message_id,record.intent.task.scope.agent_id,message_seq,record.intent.task.scope.identity_incarnation],|r|r.get(0))?;
            ensure!(
                actual,
                "Stop Result lacks actual qualified Message/Queue ACK"
            );
            if let Some(expected) = expected {
                let associated:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_result_settlements WHERE result_identity=?1 AND agent_id=?2 AND task_id=?3 AND message_id=?4 AND rejoin_generation=?5)",
                    params![expected.result_identity,record.intent.task.scope.agent_id,record.ack_task.id,message_id,expected.rejoin_generation],|r|r.get(0))?;
                ensure!(
                    associated,
                    "Stop Result lacks actual original settlement ACK"
                );
            } else {
                let unexpected: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM task_result_settlements WHERE message_id=?1)",
                    [message_id],
                    |r| r.get(0),
                )?;
                ensure!(!unexpected, "no-rejoin feedback invented a settlement");
            }
        }
        TaskStopResultOutcome::Unavailable { reason } => {
            let mode = observe_task_stop_phase_mode_tx(tx, &record.intent.task.scope)?;
            ensure!(
                match reason {
                    TaskStopResultUnavailable::OwnerNoState =>
                        mode == TaskStopPhaseInstallMode::NoState,
                    TaskStopResultUnavailable::ScopeRetired =>
                        mode == TaskStopPhaseInstallMode::Retired,
                    TaskStopResultUnavailable::TaskGenerationChanged =>
                        matches!(
                            mode,
                            TaskStopPhaseInstallMode::Current
                                | TaskStopPhaseInstallMode::Incomplete
                        ) && !original_task_generation_matches_tx(tx, record)?,
                },
                "Stop Result unavailable scope proof differs"
            );
        }
    }
    Ok(())
}

/// Only a small same-view predicate is decoded; no foreign Task body is loaded.
pub(crate) fn original_task_generation_matches_tx(
    tx: &Transaction<'_>,
    record: &TaskStopRecord,
) -> Result<bool> {
    let owner: Option<String> = tx
        .query_row(
            "SELECT owner_agent_id FROM tasks WHERE task_id=?1",
            [&record.ack_task.id],
            |r| r.get(0),
        )
        .optional()?;
    if owner.as_deref() != Some(record.intent.task.scope.agent_id.as_str()) {
        return Ok(false);
    }
    let kind = serde_json::to_value(record.intent.kind)?;
    let created = serde_json::to_value(record.intent.created_at)?;
    let detail = serde_json::to_string(&record.ack_task.detail)?;
    let tuple: Option<(bool,bool)> = tx.query_row(
        "SELECT COALESCE(owner_agent_id=json_extract(payload_json,'$.agent_id') AND task_id=json_extract(payload_json,'$.id'),0),COALESCE(owner_agent_id=?2 AND json_extract(payload_json,'$.kind')=?3 AND json_extract(payload_json,'$.created_at')=?4 AND json_extract(payload_json,'$.detail.rejoin_obligation_id') IS json_extract(?5,'$.rejoin_obligation_id') AND json_extract(payload_json,'$.detail.rejoin_generation') IS json_extract(?5,'$.rejoin_generation') AND json_extract(payload_json,'$.detail.parent_turn_id') IS json_extract(?5,'$.parent_turn_id'),0) FROM tasks WHERE task_id=?1",
        params![record.ack_task.id,record.intent.task.scope.agent_id,kind.as_str().context("Task kind encoding")?,created.as_str().context("Task clock encoding")?,detail], |r|Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    match tuple {
        Some((consistent, matches)) => {
            ensure!(consistent, "Stop current Task predicate projection corrupt");
            Ok(matches)
        }
        None => Ok(false),
    }
}

pub(super) fn validate_stored_result_receipt_tx(
    tx: &Transaction<'_>,
    record: &TaskStopRecord,
) -> Result<()> {
    if !matches!(
        record.progress.result,
        TaskStopResultState::Completed | TaskStopResultState::NeedsAttention
    ) || record.intent.result.is_none()
    {
        return Ok(());
    }
    let plan = record.intent.result.as_ref().expect("checked Result plan");
    let descriptor: String = tx.query_row(
        "SELECT prepared_json FROM input_mutation_receipts WHERE command_nonce=?1",
        [&plan.phase_nonce],
        |r| r.get(0),
    )?;
    let descriptor: PreparedInputMutation = serde_json::from_str(&descriptor)?;
    let receipt = lookup_input_mutation_receipt_tx(tx, &descriptor)?
        .context("Stop Result original whole receipt missing")?;
    let InputMutationResult::TaskStopPhase { ack } = &receipt.outcome.result else {
        anyhow::bail!("Stop Result receipt has another original outcome")
    };
    let result = ack
        .result
        .as_ref()
        .context("Stop Result receipt is not a delivery phase")?;
    ensure!(
        ack.binding == record.receipt_binding()
            && ack.original_nonce == record.prepared.command_nonce()
            && result.phase_nonce == plan.phase_nonce
            && ack.progress.result == record.progress.result,
        "Stop Result original whole receipt binding differs"
    );
    if let TaskStopResultOutcome::Delivered {
        message_id,
        message_seq,
        result_identity,
    } = &result.outcome
    {
        let expected = TaskResultSettlementRecord::pending(
            &record.ack_task,
            &plan.message,
            plan.message.created_at,
        )?;
        ensure!(
            message_id == &plan.message.id
                && *message_seq > 0
                && result_identity.as_deref()
                    == expected.as_ref().map(|r| r.result_identity.as_str()),
            "stored Stop Result receipt changed frozen delivery address"
        );
    }
    // The original ACK remains proof after canonical Message/Result retention.
    // A current delivery's readback was validated before this immutable receipt.
    Ok(())
}

/// Insert-time Result ACK binding; later lookup relies on the immutable checksum.
pub(super) fn validate_result_phase_insert_tx(
    tx: &Transaction<'_>,
    ack: &TaskStopPhaseAck,
) -> Result<()> {
    let Some(result) = &ack.result else {
        return Ok(());
    };
    let exact:bool=tx.query_row("SELECT COALESCE((SELECT json_extract(intent_json,'$.result.phase_nonce')=?1 AND (result_state!='completed' OR result_nonce=?1) FROM task_stop_effects WHERE effect_seq=?2),0)",
        params![result.phase_nonce,ack.binding.effect_seq],|r|r.get(0))?;
    ensure!(exact, "Stop Result ACK changed actual frozen phase");
    if let TaskStopResultOutcome::Delivered {
        message_id,
        message_seq,
        result_identity,
    } = &result.outcome
    {
        let actual:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_stop_effects s JOIN messages m ON m.evidence_id=json_extract(s.intent_json,'$.result.message.id') JOIN queue_entries q ON q.message_id=m.evidence_id AND q.agent_id=m.agent_id WHERE s.effect_seq=?1 AND m.agent_id=s.agent_id AND m.evidence_id=?2 AND m.message_seq=?3 AND q.status='queued' AND (?4 IS NULL OR EXISTS(SELECT 1 FROM task_result_settlements r WHERE r.message_id=m.evidence_id AND r.result_identity=?4 AND r.agent_id=s.agent_id AND r.task_id=s.task_id)))",
            params![ack.binding.effect_seq,message_id,message_seq,result_identity],|r|r.get(0))?;
        ensure!(
            actual,
            "Stop Result receipt omitted actual Message/Queue/settlement ACK"
        );
    }
    Ok(())
}
