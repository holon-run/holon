//! Explicit, owned evidence and Host-maintenance whole-transaction commands.
//! No command body, mutable current record, or large tool output is a receipt.
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

#[cfg(test)]
use super::evidence::{append_message_tx, committed_message_tx, CommittedTurnRecord};
use super::{
    evidence::{
        append_brief_with_created_event_tx, append_transcript_entry_tx, committed_tool_tx,
        committed_transcript_tx, insert_runtime_index_changes_tx, insert_tool_evidence_tx,
        BriefCreatedCommit,
    },
    input_evidence_snapshot::{
        validate_existing_input_evidence_tx, CanonicalInputEvidence as Fact,
    },
    input_evidence_writes::{
        acknowledge_input_evidence_tx, validate_input_evidence_scope_tx, InputEvidenceCommit,
    },
    input_mutation_receipts::{
        canonical_command_digest, insert_input_mutation_receipt_tx,
        lookup_input_mutation_receipt_tx, InputMutationEvidenceFamily as ResultFamily,
        InputMutationFamily, InputMutationOutcome, InputMutationResult, InputMutationScopeOutcome,
        PreparedInputMutation,
    },
    repositories::{committed_turn_record_tx, upsert_turn_record_tx},
    retention::{RuntimeDbRetentionPolicy, ScopedRuntimeDbRetentionCommit},
    transitions::InputMutationExecution,
    write_queue::RuntimeDbWriteContext,
    EvidenceRepository, RuntimeDb, RuntimeIndexChange,
};
#[cfg(test)]
use crate::types::MessageEnvelope;
use crate::{
    runtime::resident_input_evidence::{
        InputEvidenceFamily as Family, InputEvidenceKey, InputEvidenceScope,
    },
    types::{AuditEvent, BriefRecord, ToolExecutionRecord, TranscriptEntry, TurnRecord},
};

#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EvidenceCommand {
    #[cfg(test)]
    Message {
        record: MessageEnvelope,
        index_changes: Vec<RuntimeIndexChange>,
    },
    #[cfg(test)]
    Turn {
        record: TurnRecord,
    },
    TurnBarrier {
        record: TurnRecord,
        prior_digest: String,
    },
    TurnIdleBatch {
        record: TurnRecord,
        prior_digest: String,
    },
    Transcript {
        record: TranscriptEntry,
    },
    Tool {
        record: ToolExecutionRecord,
        index_changes: Vec<RuntimeIndexChange>,
    },
    BriefCreated {
        record: BriefRecord,
        event: AuditEvent,
        index_changes: Vec<RuntimeIndexChange>,
    },
}
impl EvidenceCommand {
    fn identity(&self) -> (&str, &str, Family, ResultFamily) {
        match self {
            #[cfg(test)]
            Self::Message { record, .. } => (
                &record.agent_id,
                &record.id,
                Family::Message,
                ResultFamily::Message,
            ),
            #[cfg(test)]
            Self::Turn { record } => (
                &record.agent_id,
                &record.turn_id,
                Family::Turn,
                ResultFamily::Turn,
            ),
            Self::TurnBarrier { record, .. } | Self::TurnIdleBatch { record, .. } => (
                &record.agent_id,
                &record.turn_id,
                Family::Turn,
                ResultFamily::Turn,
            ),
            Self::Transcript { record } => (
                &record.agent_id,
                &record.id,
                Family::Transcript,
                ResultFamily::Transcript,
            ),
            Self::Tool { record, .. } => (
                &record.agent_id,
                &record.id,
                Family::ToolExecution,
                ResultFamily::ToolExecution,
            ),
            Self::BriefCreated { record, .. } => (
                &record.agent_id,
                &record.id,
                Family::Brief,
                ResultFamily::Brief,
            ),
        }
    }
}

/// Unprepared input moved into storage once to derive its original index intent.
#[derive(Debug)]
pub(crate) enum InputEvidenceWrite {
    Transcript(TranscriptEntry),
    Tool(ToolExecutionRecord),
    BriefCreated {
        brief: BriefRecord,
        event: AuditEvent,
    },
}

/// No Clone: callers freeze and retain the single original body before begin.
#[derive(Debug)]
pub(crate) struct PreparedInputEvidence {
    descriptor: PreparedInputMutation,
    primary_scope: InputEvidenceScope,
    command: Box<EvidenceCommand>,
    original_material:
        Option<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}
// Exhaustive destructuring makes every non-batch Turn field part of the fence.
fn idle_batch_invariants(record: &TurnRecord) -> impl PartialEq + '_ {
    let TurnRecord {
        turn_id,
        turn_index,
        agent_id,
        run_id,
        current_work_item_id,
        owner,
        trigger,
        input_message_ids: _,
        idle_input_batch: _,
        live_input_batches,
        tool_execution_ids,
        produced_brief_ids,
        delivery_summary_ids,
        completed_work_item_ids,
        waiting_condition_ids,
        runtime_failure,
        terminal,
        replay,
        destructive_operation_id,
        external_side_effect_possible,
        created_at,
    } = record;
    (
        (
            turn_id,
            turn_index,
            agent_id,
            run_id,
            current_work_item_id,
            owner,
            trigger,
            live_input_batches,
            tool_execution_ids,
        ),
        (
            produced_brief_ids,
            delivery_summary_ids,
            completed_work_item_ids,
            waiting_condition_ids,
            runtime_failure,
            terminal,
            replay,
            destructive_operation_id,
            external_side_effect_possible,
            created_at,
        ),
    )
}

/// One canonical Value tree, not a completion decode/resolver/projection graph.
/// Admit its owning shape before allocation and hash without an output Vec.
fn admitted_canonical_digest(value: &impl serde::Serialize) -> Result<String> {
    use crate::diagnostics::resource_io::{charge_owned_metadata, cloned_value_material};

    // The pure one-tree estimate includes string/key/container capacity. Unlike
    // admit_serialized_value, this operation creates no aggregate copies.
    let (bytes, records) = cloned_value_material(value)?;
    charge_owned_metadata(u64::try_from(bytes)?, records)?;
    let mut canonical = serde_json::to_value(value)?;
    canonical.sort_all_objects();
    struct DigestWriter(Sha256);
    impl std::io::Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, &canonical)?;
    Ok(format!("{:x}", writer.0.finalize()))
}

impl PreparedInputEvidence {
    fn new(
        scope: InputEvidenceScope,
        nonce: String,
        at: DateTime<Utc>,
        command: EvidenceCommand,
    ) -> Result<Self> {
        Self::new_with_digest(scope, nonce, at, command, canonical_command_digest)
    }
    fn new_with_digest(
        scope: InputEvidenceScope,
        nonce: String,
        at: DateTime<Utc>,
        command: EvidenceCommand,
        digest: impl FnOnce(&EvidenceCommand) -> Result<String>,
    ) -> Result<Self> {
        ensure!(
            command.identity().0 == scope.agent_id,
            "prepared evidence belongs to another scope"
        );
        let command_digest = digest(&command)?;
        let descriptor = PreparedInputMutation::new(
            InputMutationFamily::Evidence,
            nonce,
            vec![scope.clone()],
            command_digest,
            at,
        )?;
        Ok(Self {
            descriptor,
            primary_scope: scope,
            command: Box::new(command),
            original_material: None,
        })
    }
    #[cfg(test)]
    pub(crate) fn message(
        scope: InputEvidenceScope,
        nonce: String,
        at: DateTime<Utc>,
        record: MessageEnvelope,
        index_changes: Vec<RuntimeIndexChange>,
    ) -> Result<Self> {
        Self::new(
            scope,
            nonce,
            at,
            EvidenceCommand::Message {
                record,
                index_changes,
            },
        )
    }
    #[cfg(test)]
    pub(crate) fn turn(
        scope: InputEvidenceScope,
        nonce: String,
        at: DateTime<Utc>,
        record: TurnRecord,
    ) -> Result<Self> {
        Self::new(scope, nonce, at, EvidenceCommand::Turn { record })
    }
    pub(crate) fn turn_barrier(
        scope: InputEvidenceScope,
        nonce: String,
        at: DateTime<Utc>,
        prior: &TurnRecord,
        possible: bool,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        ensure!(
            prior.terminal.is_none() && prior.external_side_effect_possible != possible,
            "Turn barrier requires an unfinished changed flag"
        );
        let material =
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(ledger, &scope)?;
        let mut prepared = material.read(|| {
            use crate::diagnostics::resource_io::{
                admit_cloned_value, admit_serialized_value, charge_owned_metadata,
            };
            // Count shape without an intermediate Value before either digest
            // builds its sorted JSON tree or the command clones its Turn.
            admit_serialized_value(prior)?;
            let prior_digest = canonical_command_digest(prior)?;
            admit_cloned_value(prior)?;
            let mut record = prior.clone();
            record.external_side_effect_possible = possible;
            let command = EvidenceCommand::TurnBarrier {
                record,
                prior_digest,
            };
            admit_serialized_value(&command)?;
            let descriptor_bytes = scope
                .agent_id
                .len()
                .checked_mul(2)
                .and_then(|n| n.checked_add(nonce.len()))
                .and_then(|n| n.checked_add(512))
                .ok_or_else(|| anyhow::anyhow!("Turn barrier material overflow"))?;
            charge_owned_metadata(u64::try_from(descriptor_bytes)?, 8)?;
            Self::new(scope, nonce, at, command)
        })?;
        prepared.original_material = Some(material.guard()?);
        Ok(prepared)
    }
    pub(crate) fn transcript(
        scope: InputEvidenceScope,
        nonce: String,
        at: DateTime<Utc>,
        record: TranscriptEntry,
    ) -> Result<Self> {
        Self::new(scope, nonce, at, EvidenceCommand::Transcript { record })
    }
    pub(crate) fn turn_idle_batch(
        scope: InputEvidenceScope,
        nonce: String,
        at: DateTime<Utc>,
        prior: &TurnRecord,
        record: TurnRecord,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let material =
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(ledger, &scope)?;
        let mut prepared = material.read(|| {
            use crate::diagnostics::resource_io::charge_owned_metadata;
            ensure!(
                prior.terminal.is_none() && prior.idle_input_batch.is_none(),
                "idle batch requires an unfinished unattached Turn"
            );
            ensure!(
                idle_batch_invariants(prior) == idle_batch_invariants(&record),
                "idle batch changed unrelated Turn fields"
            );
            let prior_digest = admitted_canonical_digest(prior)?;
            let command = EvidenceCommand::TurnIdleBatch {
                record,
                prior_digest,
            };
            // Scope copies, descriptor JSON/digest and the owned command header
            // are separate from the two admitted Turn trees.
            let descriptor_bytes = scope
                .agent_id
                .len()
                .checked_mul(8)
                .and_then(|n| n.checked_add(nonce.len()))
                .and_then(|n| n.checked_add(2048))
                .ok_or_else(|| anyhow::anyhow!("idle batch descriptor material overflow"))?;
            charge_owned_metadata(u64::try_from(descriptor_bytes)?, 64)?;
            Self::new_with_digest(scope, nonce, at, command, admitted_canonical_digest)
        })?;
        prepared.original_material = Some(material.guard()?);
        Ok(prepared)
    }
    pub(crate) fn tool(
        scope: InputEvidenceScope,
        nonce: String,
        at: DateTime<Utc>,
        record: ToolExecutionRecord,
        index_changes: Vec<RuntimeIndexChange>,
    ) -> Result<Self> {
        Self::new(
            scope,
            nonce,
            at,
            EvidenceCommand::Tool {
                record,
                index_changes,
            },
        )
    }
    pub(crate) fn brief_created(
        scope: InputEvidenceScope,
        nonce: String,
        at: DateTime<Utc>,
        record: BriefRecord,
        event: AuditEvent,
        index_changes: Vec<RuntimeIndexChange>,
    ) -> Result<Self> {
        Self::new(
            scope,
            nonce,
            at,
            EvidenceCommand::BriefCreated {
                record,
                event,
                index_changes,
            },
        )
    }
    pub(crate) fn descriptor(&self) -> &PreparedInputMutation {
        &self.descriptor
    }
    pub(crate) fn owned_payload_bytes(&self) -> Result<u64> {
        Ok(crate::runtime::input_evidence_budget::serialized_bytes(
            self.command.as_ref(),
        )?)
    }
    pub(crate) fn original_material(
        &self,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<Option<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>>
    {
        if let Some(material) = &self.original_material {
            material.validate_owned_material_binding(
                &self.primary_scope,
                ledger,
                self.owned_payload_bytes()?,
            )?;
        }
        Ok(self.original_material.clone())
    }
    pub(crate) fn primary_scope(&self) -> &InputEvidenceScope {
        &self.primary_scope
    }
    pub(crate) fn with_scope_group(mut self, scopes: Vec<InputEvidenceScope>) -> Result<Self> {
        ensure!(
            scopes.contains(&self.primary_scope),
            "prepared evidence group omits its original primary scope"
        );
        self.descriptor = PreparedInputMutation::new(
            self.descriptor.family(),
            self.descriptor.command_nonce().to_owned(),
            scopes,
            self.descriptor.command_digest().to_owned(),
            self.descriptor.prepared_at(),
        )?;
        Ok(self)
    }
}

/// Full records survive only the first ACK. Tool evidence needs no second full
/// body: its bounded, qualified delta is already present in InputEvidenceCommit.
#[derive(Debug)]
pub(crate) enum PreparedEvidenceValue {
    #[cfg(test)]
    Message(MessageEnvelope),
    #[cfg(test)]
    Turn(CommittedTurnRecord),
    TurnBarrierDone,
    Transcript(TranscriptEntry),
    ToolDone,
    BriefCreated(BriefCreatedCommit),
}

impl EvidenceRepository<'_> {
    #[cfg(test)]
    pub(crate) fn commit_prepared_input_evidence(
        &self,
        prepared: &PreparedInputEvidence,
    ) -> Result<InputMutationExecution<InputEvidenceCommit<PreparedEvidenceValue>>> {
        self.commit_prepared_input_evidence_with_work_request(
            prepared,
            &super::work_snapshot::WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 65_536,
                byte_limit: 64 * 1024 * 1024,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn commit_prepared_input_evidence_with_work_request(
        &self,
        prepared: &PreparedInputEvidence,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
    ) -> Result<InputMutationExecution<InputEvidenceCommit<PreparedEvidenceValue>>> {
        self.commit_prepared_input_evidence_with_budgets(prepared, work_request, None, None, None)
    }

    pub(crate) fn commit_prepared_input_evidence_with_task_budget(
        &self,
        prepared: &PreparedInputEvidence,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        task_request: &super::task_snapshot::TaskDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        input_query_source: Option<
            &crate::runtime::resident_input_evidence::OriginalInputQuerySource,
        >,
        current_query_source: Option<
            &std::sync::Arc<crate::runtime_db::current_query::CurrentQuerySource>,
        >,
    ) -> Result<InputMutationExecution<InputEvidenceCommit<PreparedEvidenceValue>>> {
        self.commit_prepared_input_evidence_with_budgets(
            prepared,
            work_request,
            Some((task_request, ledger)),
            input_query_source,
            current_query_source,
        )
    }

    fn commit_prepared_input_evidence_with_budgets(
        &self,
        prepared: &PreparedInputEvidence,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        task_budget: Option<(
            &super::task_snapshot::TaskDomainLoadRequest,
            &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        )>,
        input_query_source: Option<
            &crate::runtime::resident_input_evidence::OriginalInputQuerySource,
        >,
        current_query_source: Option<
            &std::sync::Arc<crate::runtime_db::current_query::CurrentQuerySource>,
        >,
    ) -> Result<InputMutationExecution<InputEvidenceCommit<PreparedEvidenceValue>>> {
        let scope = &prepared.primary_scope;
        let (agent, id, family, result_family) = prepared.command.identity();
        self.db.transaction(|tx| {
            ensure!(
                scope.agent_id == agent && prepared.descriptor.scopes().contains(scope),
                "prepared evidence primary scope differs from its command group"
            );
            for reserved_scope in prepared.descriptor.scopes() {
                if reserved_scope == scope {
                    validate_input_evidence_scope_tx(tx, reserved_scope, &reserved_scope.agent_id)?;
                } else {
                    super::input_evidence_writes::validate_readonly_input_reservation_tx(
                        tx,
                        reserved_scope,
                    )?;
                }
            }
            if let Some(receipt) = lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)? {
                return Ok(InputMutationExecution::Replayed {
                    receipt,
                    _material_budget_views: Vec::new(),
                });
            }
            let fallback_ledger;
            let ledger = if let Some((_, ledger)) = task_budget {
                ledger
            } else {
                fallback_ledger =
                    crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
                        Default::default(),
                    )?;
                &fallback_ledger
            };
            let mut native_input =
                super::input_evidence_snapshot::native_birth::NativeInputWriteProof::prepare_tx(
                    tx,
                    scope,
                    ledger,
                    super::input_evidence_snapshot::native_birth::original_material_limits(ledger)?,
                    None,
                    |visit| visit(family, id),
                )?;
            if let Some(native) = &mut native_input {
                native.bind_query_source(input_query_source, ledger)?;
            }
            let original_current = if let Some(native) = &mut native_input {
                native.with_input_phase(tx, ledger, None, |material| {
                    let mut current = super::current_read_writes::OriginalCurrentWrites::prepare(
                        tx,
                        scope,
                        material,
                        |visit| {
                            if let EvidenceCommand::BriefCreated { event, .. } =
                                prepared.command.as_ref()
                            {
                                visit(super::current_read_writes::CurrentFamily::Event, &event.id)?;
                            }
                            Ok(())
                        },
                    )?;
                    if let Some(current) = &mut current {
                        current.bind_query_source(current_query_source, material)?;
                    }
                    Ok(current)
                })?
            } else {
                None
            };
            let completion =
                if matches!(family, Family::Brief | Family::Transcript | Family::Message) {
                    let result = match prepared.command.as_ref() {
                        EvidenceCommand::BriefCreated { record, .. } => Some(record),
                        _ => None,
                    };
                    Some(super::work_snapshot::prepare_work_completion_write_tx(
                        tx,
                        scope,
                        result,
                        work_request,
                    )?)
                } else {
                    None
                };
            if matches!(prepared.command.as_ref(), EvidenceCommand::TurnBarrier { .. } | EvidenceCommand::TurnIdleBatch { .. }) {
                native_input
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Turn barrier requires original Native Input"))?
                    .with_input_phase(tx, ledger, None, |material| {
                        let raw: usize = tx.query_row(
                            "SELECT length(CAST(payload_json AS BLOB)) FROM turn_records WHERE agent_id=?1 AND turn_id=?2",
                            params![agent, id], |row| row.get(0),
                        )?;
                        material.observe_native_footprint(
                            raw.checked_mul(8).ok_or_else(|| anyhow::anyhow!("Turn barrier payload overflow"))?,
                            16,
                        )?;
                        let nodes: usize = tx.query_row(
                            "SELECT COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM turn_records t,json_tree(t.payload_json) j WHERE t.agent_id=?1 AND t.turn_id=?2",
                            params![agent, id], |row| row.get(0),
                        )?;
                        material.observe_native_footprint(
                            nodes.checked_mul(256 * 8).ok_or_else(|| anyhow::anyhow!("Turn barrier tree overflow"))?,
                            nodes.checked_mul(8).ok_or_else(|| anyhow::anyhow!("Turn barrier nodes overflow"))?,
                        )?;
                        Ok(())
                    })?;
            }
            validate_existing_input_evidence_tx(tx, scope, &InputEvidenceKey::new(family, id))?;
            let before = canonical_marker_tx(tx, agent, id, family)?;
            let before_index = index_frontier_tx(tx, agent)?;
            let (record, fact, actual_seq, created_event_seq, audit_events) =
                match prepared.command.as_ref() {
                    #[cfg(test)]
                    EvidenceCommand::Message {
                        record,
                        index_changes,
                    } => {
                        let (_, inserted) = append_message_tx(tx, record)?;
                        if inserted {
                            insert_runtime_index_changes_tx(tx, index_changes)?;
                        }
                        let actual = committed_message_tx(tx, agent, id)?;
                        let seq = actual.message_seq;
                        (
                            PreparedEvidenceValue::Message(actual.clone()),
                            Fact::Message(actual),
                            seq,
                            None,
                            Vec::new(),
                        )
                    }
                    #[cfg(test)]
                    EvidenceCommand::Turn { record } => {
                        upsert_turn_record_tx(tx, record)?;
                        let actual = committed_turn_record_tx(tx, agent, id)?;
                        let fact = Fact::Turn(actual.record.clone());
                        (
                            PreparedEvidenceValue::Turn(actual),
                            fact,
                            None,
                            None,
                            Vec::new(),
                        )
                    }
                    EvidenceCommand::TurnBarrier { record, prior_digest }
                    | EvidenceCommand::TurnIdleBatch { record, prior_digest } => {
                        let prior = committed_turn_record_tx(tx, agent, id)?;
                        ensure!(
                            prior.record.terminal.is_none()
                                && canonical_command_digest(&prior.record)? == *prior_digest,
                            "Turn barrier prior changed"
                        );
                        let current: bool = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM agent_states WHERE agent_id=?1 AND turn_index=?2 AND json_extract(payload_json,'$.current_turn_id')=?3 AND json_extract(payload_json,'$.turn_index')=?2 AND json_extract(payload_json,'$.current_run_id') IS ?4)",
                            params![agent, record.turn_index, id, record.run_id],
                            |row| row.get(0),
                        )?;
                        ensure!(current, "Turn barrier is no longer current");
                        upsert_turn_record_tx(tx, record)?;
                        let actual = committed_turn_record_tx(tx, agent, id)?;
                        (
                            PreparedEvidenceValue::TurnBarrierDone,
                            Fact::Turn(actual.record),
                            None,
                            None,
                            Vec::new(),
                        )
                    }
                    EvidenceCommand::Transcript { record } => {
                        append_transcript_entry_tx(tx, record)?;
                        let actual = committed_transcript_tx(tx, agent, id)?;
                        let seq = actual.transcript_seq;
                        (
                            PreparedEvidenceValue::Transcript(actual.clone()),
                            Fact::Transcript(actual),
                            seq,
                            None,
                            Vec::new(),
                        )
                    }
                    EvidenceCommand::Tool {
                        record,
                        index_changes,
                    } => {
                        insert_tool_evidence_tx(tx, record)?;
                        insert_runtime_index_changes_tx(tx, index_changes)?;
                        let actual = committed_tool_tx(tx, agent, id)?;
                        (
                            PreparedEvidenceValue::ToolDone,
                            Fact::ToolExecution(actual),
                            None,
                            None,
                            Vec::new(),
                        )
                    }
                    EvidenceCommand::BriefCreated {
                        record,
                        event,
                        index_changes,
                    } => {
                        let actual = append_brief_with_created_event_tx(
                            tx,
                            Some(agent),
                            record,
                            event,
                            index_changes,
                        )?;
                        let events = if actual.event_inserted {
                            vec![actual.event.clone()]
                        } else {
                            Vec::new()
                        };
                        let seq = actual.brief.created_event_seq;
                        let fact = Fact::Brief(actual.brief.clone());
                        (
                            PreparedEvidenceValue::BriefCreated(actual),
                            fact,
                            None,
                            seq,
                            events,
                        )
                    }
                };
            let audit_events = super::transitions::AuditEventAck::from_events(audit_events)?;
            // Original acknowledgement closes qualification and actual Message
            // count before the receipt. These are never reconstructed late.
            let mut value = acknowledge_input_evidence_tx(tx, scope, record, &[fact])?;
            let mut result_work_ids = std::collections::BTreeSet::new();
            if let Some(completion) = completion {
                let mut brief_ids = std::collections::BTreeSet::new();
                if let PreparedEvidenceValue::BriefCreated(actual) = &value.record {
                    if actual.brief.kind == crate::types::BriefKind::Result
                        && !actual.brief.text.is_empty()
                    {
                        if let Some(work) = &actual.brief.work_item_id {
                            result_work_ids.insert(work.clone());
                            brief_ids.insert(actual.brief.id.clone());
                        }
                    }
                }
                value.work_delta = super::work_snapshot::finish_work_completion_write_tx(
                    tx,
                    completion,
                    &brief_ids,
                    work_request,
                )?;
            }
            if let Some((task_request, ledger)) = task_budget {
                if let Some((delta, guards)) =
                    super::task_snapshot::register_task_source_metadata_tx(
                        tx,
                        scope,
                        &value.input_delta,
                        task_request,
                        ledger,
                        None,
                    )?
                {
                    value.task_delta = Some(delta);
                    value.task_material_budget_views = guards;
                }
            }
            if let (Some(current), Some(native)) = (original_current, native_input.as_mut()) {
                value.current_read_writes =
                    Some(native.with_input_phase(tx, ledger, None, |material| {
                        current.finish_with_audit_acks(material, &audit_events, |visit| {
                            for event in &audit_events {
                                visit(super::current_read_writes::CurrentFamily::Event, &event.id)?;
                            }
                            Ok(())
                        })
                    })?);
            }
            if let Some(native) = native_input {
                native.finish_delta_tx(tx, &mut value.input_delta, ledger, None)?;
            }
            let applied = value.agent_state.is_some()
                || before != canonical_marker_tx(tx, agent, id, family)?
                || before_index != index_frontier_tx(tx, agent)?
                || !audit_events.is_empty();
            let mut receipt_events = super::transitions::AuditEventAck::clone_batch(&audit_events)?;
            let outcome = InputMutationOutcome {
                result: InputMutationResult::Evidence {
                    family: result_family,
                    id: id.to_owned(),
                    actual_seq,
                    created_event_seq,
                    created_at: match &value.record {
                        PreparedEvidenceValue::BriefCreated(actual) => {
                            Some(actual.brief.created_at)
                        }
                        _ => None,
                    },
                },
                applied,
                delivery_receipt: None,
                scopes: prepared
                    .descriptor
                    .scopes()
                    .iter()
                    .map(|reserved_scope| {
                        let primary = reserved_scope == scope;
                        InputMutationScopeOutcome {
                            install_keys: super::input_recovery_install::InputMutationInstallKeys {
                                work_items: if primary {
                                    result_work_ids.clone()
                                } else {
                                    Default::default()
                                },
                                ..Default::default()
                            },
                            scope: reserved_scope.clone(),
                            audit_events: if primary {
                                std::mem::take(&mut receipt_events)
                            } else {
                                Vec::new()
                            },
                            // Matches the original standalone runtime installation.
                            notify_memory_index: primary,
                            notify_scheduler: primary,
                        }
                    })
                    .collect(),
            };
            let receipt =
                insert_input_mutation_receipt_tx(tx, &prepared.descriptor, &outcome, Utc::now())?;
            Ok(InputMutationExecution::Committed { value, receipt })
        })
    }
}

fn index_frontier_tx(tx: &Transaction<'_>, agent: &str) -> Result<i64> {
    Ok(tx.query_row(
        "SELECT COALESCE(MAX(change_seq),0) FROM runtime_index_outbox WHERE agent_id=?1",
        [agent],
        |r| r.get(0),
    )?)
}
/// Small persisted metadata only; never read a huge Tool stdout a second time.
fn canonical_marker_tx(
    tx: &Transaction<'_>,
    agent: &str,
    id: &str,
    family: Family,
) -> Result<Option<String>> {
    let sql=match family {
        Family::Message=>"SELECT content_hash FROM messages WHERE agent_id=?1 AND evidence_id=?2",
        Family::Transcript=>"SELECT content_hash FROM transcript_entries WHERE agent_id=?1 AND evidence_id=?2",
        Family::ToolExecution=>"SELECT content_hash FROM tool_executions WHERE agent_id=?1 AND evidence_id=?2",
        Family::Brief=>"SELECT content_hash FROM briefs WHERE agent_id=?1 AND evidence_id=?2",
        Family::Turn=>"SELECT CAST(summary_revision AS TEXT) || ':' || CAST(detail_revision AS TEXT) || ':' || CAST(result_settled AS TEXT) FROM conversation_turn_revisions WHERE agent_id=?1 AND turn_id=?2",
        _=>anyhow::bail!("unsupported prepared evidence marker"),
    };
    Ok(tx
        .query_row(sql, params![agent, id], |r| r.get::<_, Option<String>>(0))
        .optional()?
        .flatten())
}

/// Host callbacks accept only these frozen original transaction families. They
/// cannot hide arbitrary State/queue writes or an opaque result in a closure.
#[derive(Debug)]
pub(crate) enum PreparedExternalInputEvidence {
    BriefCreated(PreparedInputEvidence),
    Retention(PreparedInputRetention),
}
impl PreparedExternalInputEvidence {
    pub(crate) fn brief_created(prepared: PreparedInputEvidence) -> Result<Self> {
        ensure!(
            matches!(
                prepared.command.as_ref(),
                EvidenceCommand::BriefCreated { .. }
            ),
            "Host notice requires its original Brief+created-event transaction"
        );
        Ok(Self::BriefCreated(prepared))
    }
    pub(crate) fn descriptor(&self) -> &PreparedInputMutation {
        match self {
            Self::BriefCreated(p) => p.descriptor(),
            Self::Retention(p) => p.descriptor(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PreparedInputRetention {
    descriptor: PreparedInputMutation,
    policy: RuntimeDbRetentionPolicy,
    now: DateTime<Utc>,
}
impl PreparedInputRetention {
    pub(crate) fn new(
        scopes: Vec<InputEvidenceScope>,
        nonce: String,
        at: DateTime<Utc>,
        policy: RuntimeDbRetentionPolicy,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        let policy = policy.validate()?;
        ensure!(
            policy.enabled,
            "disabled retention has no resident mutation group"
        );
        let descriptor = PreparedInputMutation::new(
            InputMutationFamily::Retention,
            nonce,
            scopes,
            canonical_command_digest(&serde_json::json!({"policy":policy,"now":now}))?,
            at,
        )?;
        Ok(Self {
            descriptor,
            policy,
            now,
        })
    }
    pub(crate) fn descriptor(&self) -> &PreparedInputMutation {
        &self.descriptor
    }
}

#[derive(Debug)]
pub(crate) enum PreparedExternalInputValue {
    BriefCreated(InputEvidenceCommit<PreparedEvidenceValue>),
    Retention(ScopedRuntimeDbRetentionCommit),
}
impl RuntimeDb {
    #[cfg(test)]
    pub(crate) fn commit_prepared_external_input_evidence(
        &self,
        prepared: &PreparedExternalInputEvidence,
    ) -> Result<InputMutationExecution<PreparedExternalInputValue>> {
        self.commit_prepared_external_input_evidence_with_work_request(
            prepared,
            &super::work_snapshot::WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 65_536,
                byte_limit: 64 * 1024 * 1024,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn commit_prepared_external_input_evidence_with_work_request(
        &self,
        prepared: &PreparedExternalInputEvidence,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
    ) -> Result<InputMutationExecution<PreparedExternalInputValue>> {
        self.commit_prepared_external_input_evidence_with_budgets(prepared, work_request, None)
    }

    pub(crate) fn commit_prepared_external_input_evidence_with_task_budget(
        &self,
        prepared: &PreparedExternalInputEvidence,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        task_request: &super::task_snapshot::TaskDomainLoadRequest,
        ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    ) -> Result<InputMutationExecution<PreparedExternalInputValue>> {
        self.commit_prepared_external_input_evidence_with_budgets(
            prepared,
            work_request,
            Some((task_request, ledger)),
        )
    }

    fn commit_prepared_external_input_evidence_with_budgets(
        &self,
        prepared: &PreparedExternalInputEvidence,
        work_request: &super::work_snapshot::WorkDomainLoadRequest,
        task_budget: Option<(
            &super::task_snapshot::TaskDomainLoadRequest,
            &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        )>,
    ) -> Result<InputMutationExecution<PreparedExternalInputValue>> {
        match prepared {
            PreparedExternalInputEvidence::BriefCreated(prepared) => Ok(
                match self
                    .evidence()
                    .commit_prepared_input_evidence_with_budgets(
                        prepared,
                        work_request,
                        task_budget,
                        None,
                        None,
                    )? {
                    InputMutationExecution::Committed { value, receipt } => {
                        InputMutationExecution::Committed {
                            value: PreparedExternalInputValue::BriefCreated(value),
                            receipt,
                        }
                    }
                    InputMutationExecution::Replayed {
                        receipt,
                        _material_budget_views,
                    } => InputMutationExecution::Replayed {
                        receipt,
                        _material_budget_views,
                    },
                },
            ),
            PreparedExternalInputEvidence::Retention(prepared) => self.transaction_with_context(
                RuntimeDbWriteContext::sync("runtime_db.retention.scoped", "retained_evidence"),
                |tx| {
                    for scope in prepared.descriptor.scopes() {
                        validate_input_evidence_scope_tx(tx, scope, &scope.agent_id)?;
                    }
                    if let Some(receipt) =
                        lookup_input_mutation_receipt_tx(tx, &prepared.descriptor)?
                    {
                        return Ok(InputMutationExecution::Replayed {
                            receipt,
                            _material_budget_views: Vec::new(),
                        });
                    }
                    // Recheck actual reservation inventory only for a new command;
                    // previously pruned rows cannot invalidate exact original replay.
                    let completion = prepared
                        .descriptor
                        .scopes()
                        .iter()
                        .map(|scope| {
                            super::work_snapshot::prepare_work_completion_write_tx(
                                tx,
                                scope,
                                None,
                                work_request,
                            )
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let mut value = self.commit_retention_pass_in_scopes_tx(
                        tx,
                        &prepared.policy,
                        prepared.now,
                        prepared.descriptor.scopes(),
                    )?;
                    for (scope, completion) in prepared.descriptor.scopes().iter().zip(completion) {
                        if let Some(delta) = super::work_snapshot::finish_work_completion_write_tx(
                            tx,
                            completion,
                            &Default::default(),
                            work_request,
                        )? {
                            value.work_deltas.push((scope.clone(), delta));
                        }
                    }
                    if let Some((task_request, ledger)) = task_budget {
                        for (scope, input) in &value.input_deltas {
                            if let Some((delta, guards)) =
                                super::task_snapshot::register_task_source_metadata_tx(
                                    tx,
                                    scope,
                                    input,
                                    task_request,
                                    ledger,
                                    None,
                                )?
                            {
                                value.task_deltas.push((scope.clone(), delta, guards));
                            }
                        }
                    }
                    let scopes = prepared
                        .descriptor
                        .scopes()
                        .iter()
                        .map(|scope| {
                            let affected = value.input_deltas.iter().any(|(s, _)| s == scope);
                            InputMutationScopeOutcome {
                                install_keys: Default::default(),
                                scope: scope.clone(),
                                audit_events: Vec::new(),
                                notify_memory_index: affected,
                                notify_scheduler: affected,
                            }
                        })
                        .collect();
                    let report = &value.committed.report;
                    let applied = report.audit_events.deleted_rows > 0
                        || report.transcript_entries.deleted_rows > 0
                        || report.tool_executions.deleted_rows > 0;
                    let outcome = InputMutationOutcome {
                        result: InputMutationResult::Retention {
                            report: Box::new(report.clone()),
                        },
                        applied,
                        delivery_receipt: None,
                        scopes,
                    };
                    let receipt = insert_input_mutation_receipt_tx(
                        tx,
                        &prepared.descriptor,
                        &outcome,
                        Utc::now(),
                    )?;
                    Ok(InputMutationExecution::Committed {
                        value: PreparedExternalInputValue::Retention(value),
                        receipt,
                    })
                },
            ),
        }
    }
}

#[cfg(test)]
#[path = "prepared_input_evidence_tests.rs"]
mod tests;
