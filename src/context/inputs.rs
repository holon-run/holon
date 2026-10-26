//! Typed read capability for explicit context acquisition. No renderer receives it.

use anyhow::{ensure, Result};
use std::collections::BTreeSet;

#[path = "inputs/tasks.rs"]
mod tasks;
pub(crate) use tasks::pending_task_input_roots;

mod turns;
pub(crate) use turns::ContextHistoryTurn;

mod messages;
pub(crate) use messages::ContextHistoryMessage;

mod transcript;
pub(crate) use transcript::ContextHistoryTranscript;

mod briefs;
pub(crate) use briefs::ContextHistoryBrief;

mod tools;
pub(crate) use tools::ContextHistoryTool;

mod brief_text;
pub(crate) use brief_text::ContextHistoryBriefText;

mod assignments;
pub(crate) use assignments::ContextHistoryAssignments;

#[path = "inputs/work.rs"]
mod work;
pub(crate) use work::{selected_completion_brief_ids, work_item as captured_work_item};

pub(crate) type AdmittedSettlementInput =
    crate::runtime_db::context_inputs::AdmittedSettlementInput<ContextHistoryMessage>;

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
            prepare_admitted_settlement_inputs, CompletionBriefInput, ContextDurableRead,
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
    fn history_message_by_id(&self, id: &str) -> Result<Option<ContextHistoryMessage>> {
        self.message_by_id(id)?
            .map(ContextHistoryMessage::owned)
            .transpose()
    }
    /// Existence-only observation; resident readers need not copy the body.
    fn message_exists(&self, id: &str) -> Result<bool> {
        Ok(self.message_by_id(id)?.is_some())
    }
    /// Canonical input assignment from the captured read view; Message.turn_id is only a hint.
    fn assigned_turn_for_message(&self, id: &str) -> Result<Option<String>>;
    /// Inspect canonical assignment without retaining an owning string.
    fn inspect_assigned_turn<T>(
        &self,
        id: &str,
        inspect: impl FnOnce(Option<&str>) -> T,
    ) -> Result<T> {
        let assignment = self.assigned_turn_for_message(id)?;
        Ok(inspect(assignment.as_deref()))
    }
    fn history_message_assignments(
        &self,
        ids: BTreeSet<String>,
    ) -> Result<std::collections::BTreeMap<String, Option<String>>> {
        ids.into_iter()
            .map(|id| {
                let assignment = self.assigned_turn_for_message(&id)?;
                Ok((id, assignment))
            })
            .collect()
    }
    fn history_assignment_view<'id>(
        &self,
        ids: impl Iterator<Item = &'id str> + Clone,
    ) -> Result<ContextHistoryAssignments> {
        self.history_message_assignments(assignments::selection(ids)?)
            .map(ContextHistoryAssignments::from)
    }
    fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>>;
    fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>>;
    fn history_briefs(&self, limit: usize) -> Result<Vec<ContextHistoryBrief>> {
        self.briefs(limit)?
            .into_iter()
            .map(ContextHistoryBrief::owned)
            .collect()
    }
    fn history_brief_by_id(&self, id: &str) -> Result<Option<ContextHistoryBrief>> {
        self.brief_by_id(id)?
            .map(ContextHistoryBrief::owned)
            .transpose()
    }
    /// Exact latest-result head and frozen optional sources from the Work domain.
    fn latest_completion_brief_for_work_item(
        &self,
        id: &str,
    ) -> Result<Option<CompletionBriefInput<'_>>>;
    fn tools(&self, limit: usize) -> Result<Vec<ToolContextProjection>>;
    fn tool_by_id(&self, id: &str) -> Result<Option<ToolContextProjection>>;
    fn history_tools(&self, limit: usize) -> Result<Vec<ContextHistoryTool>> {
        self.tools(limit)?
            .into_iter()
            .map(ContextHistoryTool::owned)
            .collect()
    }
    fn history_tool_by_id(&self, id: &str) -> Result<Option<ContextHistoryTool>> {
        self.tool_by_id(id)?
            .map(ContextHistoryTool::owned)
            .transpose()
    }
    fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>>;
    fn turns_for_owner(&self, owner: &TurnOwner, limit: usize) -> Result<Vec<TurnRecord>>;
    fn history_turns(
        &self,
        owner: Option<&TurnOwner>,
        message_id: &str,
        limit: usize,
    ) -> Result<Vec<ContextHistoryTurn>> {
        let selected = match owner {
            Some(owner) => self.turns_for_owner(owner, limit.saturating_add(1))?,
            None => self.turns(limit.saturating_add(1))?,
        };
        super::history_turns_without_current_input(selected, message_id, limit)
            .into_iter()
            .map(ContextHistoryTurn::owned)
            .collect()
    }
    fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>>;
    fn history_turn_by_id(&self, id: &str) -> Result<Option<ContextHistoryTurn>> {
        self.turn_by_id(id)?
            .map(ContextHistoryTurn::owned)
            .transpose()
    }
    fn idle_input_batch_for_turn(
        &self,
        id: &str,
    ) -> Result<Option<crate::types::IdleInputBatchRecord>> {
        Ok(self.turn_by_id(id)?.and_then(|turn| turn.idle_input_batch))
    }
    fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>>;
    fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>>;
    /// History consumes only assistant rounds and tool-result references.
    /// Cold readers retain their original full-window behavior.
    fn history_transcript(&self, limit: usize) -> Result<Vec<ContextHistoryTranscript>> {
        self.transcript(limit)?
            .into_iter()
            .map(ContextHistoryTranscript::owned)
            .collect()
    }
    fn history_transcript_for_turns(
        &self,
        ids: &[String],
    ) -> Result<Vec<ContextHistoryTranscript>> {
        self.transcript_for_turns(ids)?
            .into_iter()
            .map(ContextHistoryTranscript::owned)
            .collect()
    }
    fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>>;
    fn resolved_brief_text(&self, brief: &BriefRecord) -> Result<String> {
        super::resolved_brief_text_from_owned_inputs(self, brief)
    }
    fn history_brief_text(&self, brief: &BriefRecord) -> Result<ContextHistoryBriefText> {
        self.resolved_brief_text(brief)
            .map(ContextHistoryBriefText::owned)
    }
    fn execution_protocol_state(&self, agent_id: &str) -> Result<Option<ExecutionProtocolState>>;
    fn execution_attempt_by_id(
        &self,
        agent_id: &str,
        id: &str,
    ) -> Result<Option<crate::domain::execution_protocol::ExecutionAttempt>> {
        Ok(self
            .execution_protocol_state(agent_id)?
            .and_then(|mut state| state.attempts.remove(id)))
    }
    fn current_input_projection(
        &self,
        agent_id: &str,
        message_id: &str,
    ) -> Result<Option<crate::types::IdleInputBatchItem>> {
        Ok(self.execution_protocol_state(agent_id)?.and_then(|state| {
            state
                .open_attempt()
                .filter(|attempt| attempt.source_message_id.as_deref() == Some(message_id))
                .and_then(|attempt| attempt.input_projection.clone())
        }))
    }
    fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>>;
    fn work_queue(&self) -> Result<WorkQueueReadModel>;
    fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>>;
    fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>>;
    /// Existing active_for_agent ordering, unbounded so count and display share a view.
    fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>>;
    fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>>;
    /// Cold adapters keep their independent compatibility policy.
    fn completion_delivery_summaries<'a>(
        &self,
        _work_item_ids: impl Iterator<Item = &'a str>,
    ) -> Result<Vec<DeliverySummaryRecord>> {
        self.delivery_summaries(usize::MAX)
    }
    /// Selected Task settlement facts and exact Message/reply sources share one view.
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
        Ok(self
            .read_latest_trusted_operator_message()?
            .into_iter()
            .collect())
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
    ) -> Result<Option<CompletionBriefInput<'_>>> {
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
            brief: std::borrow::Cow::Owned(brief),
            content_source: content_source.map(std::borrow::Cow::Owned),
            source_message: source_message.map(std::borrow::Cow::Owned),
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
            |id| self.history_message_by_id(id),
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
    ) -> Result<Option<CompletionBriefInput<'_>>> {
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
        )?
        .into_iter()
        .map(|input| {
            Ok(AdmittedSettlementInput {
                record: input.record,
                message: input
                    .message
                    .map(ContextHistoryMessage::owned)
                    .transpose()?,
                reply: input.reply.map(ContextHistoryMessage::owned).transpose()?,
            })
        })
        .collect()
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
/// Remaining Timer/Trigger adapters explicitly borrow one durable view.
pub(crate) struct ResidentContextInputs<'a, 'db> {
    durable: &'a ContextDurableRead<'db>,
    captured: &'a CapturedInputEvidence,
    work: &'a CapturedWork,
    tasks: Option<&'a crate::runtime::resident_task::CapturedTasks>,
    task_projection_guards: tasks::TaskProjectionGuards,
    history_material: std::cell::RefCell<std::sync::Weak<turns::HistoryTurnMaterial>>,
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
            tasks: None,
            task_projection_guards: Default::default(),
            history_material: Default::default(),
            state,
            triggers,
        })
    }
    pub(crate) fn with_tasks(
        mut self,
        tasks: &'a crate::runtime::resident_task::CapturedTasks,
    ) -> Result<Self> {
        ensure!(
            tasks.scope == self.captured.scope,
            "resident Task context scope mismatch"
        );
        self.tasks = Some(tasks);
        Ok(self)
    }
    fn check_scope(&self, agent_id: &str) -> Result<()> {
        ensure!(
            agent_id == self.captured.scope.agent_id,
            "resident context input scope mismatch"
        );
        Ok(())
    }
    fn qualified_by_id(
        &self,
        family: Family,
        id: &str,
    ) -> Result<Option<&std::sync::Arc<QualifiedInputEvidence>>> {
        self.captured
            .lookup_borrowed(family, id)?
            .map(|evidence| {
                ensure!(
                    evidence.locator.scope == self.captured.scope,
                    "resident evidence scope mismatch"
                );
                Ok(evidence)
            })
            .transpose()
    }
    fn record_by_id(&self, family: Family, id: &str) -> Result<Option<&PreparedInputEvidence>> {
        Ok(self
            .qualified_by_id(family, id)?
            .map(|evidence| &evidence.record))
    }
    fn by_id<T>(
        &self,
        family: Family,
        id: &str,
        project: impl FnOnce(&PreparedInputEvidence) -> Option<T>,
    ) -> Result<Option<T>> {
        self.record_by_id(family, id)?
            .map(|record| {
                crate::diagnostics::resource_io::admit_cloned_value(record)?;
                project(record).ok_or_else(|| anyhow::anyhow!("resident evidence family mismatch"))
            })
            .transpose()
    }
    fn history_material(&self) -> Result<std::sync::Arc<turns::HistoryTurnMaterial>> {
        crate::diagnostics::resource_io::check_active_budget()?;
        let mut cached = self.history_material.borrow_mut();
        if let Some(material) = cached.upgrade() {
            return Ok(material);
        }
        let material = turns::HistoryTurnMaterial::capture(self.captured)?;
        *cached = std::sync::Arc::downgrade(&material);
        Ok(material)
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
                crate::diagnostics::resource_io::admit_cloned_value(&evidence.record)?;
                project(&evidence.record)
                    .ok_or_else(|| anyhow::anyhow!("resident evidence family mismatch"))
            })
            .collect()
    }
    fn history_records(
        &self,
        records: Vec<std::sync::Arc<QualifiedInputEvidence>>,
    ) -> Result<Vec<ContextHistoryTranscript>> {
        crate::diagnostics::resource_io::check_active()?;
        let mut entries = Vec::new();
        let mut material = None;
        for evidence in records {
            ensure!(
                evidence.locator.scope == self.captured.scope,
                "resident evidence scope mismatch"
            );
            let PreparedInputEvidence::Transcript(entry) = &evidence.record else {
                anyhow::bail!("resident evidence family mismatch");
            };
            if matches!(
                entry.kind,
                crate::types::TranscriptEntryKind::AssistantRound
                    | crate::types::TranscriptEntryKind::ToolResults
            ) {
                let material = match &material {
                    Some(material) => std::sync::Arc::clone(material),
                    None => {
                        let captured = self.history_material()?;
                        material = Some(captured.clone());
                        captured
                    }
                };
                entries.push(ContextHistoryTranscript::qualified(evidence, material)?);
            }
        }
        Ok(entries)
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
    fn message_exists(&self, id: &str) -> Result<bool> {
        match self.record_by_id(Family::Message, id)? {
            Some(PreparedInputEvidence::Message(_)) => Ok(true),
            Some(_) => anyhow::bail!("resident evidence family mismatch"),
            None => Ok(false),
        }
    }
    fn history_message_by_id(&self, id: &str) -> Result<Option<ContextHistoryMessage>> {
        let Some(evidence) = self.captured.lookup_borrowed(Family::Message, id)? else {
            return Ok(None);
        };
        ensure!(
            evidence.locator.scope == self.captured.scope,
            "resident history Message scope mismatch"
        );
        ContextHistoryMessage::qualified(evidence.clone(), self.history_material()?).map(Some)
    }
    fn history_turn_by_id(&self, id: &str) -> Result<Option<ContextHistoryTurn>> {
        let Some(evidence) = self.qualified_by_id(Family::Turn, id)? else {
            return Ok(None);
        };
        ContextHistoryTurn::qualified(evidence.clone(), self.history_material()?).map(Some)
    }
    fn execution_attempt_by_id(
        &self,
        agent_id: &str,
        id: &str,
    ) -> Result<Option<crate::domain::execution_protocol::ExecutionAttempt>> {
        self.check_scope(agent_id)?;
        self.by_id(Family::Attempt, id, |record| match record {
            PreparedInputEvidence::Attempt(attempt) => Some(attempt.clone()),
            _ => None,
        })
    }
    fn current_input_projection(
        &self,
        agent_id: &str,
        message_id: &str,
    ) -> Result<Option<crate::types::IdleInputBatchItem>> {
        self.check_scope(agent_id)?;
        for evidence in &self.captured.records {
            let PreparedInputEvidence::Attempt(attempt) = &evidence.record else {
                continue;
            };
            if attempt.state == crate::domain::execution_protocol::ExecutionAttemptState::Open
                && attempt.source_message_id.as_deref() == Some(message_id)
            {
                crate::diagnostics::resource_io::admit_cloned_value(&attempt.input_projection)?;
                return Ok(attempt.input_projection.clone());
            }
        }
        Ok(None)
    }
    fn assigned_turn_for_message(&self, id: &str) -> Result<Option<String>> {
        let Some(assignment) = assignments::borrowed(self, id)? else {
            return Ok(None);
        };
        crate::diagnostics::resource_io::admit_cloned_value(&Some(assignment))?;
        Ok(assignment.clone())
    }
    fn inspect_assigned_turn<T>(
        &self,
        id: &str,
        inspect: impl FnOnce(Option<&str>) -> T,
    ) -> Result<T> {
        let assignment = assignments::borrowed(self, id)?;
        Ok(inspect(assignment.and_then(|value| value.as_deref())))
    }
    fn history_message_assignments(
        &self,
        ids: BTreeSet<String>,
    ) -> Result<std::collections::BTreeMap<String, Option<String>>> {
        assignments::capture(self, ids)
    }
    fn history_assignment_view<'id>(
        &self,
        ids: impl Iterator<Item = &'id str> + Clone,
    ) -> Result<ContextHistoryAssignments> {
        assignments::capture_view(self, ids)
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
    fn history_briefs(&self, limit: usize) -> Result<Vec<ContextHistoryBrief>> {
        let records = self
            .captured
            .window(&InputWindowSelector::RecentBriefs, limit)?;
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let material = self.history_material()?;
        records
            .into_iter()
            .map(|evidence| {
                ensure!(
                    evidence.locator.scope == self.captured.scope,
                    "resident history Brief scope mismatch"
                );
                ContextHistoryBrief::qualified(evidence, material.clone())
            })
            .collect()
    }
    fn history_brief_by_id(&self, id: &str) -> Result<Option<ContextHistoryBrief>> {
        let Some(evidence) = self.captured.lookup_borrowed(Family::Brief, id)? else {
            return Ok(None);
        };
        ensure!(
            evidence.locator.scope == self.captured.scope,
            "resident history Brief scope mismatch"
        );
        ContextHistoryBrief::qualified(evidence.clone(), self.history_material()?).map(Some)
    }
    fn latest_completion_brief_for_work_item(
        &self,
        id: &str,
    ) -> Result<Option<CompletionBriefInput<'_>>> {
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
    fn history_tools(&self, limit: usize) -> Result<Vec<ContextHistoryTool>> {
        let records = self
            .captured
            .window(&InputWindowSelector::RecentTools, limit)?;
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let material = self.history_material()?;
        records
            .into_iter()
            .map(|evidence| {
                ensure!(
                    evidence.locator.scope == self.captured.scope,
                    "resident history ToolExecution scope mismatch"
                );
                ContextHistoryTool::qualified(evidence, material.clone())
            })
            .collect()
    }
    fn history_tool_by_id(&self, id: &str) -> Result<Option<ContextHistoryTool>> {
        let Some(evidence) = self.qualified_by_id(Family::ToolExecution, id)? else {
            return Ok(None);
        };
        ContextHistoryTool::qualified(evidence.clone(), self.history_material()?).map(Some)
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
        crate::diagnostics::resource_io::admit_cloned_value(owner)?;
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
    fn idle_input_batch_for_turn(
        &self,
        id: &str,
    ) -> Result<Option<crate::types::IdleInputBatchRecord>> {
        crate::diagnostics::resource_io::check_active_budget()?;
        let Some(record) = self.record_by_id(Family::Turn, id)? else {
            return Ok(None);
        };
        let PreparedInputEvidence::Turn(turn) = record else {
            anyhow::bail!("resident evidence family mismatch")
        };
        let Some(batch) = &turn.idle_input_batch else {
            return Ok(None);
        };
        crate::diagnostics::resource_io::admit_cloned_value(batch)?;
        Ok(Some(batch.clone()))
    }
    fn history_turns(
        &self,
        owner: Option<&TurnOwner>,
        message_id: &str,
        limit: usize,
    ) -> Result<Vec<ContextHistoryTurn>> {
        let selector = match owner {
            Some(owner) => {
                if let TurnOwner::AgentLifecycle { agent_id } = owner {
                    self.check_scope(agent_id)?;
                }
                crate::diagnostics::resource_io::admit_cloned_value(owner)?;
                InputWindowSelector::OwnerTurns {
                    owner: owner.clone(),
                }
            }
            None => InputWindowSelector::GlobalTurns,
        };
        let mut records = self.captured.window(&selector, limit.saturating_add(1))?;
        // Current input has its own section. Filter the frozen window before
        // any owning projection; an excluded live batch can be very large.
        for evidence in &records {
            ensure!(
                evidence.locator.scope == self.captured.scope
                    && matches!(evidence.record, PreparedInputEvidence::Turn(_)),
                "resident history Turn scope/family mismatch"
            );
        }
        records.retain(|evidence| match &evidence.record {
            PreparedInputEvidence::Turn(turn) => {
                turn.terminal.is_some() || !turn.input_message_ids.iter().any(|id| id == message_id)
            }
            _ => unreachable!(),
        });
        records.drain(..records.len().saturating_sub(limit));
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let material = self.history_material()?;
        records
            .into_iter()
            .map(|evidence| ContextHistoryTurn::qualified(evidence, material.clone()))
            .collect()
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
    fn history_transcript(&self, limit: usize) -> Result<Vec<ContextHistoryTranscript>> {
        self.history_records(
            self.captured
                .window(&InputWindowSelector::RecentTranscript, limit)?,
        )
    }
    fn history_transcript_for_turns(
        &self,
        ids: &[String],
    ) -> Result<Vec<ContextHistoryTranscript>> {
        self.history_records(self.captured.transcripts_for_turns(ids)?)
    }
    fn history_brief_text(&self, brief: &BriefRecord) -> Result<ContextHistoryBriefText> {
        if let crate::types::BriefContentSource::TranscriptEntry { entry_id, .. } =
            &brief.content_source
        {
            if let Some(evidence) = self.qualified_by_id(Family::Transcript, entry_id)? {
                let PreparedInputEvidence::Transcript(entry) = &evidence.record else {
                    anyhow::bail!("resident evidence family mismatch");
                };
                let message = if entry.kind == crate::types::TranscriptEntryKind::IncomingMessage {
                    match entry.related_message_id.as_deref() {
                        Some(id) => self.qualified_by_id(Family::Message, id)?,
                        None => None,
                    }
                } else {
                    None
                };
                if let Some(message) = message {
                    ensure!(
                        matches!(message.record, PreparedInputEvidence::Message(_)),
                        "resident evidence family mismatch"
                    );
                }
                let source = message.unwrap_or(evidence);
                if brief_text::borrowed_text(&source.record).is_some() {
                    return ContextHistoryBriefText::qualified(
                        source.clone(),
                        self.history_material()?,
                    );
                }
            }
        }
        self.resolved_brief_text(brief)
            .map(ContextHistoryBriefText::owned)
    }
    fn resolved_brief_text(&self, brief: &BriefRecord) -> Result<String> {
        let entry = match &brief.content_source {
            crate::types::BriefContentSource::TranscriptEntry { entry_id, .. } => {
                match self.record_by_id(Family::Transcript, entry_id)? {
                    Some(PreparedInputEvidence::Transcript(entry)) => Some(entry),
                    Some(_) => anyhow::bail!("resident evidence family mismatch"),
                    None => None,
                }
            }
            _ => None,
        };
        let message = match entry {
            Some(entry) if entry.kind == crate::types::TranscriptEntryKind::IncomingMessage => {
                match entry.related_message_id.as_deref() {
                    Some(id) => match self.record_by_id(Family::Message, id)? {
                        Some(PreparedInputEvidence::Message(message)) => Some(message),
                        Some(_) => anyhow::bail!("resident evidence family mismatch"),
                        None => None,
                    },
                    None => None,
                }
            }
            _ => None,
        };
        super::resolved_brief_text_from_values(brief, entry, message)
    }
    fn execution_protocol_state(&self, agent_id: &str) -> Result<Option<ExecutionProtocolState>> {
        self.check_scope(agent_id)?;
        crate::diagnostics::resource_io::admit_cloned_value(&agent_id)?;
        for evidence in &self.captured.records {
            if matches!(
                &evidence.record,
                PreparedInputEvidence::Attempt(_)
                    | PreparedInputEvidence::Outcome(_)
                    | PreparedInputEvidence::WorkExecution { .. }
            ) {
                crate::diagnostics::resource_io::admit_cloned_value(&evidence.record)?;
            }
        }
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
        self.resident_active_tasks(agent_id)
    }
    fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>> {
        self.durable.delivery_summaries(limit)
    }
    fn completion_delivery_summaries<'a>(
        &self,
        work_item_ids: impl Iterator<Item = &'a str>,
    ) -> Result<Vec<DeliverySummaryRecord>> {
        self.durable.completion_delivery_summaries(work_item_ids)
    }
    fn admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.resident_admitted_settlements(agent_id, activation_id)
    }
    fn admitted_settlement_inputs(
        &self,
        agent_id: &str,
        activation_id: &str,
        current_message_id: &str,
        reply_limit: usize,
    ) -> Result<Vec<AdmittedSettlementInput>> {
        self.resident_admitted_settlement_inputs(
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
            query: None,
            query_acquisition: None,
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
            message_turn_keys: Default::default(),
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

    #[test]
    fn resident_history_filters_after_selection_and_preserves_rendered_sequences() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let ignored = serde_json::json!(vec![serde_json::json!({"unused":"body"}); 4096]);
        let mut tool = project_tool_record(&tool())?;
        tool.turn_id = Some("turn-a".into());
        let mut full = Vec::new();
        for kind in [
            TranscriptEntryKind::IncomingMessage,
            TranscriptEntryKind::AssistantRound,
            TranscriptEntryKind::ToolResults,
            TranscriptEntryKind::RuntimeFailure,
            TranscriptEntryKind::ContinuationPrompt,
            TranscriptEntryKind::SubagentPrompt,
            TranscriptEntryKind::SubagentAssistantRound,
        ] {
            let data = match kind {
                TranscriptEntryKind::AssistantRound => serde_json::json!({
                    "turn_id":"turn-a", "work_item_id":"work-a",
                    "blocks":[{"type":"text","text":" first "},{"Text":{"text":"second"}}]
                }),
                TranscriptEntryKind::ToolResults => serde_json::json!({
                    "turn_id":"turn-a", "refs":[{"tool_execution_id":tool.id}]
                }),
                _ => serde_json::json!({"turn_id":"turn-a","ignored":ignored}),
            };
            full.push(TranscriptEntry::new("default", kind, None, None, data));
        }
        let mut resident = captured(
            full.iter()
                .enumerate()
                .map(|(index, entry)| {
                    let mut record = evidence(PreparedInputEvidence::Transcript(entry.clone()));
                    Arc::make_mut(&mut record).order_sequence = Some(index as u64);
                    record
                })
                .collect(),
        );
        let keys = full
            .iter()
            .map(|entry| Key::new(Family::Transcript, &entry.id))
            .collect::<Vec<_>>();
        resident.windows.push(window(
            InputWindowSelector::RecentTranscript,
            keys.iter().rev().cloned().collect(),
            keys.len(),
        ));
        resident
            .turn_transcript_keys
            .insert("turn-a".into(), keys.into_iter().collect());
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 4_000_000, 8192)?;
        let selected = {
            let _active = phase.enter();
            // The newest ignored entry occupies the original window slot.
            assert!(inputs.history_transcript(1)?.is_empty());
            let selected = inputs.history_transcript(full.len())?;
            assert_eq!(selected, full[1..3]);
            assert_eq!(
                inputs.history_transcript_for_turns(&["turn-a".into()])?,
                selected
            );
            selected
        };
        phase.finish()?;
        drop(phase);
        assert!(ledger.report()?.observed_bytes > 0);

        let turn = TurnRecord::new("default", "turn-a", 1);
        let render = |entries: &[TranscriptEntry]| {
            super::super::render_turn_execution_sequence(
                &turn,
                &[] as &[BriefRecord],
                std::slice::from_ref(&tool),
                entries,
                super::super::RecentTurnProjectionMode::Continuity,
            )
        };
        let selected_render = super::super::render_turn_execution_sequence(
            &turn,
            &[] as &[BriefRecord],
            std::slice::from_ref(&tool),
            &selected,
            super::super::RecentTurnProjectionMode::Continuity,
        );
        assert_eq!(render(&full), selected_render);
        assert_eq!(selected_render.0.len(), 2);
        let mut work = WorkItemRecord::new(
            "default",
            "history projection",
            crate::types::WorkItemState::Open,
        );
        work.id = "work-a".into();
        assert_eq!(
            super::super::render_current_work_item_process_trace(
                &work,
                &[] as &[BriefRecord],
                &[] as &[ToolContextProjection],
                &full
            ),
            super::super::render_current_work_item_process_trace(
                &work,
                &[] as &[BriefRecord],
                &[] as &[ToolContextProjection],
                &selected
            )
        );
        // The full owning API still rejects the same ignored source material.
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 4_000_000, 8192)?;
        {
            let _active = phase.enter();
            assert!(inputs.transcript(1).is_err());
        }
        assert!(phase.finish().is_err());
        drop(phase);
        drop(selected);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_full_transcript_keeps_consumed_payload_admission_and_failure_latch() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        for kind in [
            TranscriptEntryKind::AssistantRound,
            TranscriptEntryKind::ToolResults,
        ] {
            let entry = TranscriptEntry::new(
                "default",
                kind,
                None,
                None,
                serde_json::json!(vec![serde_json::json!({"body":"retained"}); 4096]),
            );
            let key = Key::new(Family::Transcript, &entry.id);
            let mut resident = captured(vec![evidence(PreparedInputEvidence::Transcript(entry))]);
            resident.windows.push(window(
                InputWindowSelector::RecentTranscript,
                vec![key.clone()],
                1,
            ));
            resident
                .turn_transcript_keys
                .insert("turn-a".into(), BTreeSet::from([key]));
            let mut connection = rusqlite::Connection::open_in_memory()?;
            let tx = connection.transaction()?;
            let durable = ContextDurableRead::new(&tx, "default");
            let work = empty_work();
            let state = AgentState::new("default");
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            for by_turn in [false, true] {
                let phase =
                    ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
                {
                    let _active = phase.enter();
                    let result = if by_turn {
                        inputs.transcript_for_turns(&["turn-a".into()])
                    } else {
                        inputs.transcript(1)
                    };
                    assert!(result.is_err());
                    assert!(crate::diagnostics::resource_io::admit_cloned_value(&"later").is_err());
                }
                assert!(phase.finish().is_err());
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
        Ok(())
    }

    #[test]
    fn resident_history_checks_scope_family_and_prepared_absences_before_filtering() -> Result<()> {
        let entry = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            None,
            serde_json::Value::Null,
        );
        let key = Key::new(Family::Transcript, &entry.id);
        let original = evidence(PreparedInputEvidence::Transcript(entry));
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        for wrong_scope in [false, true] {
            let mut record = original.clone();
            if wrong_scope {
                Arc::make_mut(&mut record)
                    .locator
                    .scope
                    .identity_incarnation += 1;
            } else {
                Arc::make_mut(&mut record).record =
                    PreparedInputEvidence::Message(message("wrong"));
            }
            let mut resident = captured(vec![record]);
            resident.windows.push(window(
                InputWindowSelector::RecentTranscript,
                vec![key.clone()],
                1,
            ));
            resident
                .turn_transcript_keys
                .insert("turn-a".into(), BTreeSet::from([key.clone()]));
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            assert!(inputs
                .history_transcript(1)
                .unwrap_err()
                .to_string()
                .contains(if wrong_scope {
                    "scope mismatch"
                } else {
                    "family mismatch"
                }));
            assert!(inputs
                .history_transcript_for_turns(&["turn-a".into()])
                .is_err());
        }
        let mut resident = captured(Vec::new());
        resident.unavailable.insert(key.clone());
        resident
            .turn_transcript_keys
            .insert("pruned".into(), BTreeSet::from([key]));
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        assert!(inputs
            .history_transcript_for_turns(&["pruned".into()])?
            .is_empty());
        assert!(inputs.history_transcript(0)?.is_empty());
        assert!(inputs
            .history_transcript(1)
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
        assert!(inputs
            .history_transcript_for_turns(&["unprepared".into()])
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
        Ok(())
    }

    #[test]
    fn cold_history_reads_keep_full_transcript_behavior() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = AppStorage::new_for_test(directory.path())?;
        let entry = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            None,
            serde_json::json!({"turn_id":"turn-a","body":"cold source"}),
        );
        storage.append_transcript_entry(&entry)?;
        let committed = storage
            .transcript_by_id(&entry.id)?
            .expect("the appended transcript exists");
        assert!(committed.transcript_seq.is_some());
        assert_eq!(committed.kind, entry.kind);
        assert_eq!(committed.data, entry.data);
        assert_eq!(storage.history_transcript(1)?, vec![committed]);
        assert_eq!(
            storage.history_transcript_for_turns(&["turn-a".into()])?,
            storage.transcript_for_turns(&["turn-a".into()])?
        );
        Ok(())
    }

    #[test]
    fn resident_message_existence_borrows_body_but_owned_reads_still_admit_it() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let source = message(&"large message body".repeat(16_384));
        let resident = captured(vec![evidence(PreparedInputEvidence::Message(
            source.clone(),
        ))]);
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
        {
            let _active = phase.enter();
            assert!(inputs.message_exists(&source.id)?);
        }
        phase.finish()?;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);

        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
        {
            let _active = phase.enter();
            assert!(inputs
                .message_by_id(&source.id)
                .unwrap_err()
                .to_string()
                .contains("ResourceExhausted"));
            assert!(
                inputs.message_exists(&source.id).is_err(),
                "failure stays latched"
            );
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_message_existence_preserves_absence_preparation_scope_and_family() -> Result<()> {
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let mut resident = captured(Vec::new());
        resident
            .unavailable
            .insert(Key::new(Family::Message, "pruned"));
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        assert!(!inputs.message_exists("pruned")?);
        assert!(matches!(
            inputs.message_exists("unprepared").unwrap_err().downcast_ref(),
            Some(InputEvidenceReadError::NeedsPreparation(key))
                if key == &Key::new(Family::Message, "unprepared")
        ));
        let source = message("source");
        let mut wrong_scope = evidence(PreparedInputEvidence::Message(source.clone()));
        Arc::get_mut(&mut wrong_scope)
            .unwrap()
            .locator
            .scope
            .identity_incarnation += 1;
        let mut wrong_family = evidence(PreparedInputEvidence::Brief(BriefRecord::new(
            "default",
            BriefKind::Result,
            "not a Message",
            None,
            None,
        )));
        Arc::get_mut(&mut wrong_family).unwrap().locator.key =
            Key::new(Family::Message, &source.id);
        for (record, expected) in [
            (wrong_scope, "scope mismatch"),
            (wrong_family, "family mismatch"),
        ] {
            let resident = captured(vec![record]);
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            assert!(inputs
                .message_exists(&source.id)
                .unwrap_err()
                .to_string()
                .contains(expected));
        }
        Ok(())
    }

    #[test]
    fn resident_history_excludes_live_turn_before_owned_projection() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let source = message("current");
        let mut current = TurnRecord::new("default", "live", 2);
        current.input_message_ids = vec![source.id.clone()];
        current.tool_execution_ids = vec!["unused".repeat(32_768)];
        let prior = TurnRecord::new("default", "prior", 1);
        let owner = TurnOwner::WorkItem {
            work_item_id: "work".into(),
        };
        let mut resident = captured(vec![
            evidence(PreparedInputEvidence::Turn(prior.clone())),
            evidence(PreparedInputEvidence::Turn(current.clone())),
        ]);
        for selector in [
            InputWindowSelector::GlobalTurns,
            InputWindowSelector::OwnerTurns {
                owner: owner.clone(),
            },
        ] {
            resident.windows.push(window(
                selector,
                vec![
                    Key::new(Family::Turn, &prior.turn_id),
                    Key::new(Family::Turn, &current.turn_id),
                ],
                2,
            ));
        }
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        for selected_owner in [None, Some(&owner)] {
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
            {
                let _active = phase.enter();
                assert_eq!(
                    inputs.history_turns(selected_owner, &source.id, 1)?,
                    vec![prior.clone()]
                );
            }
            phase.finish()?;
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        // The same window still refuses an owning read of the excluded body.
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
        {
            let _active = phase.enter();
            assert!(inputs
                .turns(2)
                .unwrap_err()
                .to_string()
                .contains("ResourceExhausted"));
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_history_families_share_one_material_owner_until_final_view() -> Result<()> {
        use crate::{
            diagnostics::resource_io::{self, ResourceReadMaterial},
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let source = message("shared history source");
        let turn = TurnRecord::new("default", "shared-history", 1);
        let brief = BriefRecord::new("default", BriefKind::Result, "shared brief", None, None);
        let entry = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::AssistantRound,
            None,
            None,
            serde_json::json!({"text": "shared transcript", "turn_id": turn.turn_id}),
        );
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let origin = ResourceReadMaterial::new(&ledger, &scope())?;
        origin.read(|| {
            resource_io::admit_cloned_value(&(&source, &turn, &brief, &entry))?;
            Ok(())
        })?;
        let mut resident = captured(vec![
            evidence(PreparedInputEvidence::Message(source.clone())),
            evidence(PreparedInputEvidence::Turn(turn.clone())),
            evidence(PreparedInputEvidence::Brief(brief.clone())),
            evidence(PreparedInputEvidence::Transcript(entry.clone())),
        ]);
        resident.budget_views.push(origin.guard()?);
        resident.query_acquisition = Some(origin.clone());
        resident.windows = vec![
            window(
                InputWindowSelector::GlobalTurns,
                vec![Key::new(Family::Turn, &turn.turn_id)],
                2,
            ),
            window(
                InputWindowSelector::RecentBriefs,
                vec![Key::new(Family::Brief, &brief.id)],
                1,
            ),
            window(
                InputWindowSelector::RecentTranscript,
                vec![Key::new(Family::Transcript, &entry.id)],
                1,
            ),
        ];
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64 * 1024, 1024)?;
        let active = phase.enter();
        let selected_message = inputs.history_message_by_id(&source.id)?.unwrap();
        let material = inputs.history_material.borrow().clone();
        let before = phase.finish()?;
        let selected_turns = inputs.history_turns(None, "other-input", 1)?;
        let selected_briefs = inputs.history_briefs(1)?;
        let selected_brief = inputs.history_brief_by_id(&brief.id)?.unwrap();
        let selected_transcript = inputs.history_transcript(1)?.remove(0);
        let after = phase.finish()?;
        assert_eq!(
            after.0 - before.0,
            8 * (std::mem::size_of::<ContextHistoryTurn>()
                + 2 * std::mem::size_of::<ContextHistoryBrief>()
                + std::mem::size_of::<ContextHistoryTranscript>()) as u64
        );
        assert_eq!(after.1 - before.1, 4 * 8);
        let PreparedInputEvidence::Message(original) = &resident.records[0].record else {
            unreachable!()
        };
        assert!(std::ptr::eq::<MessageEnvelope>(
            &*selected_message,
            original
        ));
        assert!(resource_io::charge_owned_metadata(u64::MAX, 0).is_err());
        assert!(inputs.history_message_by_id(&source.id).is_err());
        assert!(inputs.history_turns(None, "other-input", 1).is_err());
        assert!(inputs.history_briefs(1).is_err());
        assert!(inputs.history_transcript(1).is_err());
        drop(active);
        drop(inputs);
        drop((resident, origin, phase));
        drop((
            selected_message,
            selected_turns,
            selected_briefs,
            selected_brief,
        ));
        assert!(material.upgrade().is_some());
        assert!(ledger.report()?.observed_bytes > 0);
        assert_eq!(selected_transcript.data["text"], "shared transcript");
        drop(selected_transcript);
        assert!(material.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_history_briefs_share_both_paths_at_exact_limits_until_last_holder() -> Result<()> {
        use crate::{
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "history-brief".repeat(8192),
            None,
            None,
        );
        let record = evidence(PreparedInputEvidence::Brief(brief.clone()));
        let mut resident = captured(vec![record.clone()]);
        resident.windows.push(window(
            InputWindowSelector::RecentBriefs,
            vec![Key::new(Family::Brief, &brief.id)],
            1,
        ));
        resident
            .unavailable
            .insert(Key::new(Family::Brief, "absent"));
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let origin = ResourceReadMaterial::new(&ledger, &scope())?;
        origin.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&brief)?;
            Ok(())
        })?;
        resident.budget_views.push(origin.guard()?);
        resident.query_acquisition = Some(origin.clone());
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        for wrong_scope in [false, true] {
            let mut invalid = record.as_ref().clone();
            if wrong_scope {
                invalid.locator.scope.identity_incarnation += 1;
            } else {
                invalid.record = PreparedInputEvidence::Message(message("wrong family"));
            }
            let mut invalid = captured(vec![Arc::new(invalid)]);
            invalid.windows = resident.windows.clone();
            let inputs = ResidentContextInputs::new(&durable, &invalid, &work, &state, &[])?;
            assert!(inputs.history_briefs(1).is_err());
            assert!(inputs.history_brief_by_id(&brief.id).is_err());
        }
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        assert!(inputs.history_brief_by_id("absent")?.is_none());
        assert!(inputs.history_brief_by_id("unprepared").is_err());
        let header = (scope().agent_id.len() as u64 + 256) * 8;
        let bytes = header
            + 8 * (std::mem::size_of::<turns::HistoryTurnMaterial>()
                + 2 * std::mem::size_of::<usize>()
                + std::mem::size_of_val(resident.budget_views.as_slice())
                + std::mem::size_of::<ContextHistoryBrief>()) as u64;
        let records = 64 + 8 * (resident.budget_views.len() + 2);
        let mut selected = Vec::new();
        for by_id in [false, true] {
            // The second path shares the still-live first path's material.
            let (bytes, records) = if by_id {
                (
                    header + 8 * std::mem::size_of::<ContextHistoryBrief>() as u64,
                    64 + 8,
                )
            } else {
                (bytes, records)
            };
            let read = || -> Result<ContextHistoryBrief> {
                if by_id {
                    Ok(inputs.history_brief_by_id(&brief.id)?.unwrap())
                } else {
                    Ok(inputs.history_briefs(1)?.remove(0))
                }
            };
            for (byte_limit, record_limit) in [(bytes - 1, records), (bytes, records - 1)] {
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    byte_limit,
                    record_limit,
                )?;
                {
                    let _active = phase.enter();
                    assert!(read().is_err());
                    assert!(read().is_err());
                }
                assert!(phase.finish().is_err());
            }
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
            selected.push(phase.read(read)?);
            assert_eq!(phase.finish()?, (bytes, records));
        }
        let PreparedInputEvidence::Brief(original) = &record.record else {
            unreachable!()
        };
        for item in &selected {
            assert!(std::ptr::eq::<BriefRecord>(&**item, original));
            assert_eq!(serde_json::to_value(item)?, serde_json::to_value(&brief)?);
        }
        for by_id in [false, true] {
            let owning = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
            {
                let _active = owning.enter();
                if by_id {
                    assert!(inputs.brief_by_id(&brief.id).is_err());
                } else {
                    assert!(inputs.briefs(1).is_err());
                }
                assert!(inputs.history_brief_by_id(&brief.id).is_err());
            }
            assert!(owning.finish().is_err());
        }
        drop(inputs);
        drop((resident, record, origin));
        assert!(ledger.report()?.observed_bytes > 0);
        assert_eq!(selected[1].text, brief.text);
        selected.remove(0);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(selected);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_history_tools_share_both_paths_at_exact_limits_until_last_holder() -> Result<()> {
        use crate::{
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let mut tool = project_tool_record(&tool())?;
        tool.summary = "history-tool".repeat(8192);
        tool.work_item_id = Some("work-tool-history".into());
        let record = evidence(PreparedInputEvidence::ToolExecution(tool.clone()));
        let mut same_id_message = message("same ID, different family");
        same_id_message.id = tool.id.clone();
        let mut resident = captured(vec![
            record.clone(),
            evidence(PreparedInputEvidence::Message(same_id_message)),
        ]);
        resident.windows.push(window(
            InputWindowSelector::RecentTools,
            vec![Key::new(Family::ToolExecution, &tool.id)],
            1,
        ));
        resident
            .unavailable
            .insert(Key::new(Family::ToolExecution, "absent"));
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let origin = ResourceReadMaterial::new(&ledger, &scope())?;
        origin.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&tool)?;
            Ok(())
        })?;
        resident.budget_views.push(origin.guard()?);
        resident.query_acquisition = Some(origin.clone());
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        for wrong_scope in [false, true] {
            let mut invalid = record.as_ref().clone();
            if wrong_scope {
                invalid.locator.scope.identity_incarnation += 1;
            } else {
                invalid.record = PreparedInputEvidence::Message(message("wrong family"));
            }
            let mut invalid = captured(vec![Arc::new(invalid)]);
            invalid.windows = resident.windows.clone();
            let inputs = ResidentContextInputs::new(&durable, &invalid, &work, &state, &[])?;
            assert!(inputs.history_tools(1).is_err());
            assert!(inputs.history_tool_by_id(&tool.id).is_err());
        }
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        assert!(inputs.history_tool_by_id("absent")?.is_none());
        assert!(inputs.history_tool_by_id("unprepared").is_err());
        let header = (scope().agent_id.len() as u64 + 256) * 8;
        let bytes = header
            + 8 * (std::mem::size_of::<turns::HistoryTurnMaterial>()
                + 2 * std::mem::size_of::<usize>()
                + std::mem::size_of_val(resident.budget_views.as_slice())
                + std::mem::size_of::<ContextHistoryTool>()) as u64;
        let records = 64 + 8 * (resident.budget_views.len() + 2);
        let mut selected = Vec::new();
        for by_id in [false, true] {
            let (bytes, records) = if by_id {
                (
                    header + 8 * std::mem::size_of::<ContextHistoryTool>() as u64,
                    64 + 8,
                )
            } else {
                (bytes, records)
            };
            let read = || -> Result<ContextHistoryTool> {
                if by_id {
                    Ok(inputs.history_tool_by_id(&tool.id)?.unwrap())
                } else {
                    Ok(inputs.history_tools(1)?.remove(0))
                }
            };
            for (byte_limit, record_limit) in [(bytes - 1, records), (bytes, records - 1)] {
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    byte_limit,
                    record_limit,
                )?;
                {
                    let _active = phase.enter();
                    assert!(read().is_err());
                    assert!(read().is_err());
                }
                assert!(phase.finish().is_err());
            }
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
            selected.push(phase.read(read)?);
            assert_eq!(phase.finish()?, (bytes, records));
        }
        let PreparedInputEvidence::ToolExecution(original) = &record.record else {
            unreachable!()
        };
        for item in &selected {
            assert!(std::ptr::eq::<ToolContextProjection>(&**item, original));
            assert_eq!(serde_json::to_value(item)?, serde_json::to_value(&tool)?);
        }
        let mut current_work =
            WorkItemRecord::new("default", "tool history", crate::types::WorkItemState::Open);
        current_work.id = "work-tool-history".into();
        let expected_trace = super::super::render_current_work_item_process_trace(
            &current_work,
            &[] as &[BriefRecord],
            std::slice::from_ref(&tool),
            &[] as &[TranscriptEntry],
        );
        assert!(expected_trace.is_some());
        for item in &selected {
            assert_eq!(
                super::super::render_current_work_item_process_trace(
                    &current_work,
                    &[] as &[BriefRecord],
                    std::slice::from_ref(item),
                    &[] as &[TranscriptEntry],
                ),
                expected_trace,
            );
        }
        for by_id in [false, true] {
            let owning = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
            {
                let _active = owning.enter();
                if by_id {
                    assert!(inputs.tool_by_id(&tool.id).is_err());
                } else {
                    assert!(inputs.tools(1).is_err());
                }
                assert!(inputs.history_tool_by_id(&tool.id).is_err());
                assert!(inputs.history_tools(1).is_err());
            }
            assert!(owning.finish().is_err());
        }
        drop(inputs);
        drop((resident, record, origin));
        assert!(ledger.report()?.observed_bytes > 0);
        assert_eq!(selected[1].summary, tool.summary);
        selected.remove(0);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(selected);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_history_shares_large_transcript_at_exact_limits_and_retains_material() -> Result<()>
    {
        use crate::{
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let entry = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::AssistantRound,
            None,
            None,
            serde_json::json!({"text": "transcript-body".repeat(8192), "turn_id": "turn-a"}),
        );
        let record = evidence(PreparedInputEvidence::Transcript(entry.clone()));
        let mut resident = captured(vec![record.clone()]);
        resident.windows.push(window(
            InputWindowSelector::RecentTranscript,
            vec![Key::new(Family::Transcript, &entry.id)],
            1,
        ));
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let origin = ResourceReadMaterial::new(&ledger, &scope())?;
        origin.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&entry)?;
            Ok(())
        })?;
        resident.budget_views.push(origin.guard()?);
        resident.query_acquisition = Some(origin.clone());
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let header = (scope().agent_id.len() as u64 + 256) * 8;
        let bytes = header
            + 8 * (std::mem::size_of::<turns::HistoryTurnMaterial>()
                + 2 * std::mem::size_of::<usize>()
                + std::mem::size_of_val(resident.budget_views.as_slice())
                + std::mem::size_of::<ContextHistoryTranscript>()) as u64;
        let records = 64 + 8 * (resident.budget_views.len() + 2);
        for (byte_limit, record_limit) in [(bytes - 1, records), (bytes, records - 1)] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                byte_limit,
                record_limit,
            )?;
            {
                let _active = phase.enter();
                assert!(inputs.history_transcript(1).is_err());
                assert!(inputs.history_transcript(1).is_err());
            }
            assert!(phase.finish().is_err());
        }
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
        let selected = {
            let _active = phase.enter();
            inputs.history_transcript(1)?
        };
        assert_eq!(phase.finish()?, (bytes, records));
        assert_eq!(selected, vec![entry.clone()]);
        assert_eq!(serde_json::to_value(&selected)?, serde_json::json!([entry]));
        let PreparedInputEvidence::Transcript(original) = &record.record else {
            unreachable!()
        };
        assert!(std::ptr::eq::<TranscriptEntry>(&*selected[0], original));
        let retained = selected.clone();
        drop((selected, inputs));
        drop((resident, record, origin, phase));
        assert!(ledger.report()?.observed_bytes > 0);
        assert_eq!(retained[0].data["text"].as_str().unwrap().len(), 15 * 8192);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_history_shares_large_turn_at_exact_limits_and_retains_material() -> Result<()> {
        use crate::{
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let mut turn = TurnRecord::new("default", "large-history", 1);
        turn.tool_execution_ids = vec!["history-body".repeat(32_768)];
        let record = evidence(PreparedInputEvidence::Turn(turn.clone()));
        let mut resident = captured(vec![record.clone()]);
        resident.windows.push(window(
            InputWindowSelector::GlobalTurns,
            vec![Key::new(Family::Turn, &turn.turn_id)],
            2,
        ));
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let origin = ResourceReadMaterial::new(&ledger, &scope())?;
        origin.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&turn)?;
            Ok(())
        })?;
        resident.budget_views.push(origin.guard()?);
        resident.query_acquisition = Some(origin.clone());
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let header = (scope().agent_id.len() as u64 + 256) * 8;
        let bytes = header
            + 8 * (std::mem::size_of::<turns::HistoryTurnMaterial>()
                + 2 * std::mem::size_of::<usize>()
                + std::mem::size_of_val(resident.budget_views.as_slice())
                + std::mem::size_of::<ContextHistoryTurn>()) as u64;
        let records = 64 + 8 * (resident.budget_views.len() + 2);
        for (byte_limit, record_limit) in [(bytes - 1, records), (bytes, records - 1)] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                byte_limit,
                record_limit,
            )?;
            {
                let _active = phase.enter();
                assert!(inputs.history_turns(None, "other-input", 1).is_err());
            }
            assert!(phase.finish().is_err());
        }
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
        let selected = {
            let _active = phase.enter();
            inputs.history_turns(None, "other-input", 1)?
        };
        assert_eq!(phase.finish()?, (bytes, records));
        assert_eq!(selected, vec![turn]);
        let PreparedInputEvidence::Turn(original) = &record.record else {
            unreachable!()
        };
        assert!(std::ptr::eq::<TurnRecord>(&*selected[0], original));
        let retained = selected.clone();
        drop((selected, inputs));
        drop((resident, record, origin, phase));
        assert!(ledger.report()?.observed_bytes > 0);
        assert_eq!(retained[0].tool_execution_ids[0].len(), 12 * 32_768);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_history_message_shares_body_at_exact_limits_until_last_holder() -> Result<()> {
        use crate::{
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let source = message(&"history-message".repeat(32_768));
        let id = source.id.clone();
        let record = evidence(PreparedInputEvidence::Message(source));
        let mut resident = captured(vec![record.clone()]);
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let origin = ResourceReadMaterial::new(&ledger, &scope())?;
        origin.read(|| {
            crate::diagnostics::resource_io::admit_cloned_value(&record.record)?;
            Ok(())
        })?;
        resident.budget_views.push(origin.guard()?);
        resident.query_acquisition = Some(origin.clone());
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let header = (scope().agent_id.len() as u64 + 256) * 8;
        let bytes = header
            + 8 * (std::mem::size_of::<turns::HistoryTurnMaterial>()
                + 2 * std::mem::size_of::<usize>()
                + std::mem::size_of_val(resident.budget_views.as_slice())
                + std::mem::size_of::<ContextHistoryMessage>()) as u64;
        let records = 64 + 8 * (resident.budget_views.len() + 2);
        for (byte_limit, record_limit) in [(bytes - 1, records), (bytes, records - 1)] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                byte_limit,
                record_limit,
            )?;
            {
                let _active = phase.enter();
                assert!(inputs.history_message_by_id(&id).is_err());
            }
            assert!(phase.finish().is_err());
        }
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
        let selected = {
            let _active = phase.enter();
            inputs
                .history_message_by_id(&id)?
                .expect("prepared message")
        };
        assert_eq!(phase.finish()?, (bytes, records));
        let PreparedInputEvidence::Message(original) = &record.record else {
            unreachable!()
        };
        assert!(std::ptr::eq::<MessageEnvelope>(&*selected, original));
        let owning = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
        {
            let _active = owning.enter();
            assert!(inputs.message_by_id(&id).is_err());
        }
        assert!(owning.finish().is_err());
        drop(owning);
        let retained = selected.clone();
        drop((selected, inputs));
        drop((resident, record, origin, phase));
        assert!(ledger.report()?.observed_bytes > 0);
        assert_eq!(retained.id, id);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_history_message_keeps_absence_scope_and_family_fences() -> Result<()> {
        let source = message("history");
        let id = source.id.clone();
        let valid = evidence(PreparedInputEvidence::Message(source));
        let mut wrong_scope = valid.as_ref().clone();
        wrong_scope.locator.scope.identity_incarnation += 1;
        let mut wrong_family = valid.as_ref().clone();
        wrong_family.record = PreparedInputEvidence::Turn(TurnRecord::new("default", "turn", 1));
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        for record in [Arc::new(wrong_scope), Arc::new(wrong_family)] {
            let resident = captured(vec![record]);
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            assert!(inputs.history_message_by_id(&id).is_err());
        }
        let mut resident = captured(vec![valid]);
        resident
            .unavailable
            .insert(Key::new(Family::Message, "absent"));
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        assert!(inputs.history_message_by_id("absent")?.is_none());
        assert!(inputs.history_message_by_id("unprepared").is_err());
        Ok(())
    }

    #[test]
    fn history_trigger_existence_preserves_canonical_assignment_without_body_copy() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let mut source = message(&"unneeded trigger body".repeat(16_384));
        source.turn_id = Some("incorrect-message-hint".into());
        let mut turn = TurnRecord::new("default", "canonical-turn", 1);
        turn.input_message_ids = vec![source.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&source));
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        for (present, assignment, valid) in [
            (true, Some("canonical-turn"), true),
            (true, Some("another-turn"), false),
            (true, None, false),
            (false, Some("another-turn"), true),
        ] {
            let mut resident = captured(if present {
                vec![evidence(PreparedInputEvidence::Message(source.clone()))]
            } else {
                Vec::new()
            });
            if !present {
                resident
                    .unavailable
                    .insert(Key::new(Family::Message, &source.id));
            }
            Arc::make_mut(&mut resident.message_turn_keys)
                .insert(source.id.clone(), assignment.map(str::to_owned));
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
            {
                let _active = phase.enter();
                let observations = crate::context::snapshot::HistoryObservations::capture_inputs(
                    &inputs,
                    std::slice::from_ref(&turn),
                    &[] as &[BriefRecord],
                    std::slice::from_ref(&source),
                )?;
                assert_eq!(observations.trigger_summary_valid(&turn), valid);
                assert_eq!(
                    observations.assigned_turn_for_message(&source.id),
                    if present { assignment } else { None },
                );
            }
            phase.finish()?;
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }

    #[test]
    fn ordinary_input_projection_uses_original_phase_remainder_before_copy() -> Result<()> {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let directory = tempfile::tempdir()?;
        let storage = AppStorage::new_for_test(directory.path())?;
        let db = storage.runtime_db()?.unwrap();
        let mut connection = db.connection()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let original = message(&"body".repeat(2048));
        let captured = captured(vec![evidence(PreparedInputEvidence::Message(
            original.clone(),
        ))]);
        let state = AgentState::new("default");
        let work = empty_work();
        let inputs = ResidentContextInputs::new(&durable, &captured, &work, &state, &[])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;

        for window in [false, true] {
            // Both paths fit with the same limits and an unconsumed allowance.
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), 1_000_000, 65_536)?;
            {
                let _active = phase.enter();
                assert_eq!(inputs.message_by_id(&original.id)?, Some(original.clone()));
                assert_eq!(
                    inputs
                        .records(captured.records.clone(), |record| Some(record.clone()))?
                        .len(),
                    1
                );
            }
            phase.finish()?;
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);

            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), 1_000_000, 65_536)?;
            let copied = std::cell::Cell::new(false);
            {
                let _active = phase.enter();
                charge_owned_metadata(100_000, 0)?;
                let project = |record: &PreparedInputEvidence| {
                    copied.set(true);
                    Some(record.clone())
                };
                let result = if window {
                    inputs
                        .records(captured.records.clone(), project)
                        .map(|_| ())
                } else {
                    inputs
                        .by_id(Family::Message, &original.id, project)
                        .map(|_| ())
                };
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("ResourceExhausted"));
            }
            assert!(
                !copied.get(),
                "projection must not allocate before admission"
            );
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        // Normal and missing-key behavior remains available without a phase.
        assert_eq!(inputs.message_by_id(&original.id)?, Some(original));
        assert!(inputs.message_by_id("missing").is_err());
        Ok(())
    }

    #[test]
    fn history_assignment_batch_uses_one_exact_owning_envelope_and_keeps_absence_proof(
    ) -> Result<()> {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::{cloned_value_material, ResourceReadMaterial},
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let mut resident = captured(Vec::new());
        let mut expected = std::collections::BTreeMap::new();
        for index in 0..16 {
            let mut source = message("assignment body is never copied");
            source.id = format!("assignment-{index}");
            source.turn_id = Some("untrusted-hint".into());
            resident
                .records
                .push(evidence(PreparedInputEvidence::Message(source.clone())));
            let assignment = (index % 2 == 0).then(|| format!("canonical-turn-{index}"));
            Arc::make_mut(&mut resident.message_turn_keys)
                .insert(source.id.clone(), assignment.clone());
            expected.insert(source.id, assignment);
        }
        resident
            .unavailable
            .insert(Key::new(Family::Message, "pruned"));
        expected.insert("pruned".into(), None);
        let ids = expected.keys().cloned().collect::<BTreeSet<_>>();
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 1_000_000, 65_536)?;
        let base = phase.finish()?;
        drop(phase);
        let (bytes, records) = cloned_value_material(&expected)?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            base.0 + 8 * bytes as u64,
            base.1 + 8 * records,
        )?;
        {
            let _active = phase.enter();
            assert_eq!(inputs.history_message_assignments(ids.clone())?, expected);
            assert_eq!(
                phase.finish()?,
                (base.0 + 8 * bytes as u64, base.1 + 8 * records)
            );
            assert!(inputs.history_message_assignments(ids.clone()).is_err());
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);

        drop(inputs);
        let mut incomplete = resident;
        Arc::make_mut(&mut incomplete.message_turn_keys).remove("assignment-0");
        let inputs = ResidentContextInputs::new(&durable, &incomplete, &work, &state, &[])?;
        let error = inputs.history_message_assignments(ids).unwrap_err();
        assert!(matches!(
            error.downcast_ref(),
            Some(
                crate::runtime::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(
                    _
                )
            )
        ));
        Ok(())
    }

    #[test]
    fn history_assignment_views_share_canonical_metadata_and_final_material() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let source = message("selected assignment");
        let hidden = message("unselected assignment");
        let mut resident = captured(vec![
            evidence(PreparedInputEvidence::Message(source.clone())),
            evidence(PreparedInputEvidence::Message(hidden.clone())),
        ]);
        Arc::make_mut(&mut resident.message_turn_keys)
            .insert(source.id.clone(), Some("canonical-turn".repeat(4096)));
        Arc::make_mut(&mut resident.message_turn_keys).insert(hidden.id.clone(), None);
        resident
            .unavailable
            .insert(Key::new(Family::Message, "pruned"));
        Arc::make_mut(&mut resident.message_turn_keys)
            .insert("pruned".into(), Some("stale-pruned-assignment".into()));
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let ids = [source.id.as_str(), "pruned", source.id.as_str()];
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 1_000_000, 8192)?;
        let limits = {
            let _active = phase.enter();
            let first = inputs.history_assignment_view(ids.into_iter())?;
            let second = inputs.history_assignment_view(ids.into_iter())?;
            assert!(std::ptr::eq(
                first.get(&source.id).unwrap(),
                second.get(&source.id).unwrap()
            ));
            phase.finish()?
        };
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let active = phase.enter();
        let first = inputs.history_assignment_view(ids.into_iter())?;
        let second = inputs.history_assignment_view(ids.into_iter())?;
        assert!(std::ptr::eq(
            first.get(&source.id).unwrap(),
            resident.message_turn_keys.get(&source.id).unwrap()
        ));
        assert_eq!(first.get("pruned"), Some(&None));
        assert!(first.get(&hidden.id).is_none());
        assert_eq!(
            serde_json::to_value(&first)?,
            serde_json::json!({
                source.id.clone(): resident.message_turn_keys[&source.id],
                "pruned": null
            })
        );
        assert_eq!(phase.finish()?, limits);
        assert!(inputs.history_assignment_view(ids.into_iter()).is_err());
        assert!(inputs.history_assignment_view(ids.into_iter()).is_err());
        drop(active);
        drop(phase);
        drop(first);
        assert!(ledger.report()?.observed_bytes > 0);
        assert!(second.get(&source.id).unwrap().is_some());
        drop(second);
        assert_eq!(ledger.report()?.observed_bytes, 0);

        // A second phase must retain its own selectors, even when the source
        // material owner was created by the first history window.
        let first_phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 100_000, 8192)?;
        let first = first_phase.read(|| inputs.history_assignment_view(ids.into_iter()))?;
        let second_phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), 100_000, 8192)?;
        let second = second_phase.read(|| inputs.history_assignment_view(ids.into_iter()))?;
        let retained = ledger.report()?.observed_bytes;
        drop(second_phase);
        assert_eq!(ledger.report()?.observed_bytes, retained);
        drop(first_phase);
        drop(inputs);
        drop(resident);
        drop(first);
        let last = second.clone();
        drop(second);
        assert_eq!(ledger.report()?.observed_bytes, retained);
        assert!(last.get(&source.id).unwrap().is_some());
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);

        let mut incomplete = captured(vec![evidence(PreparedInputEvidence::Message(
            source.clone(),
        ))]);
        let inputs = ResidentContextInputs::new(&durable, &incomplete, &work, &state, &[])?;
        let error = inputs
            .history_assignment_view([source.id.as_str()].into_iter())
            .unwrap_err();
        assert!(error
            .downcast_ref::<crate::runtime::resident_input_evidence::InputEvidenceReadError>()
            .is_some());
        drop(inputs);
        Arc::make_mut(&mut incomplete.message_turn_keys).insert(source.id.clone(), None);
        Arc::make_mut(&mut incomplete.records[0])
            .locator
            .scope
            .identity_incarnation += 1;
        let inputs = ResidentContextInputs::new(&durable, &incomplete, &work, &state, &[])?;
        assert!(inputs
            .history_assignment_view([source.id.as_str()].into_iter())
            .is_err());
        Ok(())
    }

    #[test]
    fn ordinary_input_lookup_and_assignment_admit_borrowed_ids() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let directory = tempfile::tempdir()?;
        let storage = AppStorage::new_for_test(directory.path())?;
        let db = storage.runtime_db()?.unwrap();
        let mut connection = db.connection()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let original = message("assignment");
        let mut captured = captured(vec![evidence(PreparedInputEvidence::Message(
            original.clone(),
        ))]);
        Arc::make_mut(&mut captured.message_turn_keys)
            .insert(original.id.clone(), Some("turn".repeat(16_384)));
        captured
            .unavailable
            .insert(Key::new(Family::Message, "absent"));
        let state = AgentState::new("default");
        let work = empty_work();
        let inputs = ResidentContextInputs::new(&durable, &captured, &work, &state, &[])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 4096, 64)?;
        let baseline = phase.finish()?;
        {
            let _active = phase.enter();
            inputs.inspect_assigned_turn(&original.id, |assignment| {
                let expected = captured.message_turn_keys[&original.id].as_deref().unwrap();
                assert!(std::ptr::eq(assignment.unwrap(), expected));
            })?;
            assert!(inputs.inspect_assigned_turn("absent", |value| value.is_none())?);
        }
        assert_eq!(phase.finish()?, baseline);
        drop(phase);
        assert!(inputs.inspect_assigned_turn("unprepared", |_| ()).is_err());
        let huge_id = "missing".repeat(16_384);
        for assignment in [false, true] {
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 100_000, 65_536)?;
            {
                let _active = phase.enter();
                let result = if assignment {
                    inputs.assigned_turn_for_message(&original.id)
                } else {
                    inputs.message_by_id(&huge_id).map(|_| None)
                };
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("ResourceExhausted"));
            }
            assert!(phase.finish().is_err());
            {
                let _active = phase.enter();
                assert!(inputs.inspect_assigned_turn(&original.id, |_| ()).is_err());
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        assert_eq!(
            inputs.assigned_turn_for_message(&original.id)?,
            captured.message_turn_keys[&original.id]
        );
        Ok(())
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
    fn idle_batch_projection_borrows_absence_and_admits_only_the_selected_batch() -> Result<()> {
        use crate::{
            config::StateMemoryLimits,
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
            types::{IdleInputBatchItem, IdleInputBatchRecord},
        };
        // Any accidental durable fallback encounters a missing table.
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let mut absent = TurnRecord::new("default", "absent-batch", 1);
        absent.input_message_ids = vec!["unrelated".repeat(64 * 1024)];
        let mut present = absent.clone();
        present.turn_id = "present-batch".into();
        present.idle_input_batch = Some(IdleInputBatchRecord {
            anchor_message_id: "anchor".into(),
            activation_id: None,
            sender_agent_id: "sender".into(),
            cutoff: chrono::Utc::now(),
            max_items: 1,
            max_bytes: 1024,
            rendered_bytes: 5,
            items: vec![IdleInputBatchItem {
                message_id: "message".into(),
                source_ref: "source".into(),
                rendered: "hello".into(),
            }],
        });
        let mut large = present.clone();
        large.turn_id = "large-batch".into();
        large.idle_input_batch.as_mut().unwrap().items[0].rendered = "x".repeat(64 * 1024);
        let mut resident = captured(vec![
            evidence(PreparedInputEvidence::Turn(absent)),
            evidence(PreparedInputEvidence::Turn(present.clone())),
            evidence(PreparedInputEvidence::Turn(large)),
        ]);
        resident
            .unavailable
            .insert(Key::new(Family::Turn, "pruned"));
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let header_only = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 4096, 64)?;
        let baseline = header_only.finish()?;
        {
            let _active = header_only.enter();
            assert_eq!(inputs.idle_input_batch_for_turn("absent-batch")?, None);
            assert_eq!(inputs.idle_input_batch_for_turn("pruned")?, None);
        }
        assert_eq!(header_only.finish()?, baseline);
        drop(header_only);
        let (bytes, records) = crate::diagnostics::resource_io::cloned_value_material(
            present.idle_input_batch.as_ref().unwrap(),
        )?;
        let exact = (
            baseline.0 + 8 * u64::try_from(bytes)?,
            baseline.1 + 8 * records,
        );
        for (byte_limit, record_limit) in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                byte_limit,
                record_limit,
            )?;
            {
                let _active = phase.enter();
                assert!(inputs.idle_input_batch_for_turn("present-batch").is_err());
                assert!(inputs.idle_input_batch_for_turn("absent-batch").is_err());
            }
            assert!(phase.finish().is_err());
        }
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _active = phase.enter();
            assert_eq!(
                inputs.idle_input_batch_for_turn("present-batch")?,
                present.idle_input_batch
            );
            assert_eq!(phase.finish()?, exact);
            let error = inputs.idle_input_batch_for_turn("unprepared").unwrap_err();
            assert!(error.to_string().contains("ResourceExhausted"));
            assert!(inputs.turn_by_id("present-batch").is_err());
            assert!(inputs.idle_input_batch_for_turn("absent-batch").is_err());
        }
        assert!(phase.finish().is_err());
        drop(phase);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _active = phase.enter();
            let error = inputs.idle_input_batch_for_turn("unprepared").unwrap_err();
            assert!(matches!(
                error.downcast_ref::<
                    crate::runtime::resident_input_evidence::InputEvidenceReadError,
                >(),
                Some(crate::runtime::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(key))
                    if key == &Key::new(Family::Turn, "unprepared")
            ));
        }
        phase.finish()?;
        drop(phase);
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), exact.0, exact.1)?;
        {
            let _active = phase.enter();
            assert!(inputs.idle_input_batch_for_turn("large-batch").is_err());
            assert!(inputs.idle_input_batch_for_turn("pruned").is_err());
        }
        assert!(phase.finish().is_err());
        drop(phase);
        drop(inputs);
        Arc::make_mut(&mut resident.records[0])
            .locator
            .scope
            .identity_incarnation += 1;
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        assert!(inputs.idle_input_batch_for_turn("absent-batch").is_err());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
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
        Arc::make_mut(&mut resident.message_turn_keys).insert(anchor.id.clone(), None);
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
            &[] as &[BriefRecord],
            &messages,
        )
        .unwrap();
        assert!(observations.message_belongs_to_turn(&turn, &source.id));
        assert!(observations.trigger_summary_valid(&turn));
        let mut selected = captured(vec![
            evidence(PreparedInputEvidence::Message(source.clone())),
            evidence(PreparedInputEvidence::Turn(turn.clone())),
        ]);
        Arc::make_mut(&mut selected.message_turn_keys)
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
            &[] as &[BriefRecord],
            &messages,
        )
        .unwrap();
        tx.commit().unwrap();
        // Pure prompt projection consumes only the already-frozen relation.
        let evidence = crate::context::build_recent_turn_projection_evidence(
            &resident_observations,
            std::slice::from_ref(&turn),
            &messages,
            &[] as &[BriefRecord],
            &[] as &[ToolContextProjection],
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
            &[] as &[BriefRecord],
            &[] as &[ToolContextProjection],
            &[] as &[TranscriptEntry],
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
        Arc::make_mut(&mut selected.message_turn_keys).insert(source.id.clone(), None);
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
    fn resident_brief_text_borrows_sources_before_projecting() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let mut source = message("exact incoming body");
        source.origin = MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: Some("unused metadata".repeat(16_384)),
        };
        let transcript = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(source.id.clone()),
            serde_json::json!({"body":{"text":"stale".repeat(16_384)}}),
        );
        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "unused fallback".repeat(16_384),
            None,
            None,
        );
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: transcript.id.clone(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        let resident = captured(vec![
            evidence(PreparedInputEvidence::Transcript(transcript)),
            evidence(PreparedInputEvidence::Message(source)),
        ]);
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
        {
            let _active = phase.enter();
            assert_eq!(
                super::super::resolved_brief_text_from_inputs(&inputs, &brief)?,
                "exact incoming body"
            );
            assert_eq!(
                inputs.history_brief_text(&brief)?.as_str(),
                "exact incoming body"
            );
        }
        phase.finish()?;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_history_brief_text_retains_single_source_and_material() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        for incoming in [false, true] {
            let body = " selected é\n".repeat(16_384);
            let source = message(&body);
            let transcript = TranscriptEntry::new(
                "default",
                if incoming {
                    TranscriptEntryKind::IncomingMessage
                } else {
                    TranscriptEntryKind::AssistantRound
                },
                None,
                incoming.then(|| source.id.clone()),
                serde_json::json!({"blocks":[
                    {"type":"tool_call","text":"ignored"},
                    {"type":"text","Text":{"text":body},"text":"not selected"}]}),
            );
            let mut brief = BriefRecord::new("default", BriefKind::Result, "fallback", None, None);
            brief.content_source = BriefContentSource::TranscriptEntry {
                entry_id: transcript.id.clone(),
                relation: BriefContentSourceRelation::Finalizes,
            };
            let mut records = Vec::new();
            if incoming {
                records.push(evidence(PreparedInputEvidence::Message(source)));
            }
            records.push(evidence(PreparedInputEvidence::Transcript(transcript)));
            let resident = captured(records);
            let weak = Arc::downgrade(&resident.records[0]);
            let original = brief_text::borrowed_text(&resident.records[0].record)
                .expect("single selected text")
                .as_ptr();
            let mut connection = rusqlite::Connection::open_in_memory()?;
            let tx = connection.transaction()?;
            let durable = ContextDurableRead::new(&tx, "default");
            let work = empty_work();
            let state = AgentState::new("default");
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            for index in 0..resident.records.len() {
                for wrong_scope in [false, true] {
                    let mut invalid = (*resident.records[index]).clone();
                    if wrong_scope {
                        invalid.locator.scope.identity_incarnation += 1;
                    } else {
                        invalid.record = PreparedInputEvidence::Brief(brief.clone());
                    }
                    let mut records = resident.records.clone();
                    records[index] = Arc::new(invalid);
                    let invalid = captured(records);
                    let inputs =
                        ResidentContextInputs::new(&durable, &invalid, &work, &state, &[])?;
                    assert!(inputs.history_brief_text(&brief).is_err());
                }
            }
            let unprepared = captured(Vec::new());
            assert!(
                ResidentContextInputs::new(&durable, &unprepared, &work, &state, &[])?
                    .history_brief_text(&brief)
                    .is_err()
            );
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let bytes = (scope().agent_id.len() as u64 + 256) * 8
                + 8 * (std::mem::size_of::<turns::HistoryTurnMaterial>()
                    + 2 * std::mem::size_of::<usize>()
                    + std::mem::size_of::<ContextHistoryBriefText>()) as u64;
            let records = 64 + 8 * 2;
            for (bytes, records) in [(bytes - 1, records), (bytes, records - 1)] {
                let phase =
                    ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
                {
                    let _active = phase.enter();
                    assert!(inputs.history_brief_text(&brief).is_err());
                    assert!(inputs.history_brief_text(&brief).is_err());
                }
                assert!(phase.finish().is_err());
            }
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
            let text = phase.read(|| inputs.history_brief_text(&brief))?;
            assert_eq!(phase.finish()?, (bytes, records));
            assert_eq!(text.as_str().as_ptr(), original);
            assert_eq!(text.as_str(), body);
            assert_eq!(serde_json::to_value(&text)?, serde_json::json!(body));
            drop(inputs);
            drop(resident);
            drop(phase);
            assert!(weak.upgrade().is_some());
            assert!(ledger.report()?.observed_bytes > 0);
            assert_eq!(text.as_str(), body);
            drop(text);
            assert!(weak.upgrade().is_none());
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }

    #[test]
    fn resident_history_brief_text_preserves_message_precedence() -> Result<()> {
        for (body, expected) in [
            (
                MessageBody::Text {
                    text: " \n ".into(),
                },
                "fallback",
            ),
            (
                MessageBody::Json {
                    value: serde_json::json!({"selected":"body"}),
                },
                "{\"selected\":\"body\"}",
            ),
        ] {
            let mut source = message("unused");
            source.body = body;
            let transcript = TranscriptEntry::new(
                "default",
                TranscriptEntryKind::IncomingMessage,
                None,
                Some(source.id.clone()),
                serde_json::json!({"blocks":[{"type":"text","text":"stale transcript"}]}),
            );
            let mut brief = BriefRecord::new("default", BriefKind::Result, "fallback", None, None);
            brief.content_source = BriefContentSource::TranscriptEntry {
                entry_id: transcript.id.clone(),
                relation: BriefContentSourceRelation::Finalizes,
            };
            let resident = captured(vec![
                evidence(PreparedInputEvidence::Transcript(transcript)),
                evidence(PreparedInputEvidence::Message(source)),
            ]);
            let mut connection = rusqlite::Connection::open_in_memory()?;
            let tx = connection.transaction()?;
            let durable = ContextDurableRead::new(&tx, "default");
            let work = empty_work();
            let state = AgentState::new("default");
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            assert_eq!(inputs.history_brief_text(&brief)?.as_str(), expected);
            assert_eq!(inputs.resolved_brief_text(&brief)?, expected);
        }
        Ok(())
    }

    #[test]
    fn resident_history_brief_text_keeps_join_and_json_admission() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        for incoming in [false, true] {
            let mut source = message("unused");
            source.body = MessageBody::Json {
                value: serde_json::json!({"selected": "large".repeat(16_384)}),
            };
            let mut blocks = vec![serde_json::json!({"type":"text","text":""}); 8192];
            blocks.push(serde_json::json!({"type":"text","text":"selected"}));
            let transcript = TranscriptEntry::new(
                "default",
                if incoming {
                    TranscriptEntryKind::IncomingMessage
                } else {
                    TranscriptEntryKind::AssistantRound
                },
                None,
                incoming.then(|| source.id.clone()),
                serde_json::json!({"blocks":blocks}),
            );
            let mut brief = BriefRecord::new("default", BriefKind::Result, "fallback", None, None);
            brief.content_source = BriefContentSource::TranscriptEntry {
                entry_id: transcript.id.clone(),
                relation: BriefContentSourceRelation::Finalizes,
            };
            let mut records = vec![evidence(PreparedInputEvidence::Transcript(transcript))];
            if incoming {
                records.push(evidence(PreparedInputEvidence::Message(source)));
            }
            let resident = captured(records);
            let mut connection = rusqlite::Connection::open_in_memory()?;
            let tx = connection.transaction()?;
            let durable = ContextDurableRead::new(&tx, "default");
            let work = empty_work();
            let state = AgentState::new("default");
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
            {
                let _active = phase.enter();
                assert!(inputs
                    .history_brief_text(&brief)
                    .unwrap_err()
                    .to_string()
                    .contains("ResourceExhausted"));
                assert!(inputs.history_brief_text(&brief).is_err());
            }
            assert!(phase.finish().is_err());
        }
        Ok(())
    }

    #[test]
    fn resident_brief_text_rejects_body_before_copy_without_fallback() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let source = message(&"selected body".repeat(16_384));
        let transcript = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(source.id.clone()),
            serde_json::json!({"body":{"text":"stale"}}),
        );
        let mut brief = BriefRecord::new("default", BriefKind::Result, "fallback", None, None);
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: transcript.id.clone(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        let resident = captured(vec![
            evidence(PreparedInputEvidence::Transcript(transcript)),
            evidence(PreparedInputEvidence::Message(source)),
        ]);
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
        {
            let _active = phase.enter();
            let error = inputs.resolved_brief_text(&brief).unwrap_err();
            assert!(error.to_string().contains("ResourceExhausted"));
            assert!(
                inputs.resolved_brief_text(&brief).is_err(),
                "failure must stay latched"
            );
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn resident_brief_text_preserves_recorded_absence_and_scope_errors() -> Result<()> {
        let source = message("unused");
        let transcript = TranscriptEntry::new(
            "default",
            TranscriptEntryKind::IncomingMessage,
            None,
            Some(source.id.clone()),
            serde_json::json!({"body":{"text":"transcript body"}}),
        );
        let mut brief = BriefRecord::new("default", BriefKind::Result, "fallback", None, None);
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: transcript.id.clone(),
            relation: BriefContentSourceRelation::Finalizes,
        };
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        for missing_transcript in [false, true] {
            let mut resident = captured(Vec::new());
            resident
                .unavailable
                .insert(Key::new(Family::Message, &source.id));
            if missing_transcript {
                resident
                    .unavailable
                    .insert(Key::new(Family::Transcript, &transcript.id));
            } else {
                resident
                    .records
                    .push(evidence(PreparedInputEvidence::Transcript(
                        transcript.clone(),
                    )));
            }
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            assert_eq!(
                inputs.resolved_brief_text(&brief)?,
                if missing_transcript {
                    "fallback"
                } else {
                    "transcript body"
                }
            );
            assert_eq!(
                inputs.history_brief_text(&brief)?.as_str(),
                if missing_transcript {
                    "fallback"
                } else {
                    "transcript body"
                }
            );
        }
        let mut wrong_scope = evidence(PreparedInputEvidence::Transcript(transcript));
        Arc::get_mut(&mut wrong_scope)
            .unwrap()
            .locator
            .scope
            .identity_incarnation += 1;
        let resident = captured(vec![wrong_scope]);
        let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
        assert!(inputs
            .resolved_brief_text(&brief)
            .unwrap_err()
            .to_string()
            .contains("scope mismatch"));
        assert!(inputs
            .history_brief_text(&brief)
            .unwrap_err()
            .to_string()
            .contains("scope mismatch"));
        Ok(())
    }

    #[test]
    fn resident_brief_transcript_projection_ignores_non_text_structure() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let ignored = serde_json::json!(vec![serde_json::json!({"detail":"unused"}); 4096]);
        for (kind, data, expected) in [
            (
                TranscriptEntryKind::AssistantRound,
                serde_json::json!({"ignored":ignored,"blocks":[
                    {"type":"text","text":""},
                    {"type":"tool_call","output":ignored},
                    {"type":"text","Text":{"text":"é\nfirst"},"text":"not selected"},
                    {"type":"text","text":""},
                    {"type":"text","text":"second"},
                    {"type":"text","text":""}]}),
                " é\nfirst  second ",
            ),
            (
                TranscriptEntryKind::RuntimeFailure,
                serde_json::json!({"body":{"Brief":{"text":" exact body \n"}},
                    "blocks":ignored}),
                " exact body \n",
            ),
            (
                TranscriptEntryKind::IncomingMessage,
                serde_json::json!({"body":{"Text":{"text":" \n "}},
                    "ignored":ignored,"blocks":[{"type":"text","text":"block fallback"}]}),
                "block fallback",
            ),
        ] {
            let transcript = TranscriptEntry::new("default", kind, None, None, data);
            let mut brief = BriefRecord::new("default", BriefKind::Result, "fallback", None, None);
            brief.content_source = BriefContentSource::TranscriptEntry {
                entry_id: transcript.id.clone(),
                relation: BriefContentSourceRelation::Finalizes,
            };
            let resident = captured(vec![evidence(PreparedInputEvidence::Transcript(
                transcript,
            ))]);
            let mut connection = rusqlite::Connection::open_in_memory()?;
            let tx = connection.transaction()?;
            let durable = ContextDurableRead::new(&tx, "default");
            let work = empty_work();
            let state = AgentState::new("default");
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
            {
                let _active = phase.enter();
                assert_eq!(inputs.resolved_brief_text(&brief)?, expected);
                assert_eq!(inputs.history_brief_text(&brief)?.as_str(), expected);
            }
            phase.finish()?;
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }

    #[test]
    fn resident_brief_transcript_projection_rejects_selected_text_and_slots() -> Result<()> {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let mut blocks = vec![serde_json::json!({"type":"text","text":""}); 8192];
        blocks.push(serde_json::json!({"type":"text","text":"selected"}));
        for (kind, data) in [
            (
                TranscriptEntryKind::RuntimeFailure,
                serde_json::json!({"body":{"text":"selected body".repeat(16_384)}}),
            ),
            (
                TranscriptEntryKind::AssistantRound,
                serde_json::json!({"blocks":blocks}),
            ),
        ] {
            let transcript = TranscriptEntry::new("default", kind, None, None, data);
            let mut brief = BriefRecord::new("default", BriefKind::Result, "fallback", None, None);
            brief.content_source = BriefContentSource::TranscriptEntry {
                entry_id: transcript.id.clone(),
                relation: BriefContentSourceRelation::Finalizes,
            };
            let resident = captured(vec![evidence(PreparedInputEvidence::Transcript(
                transcript,
            ))]);
            let mut connection = rusqlite::Connection::open_in_memory()?;
            let tx = connection.transaction()?;
            let durable = ContextDurableRead::new(&tx, "default");
            let work = empty_work();
            let state = AgentState::new("default");
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64_000, 2048)?;
            {
                let _active = phase.enter();
                assert!(inputs
                    .resolved_brief_text(&brief)
                    .unwrap_err()
                    .to_string()
                    .contains("ResourceExhausted"));
                assert!(inputs.resolved_brief_text(&brief).is_err());
            }
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }

    #[test]
    fn resident_brief_text_preserves_json_blank_and_block_semantics() -> Result<()> {
        let mut connection = rusqlite::Connection::open_in_memory()?;
        let tx = connection.transaction()?;
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        for (kind, body, data, expected) in [
            (
                TranscriptEntryKind::IncomingMessage,
                MessageBody::Text {
                    text: " \n ".into(),
                },
                serde_json::json!({"body":{"text":"must not replace blank Message"}}),
                "fallback",
            ),
            (
                TranscriptEntryKind::IncomingMessage,
                MessageBody::Json {
                    value: serde_json::json!({"x":"é"}),
                },
                serde_json::Value::Null,
                "{\"x\":\"é\"}",
            ),
            (
                TranscriptEntryKind::AssistantRound,
                MessageBody::Text {
                    text: "unused".into(),
                },
                serde_json::json!({"blocks":[{"type":"text","text":"first"},
                 {"type":"tool_call","text":"excluded"},{"type":"text","Text":{"text":"second"}}]}),
                "first second",
            ),
        ] {
            let mut source = message("unused");
            source.body = body;
            let transcript =
                TranscriptEntry::new("default", kind, None, Some(source.id.clone()), data);
            let mut brief = BriefRecord::new("default", BriefKind::Result, "fallback", None, None);
            brief.content_source = BriefContentSource::TranscriptEntry {
                entry_id: transcript.id.clone(),
                relation: BriefContentSourceRelation::Finalizes,
            };
            let resident = captured(vec![
                evidence(PreparedInputEvidence::Transcript(transcript)),
                evidence(PreparedInputEvidence::Message(source)),
            ]);
            let inputs = ResidentContextInputs::new(&durable, &resident, &work, &state, &[])?;
            assert_eq!(inputs.resolved_brief_text(&brief)?, expected);
        }
        Ok(())
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
            &[] as &[TurnRecord],
            &[brief],
            &[] as &[MessageEnvelope],
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
    fn durable_settlement_aggregate_keeps_exclusions_reply_bound_and_original_body() {
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
        assert!(inputs
            .admitted_settlement_inputs("default", "activation", &current.id, 1)
            .unwrap_err()
            .downcast_ref::<crate::runtime::resident_task::TaskReadError>()
            .is_some());
        // The legacy adapter remains explicit; resident reads cannot borrow it.
        let pending = durable
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
    fn resident_task_context_preserves_open_reply_selection_without_sql_tables() {
        use crate::{
            config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
            runtime::input_evidence_budget::InputEvidenceBudgetLedger,
        };
        let (tasks, mut messages) = crate::runtime::task_reads::tests::admitted_fixture().unwrap();
        messages.last_mut().unwrap().body = MessageBody::Text {
            text: "r".repeat(256 * 1024),
        };
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let state = AgentState::new("default");
        let selected = captured(
            messages
                .iter()
                .cloned()
                .map(|message| evidence(PreparedInputEvidence::Message(message)))
                .collect(),
        );
        let inputs = ResidentContextInputs::new(&durable, &selected, &work, &state, &[])
            .unwrap()
            .with_tasks(&tasks)
            .unwrap();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default()).unwrap();
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), 64 * 1024, 8192).unwrap();
        let rows = phase
            .read(|| {
                inputs.admitted_settlement_inputs(
                    "default",
                    "original-open-activation",
                    "result-current-task",
                    1,
                )
            })
            .unwrap();
        phase.finish().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].record.message_id, "result-first-task");
        assert_eq!(
            rows[0].reply.as_ref().unwrap().body,
            messages.last().unwrap().body
        );
        assert_eq!(rows[1].record.message_id, "result-duplicate-task");
        assert!(rows[1].reply.is_none());
        for message in rows
            .iter()
            .flat_map(|row| row.message.iter().chain(row.reply.iter()))
        {
            let source = selected
                .lookup_borrowed(Family::Message, &message.id)
                .unwrap()
                .unwrap();
            let PreparedInputEvidence::Message(source) = &source.record else {
                panic!("selected source must remain a Message");
            };
            assert!(std::ptr::eq::<MessageEnvelope>(&**message, source));
        }
        assert!(inputs.active_tasks("default").unwrap().is_empty());
        assert!(inputs.active_tasks("foreign").is_err());
        let last_material = Arc::downgrade(&phase.guard().unwrap());
        drop(inputs);
        drop(selected);
        drop(phase);
        assert!(last_material.upgrade().is_some());
        drop(rows);
        assert!(last_material.upgrade().is_none());
        let missing_reply = captured(
            messages
                .into_iter()
                .filter(|row| row.id != "reply-outside-window")
                .map(|message| evidence(PreparedInputEvidence::Message(message)))
                .collect(),
        );
        let inputs = ResidentContextInputs::new(&durable, &missing_reply, &work, &state, &[])
            .unwrap()
            .with_tasks(&tasks)
            .unwrap();
        assert!(inputs
            .admitted_settlement_inputs(
                "default",
                "original-open-activation",
                "result-current-task",
                1
            )
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>()
            .is_some());
    }

    #[test]
    fn resident_task_context_reads_only_selected_roots_after_current_reply_exclusion() {
        let (tasks, messages) = crate::runtime::task_reads::tests::admitted_fixture().unwrap();
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        let durable = ContextDurableRead::new(&tx, "default");
        let work = empty_work();
        let mut state = AgentState::new("default");
        state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: Some("original-open-activation".into()),
            admission_provenance: None,
            source_message_id: messages[0].id.clone(),
            turn_id: "selector-current-turn".into(),
            owner: None,
            work_item_id: None,
            claimed_work_revision: None,
        });
        let ledger = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
            crate::config::StateMemoryLimits::default(),
        )
        .unwrap();
        for (current, expected_roots, expected_results) in [
            (
                "result-current-task",
                vec![
                    "result-first-task",
                    "result-duplicate-task",
                    "reply-outside-window",
                ],
                vec!["result-first-task", "result-duplicate-task"],
            ),
            (
                "reply-outside-window",
                vec!["result-current-task", "result-skip-task"],
                vec!["result-current-task", "result-skip-task"],
            ),
        ] {
            let roots =
                pending_task_input_roots(&tasks, &state, current, &ledger, 64, 64 * 1024).unwrap();
            assert_eq!(
                roots.roots,
                expected_roots
                    .into_iter()
                    .map(|id| Key::new(Family::Message, id))
                    .collect()
            );
            let selected = captured(
                messages
                    .iter()
                    .filter(|message| {
                        roots
                            .roots
                            .contains(&Key::new(Family::Message, &message.id))
                    })
                    .cloned()
                    .map(|message| evidence(PreparedInputEvidence::Message(message)))
                    .collect(),
            );
            let inputs = ResidentContextInputs::new(&durable, &selected, &work, &state, &[])
                .unwrap()
                .with_tasks(&tasks)
                .unwrap();
            let rows = inputs
                .admitted_settlement_inputs("default", "original-open-activation", current, 1)
                .unwrap();
            assert_eq!(
                rows.iter()
                    .map(|row| row.record.message_id.as_str())
                    .collect::<Vec<_>>(),
                expected_results
            );
        }
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
