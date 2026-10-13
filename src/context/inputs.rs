//! Typed read capability for explicit context acquisition. No renderer receives it.

use anyhow::{ensure, Result};

#[path = "inputs/work.rs"]
mod work;
pub(crate) use work::{selected_completion_brief_ids, work_item as captured_work_item};

use crate::{
    domain::execution_protocol::ExecutionProtocolState,
    runtime::resident_input_evidence::{
        project_tool_record, CapturedInputEvidence, InputEvidenceFamily as Family,
        InputEvidenceKey as Key, InputWindowSelector, PreparedInputEvidence,
        QualifiedInputEvidence, ToolContextProjection,
    },
    runtime::resident_work::CapturedWork,
    runtime_db::{
        context_inputs::{
            prepare_admitted_settlement_inputs, AdmittedSettlementInput, CompletionBriefInput,
            ContextDurableRead,
        },
        task_result_settlement::TaskResultSettlementRecord,
    },
    storage::{AppStorage, WorkQueueReadModel},
    types::{
        AgentState, BriefRecord, DeliverySummaryRecord, ExternalTriggerRecord, MessageEnvelope,
        TaskRecord, TranscriptEntry, TurnOwner, TurnRecord, WaitConditionRecord, WorkItemRecord,
    },
};

/// Acquisition-only typed reads. Implementations cannot provide a database,
/// connection factory, write operation, resource access or renderer fallback.
pub(crate) trait ContextInputs {
    fn input_budget(
        &self,
    ) -> Option<(
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        crate::runtime::resident_input_evidence::InputEvidenceScope,
    )> {
        None
    }
    fn anchor_messages(&self) -> Result<Vec<MessageEnvelope>>;
    fn message_by_id(&self, id: &str) -> Result<Option<MessageEnvelope>>;
    /// Canonical input assignment from the captured read view; Message.turn_id is only a hint.
    fn assigned_turn_for_message(&self, id: &str) -> Result<Option<String>>;
    fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>>;
    fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>>;
    /// Exact latest-result head and frozen optional sources from the Work domain.
    fn latest_completion_brief_for_work_item(
        &self,
        id: &str,
    ) -> Result<Option<CompletionBriefInput>>;
    fn tools(&self, limit: usize) -> Result<Vec<ToolContextProjection>>;
    fn tool_by_id(&self, id: &str) -> Result<Option<ToolContextProjection>>;
    fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>>;
    fn turns_for_owner(&self, owner: &TurnOwner, limit: usize) -> Result<Vec<TurnRecord>>;
    fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>>;
    fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>>;
    fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>>;
    fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>>;
    fn execution_protocol_state(&self, agent_id: &str) -> Result<Option<ExecutionProtocolState>>;
    fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>>;
    fn work_queue(&self) -> Result<WorkQueueReadModel>;
    fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>>;
    fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>>;
    /// Existing active_for_agent ordering, unbounded so count and display share a view.
    fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>>;
    fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>>;
    /// D5 settlement aggregate owns these explicit Message/reply SQL reads.
    fn admitted_settlement_inputs(
        &self,
        agent_id: &str,
        activation_id: &str,
        current_message_id: &str,
        reply_limit: usize,
    ) -> Result<Vec<AdmittedSettlementInput>>;
    fn admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>>;
}

/// Compatibility adapter only. The owner-coherent path uses ContextDurableRead;
/// this legacy adapter intentionally retains its current multi-connection reads.
impl ContextInputs for AppStorage {
    fn anchor_messages(&self) -> Result<Vec<MessageEnvelope>> {
        self.read_all_messages()
    }
    fn message_by_id(&self, id: &str) -> Result<Option<MessageEnvelope>> {
        self.read_message_by_id(id)
    }
    fn assigned_turn_for_message(&self, id: &str) -> Result<Option<String>> {
        let Some(message) = self.read_message_by_id(id)? else {
            return Ok(None);
        };
        let Some(db) = self.runtime_db()? else {
            return Ok(None);
        };
        let mut connection = db.connection()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let assigned =
            ContextDurableRead::new(&tx, &message.agent_id).assigned_turn_for_message(id)?;
        tx.commit()?;
        Ok(assigned)
    }
    fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        self.read_recent_briefs(limit)
    }
    fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>> {
        self.read_brief_by_id(id)
    }
    fn latest_completion_brief_for_work_item(
        &self,
        id: &str,
    ) -> Result<Option<CompletionBriefInput>> {
        if let Some(agent_id) = self.current_agent_id()? {
            if let Some(db) = self.runtime_db()? {
                let mut connection = db.connection()?;
                let transaction = connection
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
                let inputs = ContextDurableRead::new(&transaction, &agent_id);
                let completion = inputs.latest_completion_brief_for_work_item(id)?;
                transaction.commit()?;
                return Ok(completion);
            }
        }
        // Global legacy fixtures without a bound agent retain their original
        // read semantics. Loaded runtimes never enter this compatibility path.
        let brief = self
            .read_recent_briefs(usize::MAX)?
            .into_iter()
            .rev()
            .find(|brief| {
                brief.work_item_id.as_deref() == Some(id)
                    && brief.kind == crate::types::BriefKind::Result
                    && !brief.text.is_empty()
            });
        let Some(brief) = brief else { return Ok(None) };
        let content_source = match &brief.content_source {
            crate::types::BriefContentSource::TranscriptEntry { entry_id, .. } => {
                self.read_transcript_entry_by_id(entry_id)?
            }
            _ => None,
        };
        let source_message = match &content_source {
            Some(entry) if entry.kind == crate::types::TranscriptEntryKind::IncomingMessage => {
                entry
                    .related_message_id
                    .as_deref()
                    .map(|id| self.read_message_by_id(id))
                    .transpose()?
                    .flatten()
            }
            _ => None,
        };
        Ok(Some(CompletionBriefInput {
            brief,
            content_source,
            source_message,
        }))
    }
    fn tools(&self, limit: usize) -> Result<Vec<ToolContextProjection>> {
        self.read_recent_tool_executions(limit)?
            .iter()
            .map(project_tool_record)
            .collect()
    }
    fn tool_by_id(&self, id: &str) -> Result<Option<ToolContextProjection>> {
        self.read_tool_execution_by_id(id)?
            .as_ref()
            .map(project_tool_record)
            .transpose()
    }
    fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        self.read_recent_turns(limit)
    }
    fn turns_for_owner(&self, owner: &TurnOwner, limit: usize) -> Result<Vec<TurnRecord>> {
        self.read_recent_turns_for_owner(owner, limit)
    }
    fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>> {
        self.read_turn_by_id(id)
    }
    fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        self.read_recent_transcript(limit)
    }
    fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>> {
        self.read_transcript_for_turns(ids)
    }
    fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>> {
        self.read_transcript_entry_by_id(id)
    }
    fn execution_protocol_state(&self, agent_id: &str) -> Result<Option<ExecutionProtocolState>> {
        let Some(db) = self.runtime_db()? else {
            return Ok(None);
        };
        db.transitions()
            .load_execution_protocol_state_if_initialized(agent_id)
    }
    fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        self.active_wait_conditions_for_agent(agent_id)
    }
    fn work_queue(&self) -> Result<WorkQueueReadModel> {
        self.work_queue_prompt_projection()
    }
    fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>> {
        self.latest_work_item(id)
    }
    fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        self.latest_external_triggers()
    }
    fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>> {
        self.latest_active_task_records_for_agent(agent_id, usize::MAX)
    }
    fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>> {
        self.read_recent_delivery_summaries(limit)
    }
    fn admitted_settlement_inputs(
        &self,
        agent_id: &str,
        activation_id: &str,
        current_message_id: &str,
        reply_limit: usize,
    ) -> Result<Vec<AdmittedSettlementInput>> {
        prepare_admitted_settlement_inputs(
            self.admitted_settlements(agent_id, activation_id)?,
            current_message_id,
            reply_limit,
            |id| self.read_message_by_id(id),
        )
    }
    fn admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        let Some(db) = self.runtime_db()? else {
            return Ok(Vec::new());
        };
        db.task_result_settlements()
            .admitted_for_activation(agent_id, activation_id)
    }
}

// All delegated methods are inherent, same-connection reads supplied by the
// narrow runtime_db facade. No AppStorage fallback is reachable from this impl.
impl ContextInputs for ContextDurableRead<'_> {
    fn anchor_messages(&self) -> Result<Vec<MessageEnvelope>> {
        ContextDurableRead::anchor_messages(self)
    }
    fn message_by_id(&self, id: &str) -> Result<Option<MessageEnvelope>> {
        ContextDurableRead::message_by_id(self, id)
    }
    fn assigned_turn_for_message(&self, id: &str) -> Result<Option<String>> {
        ContextDurableRead::assigned_turn_for_message(self, id)
    }
    fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        ContextDurableRead::briefs(self, limit)
    }
    fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>> {
        ContextDurableRead::brief_by_id(self, id)
    }
    fn latest_completion_brief_for_work_item(
        &self,
        id: &str,
    ) -> Result<Option<CompletionBriefInput>> {
        ContextDurableRead::latest_completion_brief_for_work_item(self, id)
    }
    fn tools(&self, limit: usize) -> Result<Vec<ToolContextProjection>> {
        ContextDurableRead::tools(self, limit)?
            .iter()
            .map(project_tool_record)
            .collect()
    }
    fn tool_by_id(&self, id: &str) -> Result<Option<ToolContextProjection>> {
        ContextDurableRead::tool_by_id(self, id)?
            .as_ref()
            .map(project_tool_record)
            .transpose()
    }
    fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        ContextDurableRead::turns(self, limit)
    }
    fn turns_for_owner(&self, owner: &TurnOwner, limit: usize) -> Result<Vec<TurnRecord>> {
        ContextDurableRead::turns_for_owner(self, owner, limit)
    }
    fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>> {
        ContextDurableRead::turn_by_id(self, id)
    }
    fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        ContextDurableRead::transcript(self, limit)
    }
    fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>> {
        ContextDurableRead::transcript_for_turns(self, ids)
    }
    fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>> {
        ContextDurableRead::transcript_by_id(self, id)
    }
    fn execution_protocol_state(&self, agent_id: &str) -> Result<Option<ExecutionProtocolState>> {
        ContextDurableRead::execution_protocol_state(self, agent_id)
    }
    fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        ContextDurableRead::active_waits(self, agent_id)
    }
    fn work_queue(&self) -> Result<WorkQueueReadModel> {
        ContextDurableRead::work_queue(self)
    }
    fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>> {
        ContextDurableRead::work_item(self, id)
    }
    fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        ContextDurableRead::external_triggers(self)
    }
    fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>> {
        ContextDurableRead::active_tasks(self, agent_id)
    }
    fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>> {
        ContextDurableRead::delivery_summaries(self, limit)
    }
    fn admitted_settlement_inputs(
        &self,
        agent_id: &str,
        activation_id: &str,
        current_message_id: &str,
        reply_limit: usize,
    ) -> Result<Vec<AdmittedSettlementInput>> {
        ContextDurableRead::admitted_settlement_inputs(
            self,
            agent_id,
            activation_id,
            current_message_id,
            reply_limit,
        )
    }
    fn admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        ContextDurableRead::admitted_settlements(self, agent_id, activation_id)
    }
}

/// Migrated input/evidence and Work reads use selected resident allocations.
/// Remaining Task/Timer/Trigger adapters explicitly borrow one durable view.
pub(crate) struct ResidentContextInputs<'a, 'db> {
    durable: &'a ContextDurableRead<'db>,
    captured: &'a CapturedInputEvidence,
    work: &'a CapturedWork,
    state: &'a AgentState,
    triggers: &'a [ExternalTriggerRecord],
}
impl<'a, 'db> ResidentContextInputs<'a, 'db> {
    pub(crate) fn new(
        durable: &'a ContextDurableRead<'db>,
        captured: &'a CapturedInputEvidence,
        work: &'a CapturedWork,
        state: &'a AgentState,
        triggers: &'a [ExternalTriggerRecord],
    ) -> Result<Self> {
        ensure!(
            durable.agent_id() == captured.scope.agent_id
                && captured.scope.identity_incarnation > 0
                && work.scope == captured.scope
                && state.id == captured.scope.agent_id
                && triggers
                    .iter()
                    .all(|trigger| trigger.target_agent_id == state.id),
            "resident context input scope mismatch"
        );
        Ok(Self {
            durable,
            captured,
            work,
            state,
            triggers,
        })
    }
    fn check_scope(&self, agent_id: &str) -> Result<()> {
        ensure!(
            agent_id == self.captured.scope.agent_id,
            "resident context input scope mismatch"
        );
        Ok(())
    }
    fn by_id<T>(
        &self,
        family: Family,
        id: &str,
        project: impl FnOnce(&PreparedInputEvidence) -> Option<T>,
    ) -> Result<Option<T>> {
        self.captured
            .lookup(&Key::new(family, id))?
            .map(|evidence| {
                ensure!(
                    evidence.locator.scope == self.captured.scope,
                    "resident evidence scope mismatch"
                );
                project(&evidence.record)
                    .ok_or_else(|| anyhow::anyhow!("resident evidence family mismatch"))
            })
            .transpose()
    }
    fn records<T>(
        &self,
        records: Vec<std::sync::Arc<QualifiedInputEvidence>>,
        project: impl Fn(&PreparedInputEvidence) -> Option<T>,
    ) -> Result<Vec<T>> {
        records
            .into_iter()
            .map(|evidence| {
                ensure!(
                    evidence.locator.scope == self.captured.scope,
                    "resident evidence scope mismatch"
                );
                project(&evidence.record)
                    .ok_or_else(|| anyhow::anyhow!("resident evidence family mismatch"))
            })
            .collect()
    }
}
impl ContextInputs for ResidentContextInputs<'_, '_> {
    fn input_budget(
        &self,
    ) -> Option<(
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        crate::runtime::resident_input_evidence::InputEvidenceScope,
    )> {
        self.captured
            .budget
            .clone()
            .map(|budget| (budget, self.captured.scope.clone()))
    }
    fn anchor_messages(&self) -> Result<Vec<MessageEnvelope>> {
        self.records(
            self.captured
                .window(&InputWindowSelector::TrustedOperator, 1)?,
            |record| match record {
                PreparedInputEvidence::Message(value) => Some(value.clone()),
                _ => None,
            },
        )
    }
    fn message_by_id(&self, id: &str) -> Result<Option<MessageEnvelope>> {
        self.by_id(Family::Message, id, |record| match record {
            PreparedInputEvidence::Message(value) => Some(value.clone()),
            _ => None,
        })
    }
    fn assigned_turn_for_message(&self, id: &str) -> Result<Option<String>> {
        let key = Key::new(Family::Message, id);
        let Some(evidence) = self.captured.lookup(&key)? else {
            return Ok(None);
        };
        ensure!(
            evidence.locator.scope == self.captured.scope
                && matches!(&evidence.record, PreparedInputEvidence::Message(message)
                if message.agent_id == self.captured.scope.agent_id && message.id == id),
            "resident Message assignment scope/family mismatch"
        );
        self.captured
            .message_turn_keys
            .get(id)
            .cloned()
            .ok_or_else(|| {
                crate::runtime::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(
                    key,
                )
                .into()
            })
    }
    fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        self.records(
            self.captured
                .window(&InputWindowSelector::RecentBriefs, limit)?,
            |record| match record {
                PreparedInputEvidence::Brief(value) => Some(value.clone()),
                _ => None,
            },
        )
    }
    fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>> {
        self.by_id(Family::Brief, id, |record| match record {
            PreparedInputEvidence::Brief(value) => Some(value.clone()),
            _ => None,
        })
    }
    fn latest_completion_brief_for_work_item(
        &self,
        id: &str,
    ) -> Result<Option<CompletionBriefInput>> {
        work::completion_input(self.work, self.captured, id)
    }
    fn tools(&self, limit: usize) -> Result<Vec<ToolContextProjection>> {
        self.records(
            self.captured
                .window(&InputWindowSelector::RecentTools, limit)?,
            |record| match record {
                PreparedInputEvidence::ToolExecution(value) => Some(value.clone()),
                _ => None,
            },
        )
    }
    fn tool_by_id(&self, id: &str) -> Result<Option<ToolContextProjection>> {
        self.by_id(Family::ToolExecution, id, |record| match record {
            PreparedInputEvidence::ToolExecution(value) => Some(value.clone()),
            _ => None,
        })
    }
    fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        self.records(
            self.captured
                .window(&InputWindowSelector::GlobalTurns, limit)?,
            |record| match record {
                PreparedInputEvidence::Turn(value) => Some(value.clone()),
                _ => None,
            },
        )
    }
    fn turns_for_owner(&self, owner: &TurnOwner, limit: usize) -> Result<Vec<TurnRecord>> {
        if let TurnOwner::AgentLifecycle { agent_id } = owner {
            self.check_scope(agent_id)?
        }
        self.records(
            self.captured.window(
                &InputWindowSelector::OwnerTurns {
                    owner: owner.clone(),
                },
                limit,
            )?,
            |record| match record {
                PreparedInputEvidence::Turn(value) => Some(value.clone()),
                _ => None,
            },
        )
    }
    fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>> {
        self.by_id(Family::Turn, id, |record| match record {
            PreparedInputEvidence::Turn(value) => Some(value.clone()),
            _ => None,
        })
    }
    fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        self.records(
            self.captured
                .window(&InputWindowSelector::RecentTranscript, limit)?,
            |record| match record {
                PreparedInputEvidence::Transcript(value) => Some(value.clone()),
                _ => None,
            },
        )
    }
    fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>> {
        self.records(
            self.captured.transcripts_for_turns(ids)?,
            |record| match record {
                PreparedInputEvidence::Transcript(value) => Some(value.clone()),
                _ => None,
            },
        )
    }
    fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>> {
        self.by_id(Family::Transcript, id, |record| match record {
            PreparedInputEvidence::Transcript(value) => Some(value.clone()),
            _ => None,
        })
    }
    fn execution_protocol_state(&self, agent_id: &str) -> Result<Option<ExecutionProtocolState>> {
        self.check_scope(agent_id)?;
        Ok(Some(self.captured.protocol_state()?))
    }
    fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        self.check_scope(agent_id)?;
        work::active_waits(self.work)
    }
    fn work_queue(&self) -> Result<WorkQueueReadModel> {
        self.work.work_queue(self.state, self.triggers.to_vec())
    }
    fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>> {
        work::work_item(self.work, id)
    }
    fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        // Frozen by the explicit D6 adapter and revalidated in this read view.
        Ok(self.triggers.to_vec())
    }
    fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>> {
        self.durable.active_tasks(agent_id)
    }
    fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>> {
        self.durable.delivery_summaries(limit)
    }
    fn admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.durable.admitted_settlements(agent_id, activation_id)
    }
    fn admitted_settlement_inputs(
        &self,
        agent_id: &str,
        activation_id: &str,
        current_message_id: &str,
        reply_limit: usize,
    ) -> Result<Vec<AdmittedSettlementInput>> {
        self.durable.admitted_settlement_inputs(
            agent_id,
            activation_id,
            current_message_id,
            reply_limit,
        )
    }
}

#[cfg(test)]
mod resident_tests {
    use super::*;
    use crate::{
        runtime::resident_input_evidence::{
            EvidenceLocator, InputEvidenceReadError, InputEvidenceScope, InputWindowCoverage,
        },
        types::{
            AuthorityClass, BriefContentSource, BriefContentSourceRelation, BriefKind, MessageBody,
            MessageKind, MessageOrigin, Priority, ToolExecutionRecord, ToolExecutionStatus,
            TranscriptEntryKind,
        },
    };
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Arc,
    };

    fn empty_work() -> CapturedWork {
        work::tests::capture(Vec::new(), Vec::new(), Vec::new(), BTreeMap::new())
    }
    fn scope() -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: 1,
        }
    }
    fn message(text: &str) -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: text.into() },
        )
    }
    fn evidence(record: PreparedInputEvidence) -> Arc<QualifiedInputEvidence> {
        Arc::new(QualifiedInputEvidence {
            attempt_turn_keys: None,
            locator: EvidenceLocator {
                scope: scope(),
                key: record.key(),
                revision: 1,
                content_digest: "fixture-exact-digest".into(),
            },
            record,
            created_at: "2026-10-13T00:00:00Z".into(),
            order_sequence: Some(1),
            turn_index: None,
            owner: None,
            resident_bytes: 1024,
        })
    }
    fn captured(records: Vec<Arc<QualifiedInputEvidence>>) -> CapturedInputEvidence {
        CapturedInputEvidence {
            selected_completion_brief_ids: Default::default(),
            completion_source_observations: Default::default(),
            recovery_source_absences: Default::default(),
            execution: Default::default(),
            budget: None,
            budget_views: Vec::new(),
            scope: scope(),
            windows: Vec::new(),
            records,
            unavailable: BTreeSet::new(),
            turn_transcript_keys: BTreeMap::new(),
            message_turn_keys: BTreeMap::new(),
        }
    }
    fn window(selector: InputWindowSelector, keys: Vec<Key>, limit: usize) -> InputWindowCoverage {
        InputWindowCoverage {
            selector,
            keys,
            limit,
            is_exhaustive: true,
        }
    }
    fn tool() -> ToolExecutionRecord {
        ToolExecutionRecord {
            id: "tool-large-output".into(),
            agent_id: "default".into(),
            work_item_id: None,
            turn_index: 1,
            turn_id: Some("turn-a".into()),
            tool_name: "ExecCommand".into(),
            created_at: chrono::Utc::now(),
            completed_at: None,
            duration_ms: 1,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({"cmd":"printf hello"}),
            output: serde_json::json!({"exit_code":0,"stdout_preview":"x".repeat(10*1024*1024),"stdout_truncated":true}),
            summary: "completed with large output".into(),
            invocation_surface: None,
        }
    }

    #[test]
    fn migrated_reads_need_no_sql_tables_and_keep_both_history_windows() {
        // Any accidental durable fallback encounters a missing table here.
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        let durable = ContextDurableRead::new(&tx, "default");
        let anchor = message("trusted anchor");
        let turn_a = TurnRecord::new("default", "turn-a", 1);
        let turn_b = TurnRecord::new("default", "turn-b", 2);
        let brief = BriefRecord::new("default", BriefKind::Result, "result", None, None);
        let transcript = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::AssistantRound,
            Some(1),
            None,
            serde_json::json!({"turn_id":"turn-a","blocks":[]}),
        );
        let projection = project_tool_record(&tool()).unwrap();
        let mut resident = captured(vec![
            evidence(PreparedInputEvidence::Message(anchor.clone())),
            evidence(PreparedInputEvidence::Turn(turn_a.clone())),
            evidence(PreparedInputEvidence::Turn(turn_b.clone())),
            evidence(PreparedInputEvidence::Brief(brief.clone())),
            evidence(PreparedInputEvidence::Transcript(transcript.clone())),
            evidence(PreparedInputEvidence::ToolExecution(projection.clone())),
        ]);
        let owner = TurnOwner::WorkItem {
            work_item_id: "owner-a".into(),
        };
        resident.windows = vec![
            window(
                InputWindowSelector::GlobalTurns,
                vec![
                    Key::new(Family::Turn, "turn-b"),
                    Key::new(Family::Turn, "turn-a"),
                ],
                2,
            ),
            window(
                InputWindowSelector::OwnerTurns {
                    owner: owner.clone(),
                },
                vec![Key::new(Family::Turn, "turn-a")],
                2,
            ),
            window(
                InputWindowSelector::RecentBriefs,
                vec![Key::new(Family::Brief, &brief.id)],
                1,
            ),
            window(
                InputWindowSelector::RecentTools,
                vec![Key::new(Family::ToolExecution, &projection.id)],
                1,
            ),
            window(
                InputWindowSelector::RecentTranscript,
                vec![Key::new(Family::Transcript, &transcript.id)],
                1,
            ),
            window(
                InputWindowSelector::TrustedOperator,
                vec![Key::new(Family::Message, &anchor.id)],
                1,
            ),
        ];
        resident.turn_transcript_keys.insert(
            "turn-a".into(),
            BTreeSet::from([Key::new(Family::Transcript, &transcript.id)]),
        );
        resident
            .turn_transcript_keys
            .insert("turn-b".into(), BTreeSet::new());
        resident
            .unavailable
            .insert(Key::new(Family::Message, "pruned-message"));
        resident.message_turn_keys.insert(anchor.id.clone(), None);
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[]).unwrap();
        assert_eq!(inputs.assigned_turn_for_message(&anchor.id).unwrap(), None);
        assert_eq!(inputs.anchor_messages().unwrap(), vec![anchor.clone()]);
        assert_eq!(inputs.message_by_id(&anchor.id).unwrap(), Some(anchor));
        assert_eq!(
            inputs.turns(2).unwrap(),
            vec![turn_a.clone(), turn_b.clone()]
        );
        assert_eq!(
            inputs.turns_for_owner(&owner, 2).unwrap(),
            vec![turn_a.clone()]
        );
        assert_eq!(inputs.turn_by_id("turn-b").unwrap(), Some(turn_b));
        assert_eq!(inputs.briefs(1).unwrap(), vec![brief.clone()]);
        assert_eq!(inputs.brief_by_id(&brief.id).unwrap(), Some(brief));
        assert_eq!(inputs.tools(1).unwrap(), vec![projection.clone()]);
        assert_eq!(inputs.tool_by_id(&projection.id).unwrap(), Some(projection));
        assert_eq!(inputs.transcript(1).unwrap(), vec![transcript.clone()]);
        assert_eq!(
            inputs.transcript_by_id(&transcript.id).unwrap(),
            Some(transcript.clone())
        );
        assert_eq!(
            inputs.transcript_for_turns(&["turn-a".into()]).unwrap(),
            vec![transcript]
        );
        assert!(inputs
            .transcript_for_turns(&["turn-b".into()])
            .unwrap()
            .is_empty());
        assert!(inputs
            .execution_protocol_state("default")
            .unwrap()
            .unwrap()
            .attempts
            .is_empty());
        assert!(inputs.execution_protocol_state("other-agent").is_err());
        assert!(inputs.message_by_id("pruned-message").unwrap().is_none());
        assert!(inputs
            .message_by_id("unprepared")
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
        assert!(inputs
            .turns(3)
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
        assert!(inputs
            .transcript_for_turns(&["uncaptured-turn".into()])
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
        assert!(
            inputs.work_item("remaining-domain").is_err(),
            "uncaptured Work requires explicit preparation"
        );
    }

    #[test]
    fn actual_sql_assignment_owns_history_body_even_when_the_message_hint_names_another_turn() {
        let directory = tempfile::tempdir().unwrap();
        let storage = AppStorage::new_for_test(directory.path()).unwrap();
        let mut source = message("body owned by the canonical assignment");
        source.turn_id = Some("preallocated-hint-with-no-turn".into());
        storage.append_message(&source).unwrap();
        let mut turn = TurnRecord::new("default", "actual-turn", 1);
        turn.owner = Some(TurnOwner::AgentLifecycle {
            agent_id: "default".into(),
        });
        turn.input_message_ids = vec![source.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
        storage.append_turn(&turn).unwrap();
        let db = storage.runtime_db().unwrap().unwrap();
        let mut connection = db.connection().unwrap();
        let tx = connection.transaction().unwrap();
        let durable = ContextDurableRead::new(&tx, "default");
        let source = durable.message_by_id(&source.id).unwrap().unwrap();
        assert_eq!(
            source.turn_id.as_deref(),
            Some("preallocated-hint-with-no-turn")
        );
        assert_eq!(
            durable
                .assigned_turn_for_message(&source.id)
                .unwrap()
                .as_deref(),
            Some("actual-turn")
        );
        let mut messages = Vec::new();
        crate::context::hydrate_recent_turn_references(
            &durable,
            std::slice::from_ref(&turn),
            &mut messages,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(messages, vec![source.clone()]);
        let observations = crate::context::snapshot::HistoryObservations::capture_inputs(
            &durable,
            std::slice::from_ref(&turn),
            &[],
            &messages,
        )
        .unwrap();
        assert!(observations.message_belongs_to_turn(&turn, &source.id));
        assert!(observations.trigger_summary_valid(&turn));
        let mut selected = captured(vec![
            evidence(PreparedInputEvidence::Message(source.clone())),
            evidence(PreparedInputEvidence::Turn(turn.clone())),
        ]);
        selected
            .message_turn_keys
            .insert(source.id.clone(), Some(turn.turn_id.clone()));
        let work = empty_work();
        let state = AgentState::new("default");
        let resident = ResidentContextInputs::new(&durable, &selected, &work, &state, &[]).unwrap();
        assert_eq!(
            resident
                .assigned_turn_for_message(&source.id)
                .unwrap()
                .as_deref(),
            Some("actual-turn")
        );
        let resident_observations = crate::context::snapshot::HistoryObservations::capture_inputs(
            &resident,
            std::slice::from_ref(&turn),
            &[],
            &messages,
        )
        .unwrap();
        tx.commit().unwrap();
        // Pure prompt projection consumes only the already-frozen relation.
        let evidence = crate::context::build_recent_turn_projection_evidence(
            &resident_observations,
            std::slice::from_ref(&turn),
            &messages,
            &[],
            &[],
            crate::projection_eval::ProjectionOwner::Command,
        );
        assert!(evidence
            .iter()
            .any(|entry| entry.reference == format!("message:{}", source.id)
                && entry.owner
                    == crate::projection_eval::ProjectionOwner::AgentLifecycle {
                        agent_id: "default".into()
                    }));
        let projected = crate::context::render_turn_records_with_budget_from_snapshot(
            &resident_observations,
            &[turn],
            &messages,
            &[],
            &[],
            &[],
            &message("next input"),
            None,
            1024,
        )
        .unwrap();
        assert!(projected.contains("body owned by the canonical assignment"));
        assert_eq!(
            messages[0].turn_id.as_deref(),
            Some("preallocated-hint-with-no-turn")
        );
    }

    #[test]
    fn unassigned_hint_is_explicit_none_but_missing_capture_and_foreign_assignment_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let storage = AppStorage::new_for_test(directory.path()).unwrap();
        let mut source = message("accepted input has not started");
        source.turn_id = Some("hint-turn".into());
        storage.append_message(&source).unwrap();
        let db = storage.runtime_db().unwrap().unwrap();
        let mut connection = db.connection().unwrap();
        let tx = connection.transaction().unwrap();
        let durable = ContextDurableRead::new(&tx, "default");
        assert!(durable
            .assigned_turn_for_message(&source.id)
            .unwrap()
            .is_none());
        let mut selected = captured(vec![evidence(PreparedInputEvidence::Message(
            source.clone(),
        ))]);
        let work = empty_work();
        let state = AgentState::new("default");
        let missing = ResidentContextInputs::new(&durable, &selected, &work, &state, &[]).unwrap();
        assert!(missing
            .assigned_turn_for_message(&source.id)
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
        selected.message_turn_keys.insert(source.id.clone(), None);
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &selected, &work, &state, &[]).unwrap();
        assert!(inputs
            .assigned_turn_for_message(&source.id)
            .unwrap()
            .is_none());
        let mut hint_turn = TurnRecord::new("default", "hint-turn", 1);
        hint_turn.input_message_ids = vec![source.id.clone()];
        hint_turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
        let mut hydrated = Vec::new();
        crate::context::hydrate_recent_turn_references(
            &inputs,
            std::slice::from_ref(&hint_turn),
            &mut hydrated,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(
            hydrated.is_empty(),
            "the raw hint was used as an assignment"
        );
        tx.commit().unwrap();
        db.connection().unwrap().execute("INSERT INTO conversation_input_assignments
            (message_id,agent_id,turn_id,revision,assigned_at) VALUES(?1,'other-agent','foreign-turn',1,?2)",
            rusqlite::params![source.id, chrono::Utc::now().to_rfc3339()]).unwrap();
        let tx = connection.transaction().unwrap();
        assert!(ContextDurableRead::new(&tx, "default")
            .assigned_turn_for_message(&source.id)
            .is_err());
    }

    #[test]
    fn unprepared_brief_source_cannot_silently_render_fallback_text() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        let durable = ContextDurableRead::new(&tx, "default");
        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "fallback must not hide unprepared source",
            None,
            None,
        );
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: "not-prepared".into(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        let resident = captured(vec![evidence(PreparedInputEvidence::Brief(brief.clone()))]);
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[]).unwrap();
        let error = crate::context::snapshot::HistoryObservations::capture_inputs(
            &inputs,
            &[],
            &[brief],
            &[],
        )
        .unwrap_err();
        assert!(error.downcast_ref::<InputEvidenceReadError>().is_some());
    }

    #[test]
    fn completion_head_prepares_exact_source_outside_recent_history() {
        let directory = tempfile::tempdir().unwrap();
        let storage = AppStorage::new_for_test(directory.path()).unwrap();
        let source = message("exact incoming body for the cold completion report");
        storage.append_message(&source).unwrap();
        let transcript = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(source.id.clone()),
            serde_json::json!({"text":"stale transcript preview"}),
        );
        storage.append_transcript_entry(&transcript).unwrap();
        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "stale Brief fallback",
            None,
            None,
        );
        brief.work_item_id = Some("cold-work".into());
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: transcript.id.clone(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        storage.append_brief(&brief).unwrap();
        let db = storage.runtime_db().unwrap().unwrap();
        let mut connection = db.connection().unwrap();
        let tx = connection.transaction().unwrap();
        let durable = ContextDurableRead::new(&tx, "default");
        let resident = captured(vec![
            evidence(PreparedInputEvidence::Brief(brief.clone())),
            evidence(PreparedInputEvidence::Transcript(transcript.clone())),
            evidence(PreparedInputEvidence::Message(source.clone())),
        ]);
        let mut record = crate::types::WorkItemRecord::new(
            "default",
            "cold result",
            crate::types::WorkItemState::Completed,
        );
        record.id = "cold-work".into();
        let work = work::tests::capture(
            vec![crate::runtime::resident_work::CanonicalWorkRecord::Work(
                record,
            )],
            Vec::new(),
            Vec::new(),
            BTreeMap::from([(
                "cold-work".into(),
                Some(crate::runtime::resident_work::WorkCompletionRef {
                    brief_id: brief.id.clone(),
                    transcript_entry_id: Some(transcript.id.clone()),
                    source_message_id: Some(source.id.clone()),
                }),
            )]),
        );
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[]).unwrap();
        assert!(
            inputs.briefs(1).is_err(),
            "a focused completion head does not grant a history window"
        );
        let reports = crate::context::latest_completion_report_by_work_item(
            &inputs,
            "default",
            &BTreeSet::from(["cold-work".into()]),
        )
        .unwrap();
        let report = reports.get("cold-work").unwrap();
        assert_eq!(
            report.text,
            "exact incoming body for the cold completion report"
        );
        assert_eq!(report.brief_id.as_deref(), Some(brief.id.as_str()));
        assert_eq!(
            report.source_brief.as_ref().unwrap().content_source,
            brief.content_source
        );
    }

    #[test]
    fn settlement_aggregate_keeps_exclusions_reply_bound_and_original_body() {
        use crate::{
            runtime_db::task_result_settlement::{upsert_pending_tx, TaskResultSettlementState},
            types::{AdmissionContext, MessageDeliverySurface},
        };
        let directory = tempfile::tempdir().unwrap();
        let storage = AppStorage::new_for_test(directory.path()).unwrap();
        let current = message("current input");
        let reply = message("original reply body outside resident history");
        storage.append_message(&current).unwrap();
        storage.append_message(&reply).unwrap();
        let result = |task_id: &str, reply_id: &str| {
            let mut message = MessageEnvelope::new(
                "default",
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: task_id.into(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "task terminal summary".into(),
                },
            );
            message.task_id = Some(task_id.into());
            message.delivery_surface = Some(MessageDeliverySurface::TaskRejoin);
            message.admission_context = Some(AdmissionContext::RuntimeOwned);
            message.metadata = Some(serde_json::json!({"task_id":task_id,
                "task_kind":crate::types::AGENT_MESSAGE_WAIT_TASK_KIND,"task_status":"completed",
                "task_detail":{"reply_content_source":"original_message","message_id":reply_id}}));
            message
        };
        let skip = result("skip-task", &current.id);
        let first = result("first-task", &reply.id);
        let duplicate = result("duplicate-task", &reply.id);
        for message in [&skip, &first, &duplicate] {
            storage.append_message(message).unwrap();
        }
        let db = storage.runtime_db().unwrap().unwrap();
        let mut connection = db.connection().unwrap();
        let tx = connection.transaction().unwrap();
        let at = chrono::Utc::now();
        for (id, message) in [
            ("0-current", &current),
            ("1-skip-reply", &skip),
            ("2-first", &first),
            ("3-duplicate", &duplicate),
        ] {
            let record = TaskResultSettlementRecord {
                result_identity: id.into(),
                agent_id: "default".into(),
                task_id: message
                    .task_id
                    .clone()
                    .unwrap_or_else(|| "current-task".into()),
                message_id: message.id.clone(),
                work_item_id: None,
                rejoin_generation: 1,
                parent_turn_id: "parent-turn".into(),
                task_status: "completed".into(),
                state: TaskResultSettlementState::CallerAdmitted,
                activation_id: Some("activation".into()),
                disposition: None,
                created_at: at,
                updated_at: at,
                admitted_at: Some(at),
                settled_at: None,
                deferred_reason: None,
                deferred_at: None,
                next_recheck_at: None,
            };
            upsert_pending_tx(&tx, &record).unwrap();
        }
        let durable = ContextDurableRead::new(&tx, "default");
        let resident = captured(Vec::new());
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[]).unwrap();
        assert!(inputs.message_by_id(&reply.id).is_err());
        let pending = inputs
            .admitted_settlement_inputs("default", "activation", &current.id, 1)
            .unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].record.message_id, first.id);
        assert_eq!(pending[0].reply.as_ref().unwrap().body, reply.body);
        assert_eq!(pending[1].record.message_id, duplicate.id);
        assert!(
            pending[1].reply.is_none(),
            "reply fetch stays bounded and deduplicated"
        );
    }

    #[test]
    fn tool_context_projection_drops_large_stdout_before_capture_and_projection() {
        let record = tool();
        let projection = project_tool_record(&record).unwrap();
        drop(record);
        let bytes = serde_json::to_vec(&projection).unwrap();
        assert!(bytes.len() < 32 * 1024);
        assert!(!String::from_utf8(bytes).unwrap().contains(&"x".repeat(256)));
        assert!(projection.command.truncated);
        assert!(projection.command.has_output_evidence);
        assert_eq!(
            projection.command.command_preview.as_deref(),
            Some("printf hello")
        );
        let rendered =
            crate::context::render_recent_tool_execution_rollup(&[&projection]).join("\n");
        assert!(rendered.contains("printf hello"));
        assert!(rendered.contains("tool_execution:tool-large-output:output"));
    }
}
