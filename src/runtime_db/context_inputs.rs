//! Read-only, transaction-bound inputs for one coherent context acquisition.

use std::collections::BTreeSet;

use anyhow::{ensure, Result};
use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::{
    diagnostics::attribution,
    domain::execution_protocol::ExecutionProtocolState,
    storage::{
        derive_context_work_queue, filter_context_active_waits, WorkQueueReadModel,
        WORK_QUEUE_NON_OPEN_WINDOW,
    },
    types::{
        AgentIdentityRecord, AgentState, BriefContentSource, BriefRecord, DeliverySummaryRecord,
        ExecutionRootEntry, ExternalTriggerRecord, MessageEnvelope, TaskRecord,
        ToolExecutionRecord, TranscriptEntry, TurnOwner, TurnRecord, WaitConditionRecord,
        WorkItemRecord, WorkspaceEntry,
    },
};

use super::{
    evidence::EvidenceKind,
    repositories::{self, decode_agent_identity_payload, decode_agent_state_payload},
    task_result_settlement::{context_admitted_settlements_on, TaskResultSettlementRecord},
    transitions::execution_protocol_repository::context_execution_protocol_on,
};

/// Root compatibility is checked by the caller against these exact read-view facts.
#[derive(Debug, Clone)]
pub(crate) struct ContextScopeFacts {
    pub identity: Option<AgentIdentityRecord>,
    pub state: Option<AgentState>,
    pub control_revision: Option<u64>,
}

/// Temporary D4 Work aggregate; its exact source is prepared in the same read view.
#[derive(Debug, Clone)]
pub(crate) struct CompletionBriefInput {
    pub brief: BriefRecord,
    pub content_source: Option<TranscriptEntry>,
    pub source_message: Option<MessageEnvelope>,
}

/// Temporary D5 settlement aggregate, separate from resident Message lookup.
#[derive(Debug, Clone)]
pub(crate) struct AdmittedSettlementInput {
    pub record: TaskResultSettlementRecord,
    pub message: Option<MessageEnvelope>,
    pub reply: Option<MessageEnvelope>,
}

/// This adapter cannot open a connection, write, or read a filesystem resource.
pub(crate) struct ContextDurableRead<'a> {
    connection: &'a Connection,
    agent_id: &'a str,
}

impl<'a> ContextDurableRead<'a> {
    pub(crate) fn new(transaction: &'a Transaction<'_>, agent_id: &'a str) -> Self {
        Self {
            connection: transaction,
            agent_id,
        }
    }

    pub(crate) fn agent_id(&self) -> &str {
        self.agent_id
    }

    pub(crate) fn scope_facts(&self) -> Result<ContextScopeFacts> {
        ensure!(!self.agent_id.is_empty(), "context agent scope is empty");
        let identity = self
            .connection
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                [self.agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| decode_agent_identity_payload(&payload))
            .transpose()?;
        let identity = self.record(identity, |record| &record.agent_id)?;
        let (state, control_revision) = self.state_facts()?;
        Ok(ContextScopeFacts {
            identity,
            state,
            control_revision,
        })
    }

    fn state_facts(&self) -> Result<(Option<AgentState>, Option<u64>)> {
        let row = self
            .connection
            .query_row(
                "SELECT payload_json, control_revision FROM agent_states WHERE agent_id = ?1",
                [self.agent_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let (state, control_revision) = match row {
            Some((payload, revision)) => {
                let state = decode_agent_state_payload(&payload)?;
                self.check_scope(&state.id)?;
                (Some(state), Some(u64::try_from(revision)?))
            }
            None => (None, None),
        };
        Ok((state, control_revision))
    }

    pub(crate) fn workspace_entries(&self) -> Result<Vec<WorkspaceEntry>> {
        repositories::context_workspace_entries_on(self.connection)
    }

    pub(crate) fn execution_roots_for_workspaces(
        &self,
        ids: &[String],
    ) -> Result<Vec<ExecutionRootEntry>> {
        let mut roots = Vec::new();
        for id in ids {
            let entries = repositories::context_execution_roots_on(self.connection, id)?;
            ensure!(
                entries
                    .iter()
                    .all(|entry| entry.workspace_id == *id && entry.removed_at.is_none()),
                "context execution root workspace projection mismatch"
            );
            roots.extend(entries);
        }
        Ok(roots)
    }

    pub(crate) fn anchor_messages(&self) -> Result<Vec<MessageEnvelope>> {
        self.records(
            repositories::context_anchor_messages_on(self.connection, Some(self.agent_id))?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn message_by_id(&self, id: &str) -> Result<Option<MessageEnvelope>> {
        self.record(
            repositories::context_message_by_id_on(self.connection, Some(self.agent_id), id)?,
            |record| &record.agent_id,
        )
    }

    /// Same-transaction assignment observation. A missing row is explicitly unassigned.
    pub(crate) fn assigned_turn_for_message(&self, id: &str) -> Result<Option<String>> {
        let assignment = self
            .connection
            .query_row(
                "SELECT agent_id,turn_id FROM conversation_input_assignments WHERE message_id=?1",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        assignment
            .map(|(agent_id, turn_id)| {
                self.check_scope(&agent_id)?;
                ensure!(
                    !turn_id.trim().is_empty(),
                    "context assignment Turn ID is empty"
                );
                Ok(turn_id)
            })
            .transpose()
    }

    pub(crate) fn briefs(&self, limit: usize) -> Result<Vec<BriefRecord>> {
        self.records(
            self.evidence::<BriefRecord>(EvidenceKind::Brief, limit)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn brief_by_id(&self, id: &str) -> Result<Option<BriefRecord>> {
        self.record(
            self.evidence_by_id::<BriefRecord>(EvidenceKind::Brief, id)?,
            |record| &record.agent_id,
        )
    }

    /// D4 must replace this Work index and its source closure with resident facts.
    pub(crate) fn latest_completion_brief_for_work_item(
        &self,
        work_item_id: &str,
    ) -> Result<Option<CompletionBriefInput>> {
        let payload = self
            .connection
            .query_row(
                "SELECT payload_json FROM briefs
             WHERE agent_id=?1 AND work_item_id=?2 AND kind='result'
               AND length(json_extract(payload_json,'$.text')) > 0
             ORDER BY created_at DESC,evidence_id DESC LIMIT 1",
                rusqlite::params![self.agent_id, work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let brief: BriefRecord = serde_json::from_str(&payload)?;
        self.check_scope(&brief.agent_id)?;
        ensure!(
            brief.work_item_id.as_deref() == Some(work_item_id)
                && brief.kind == crate::types::BriefKind::Result
                && !brief.text.is_empty(),
            "completion brief projection mismatch"
        );
        let content_source = match &brief.content_source {
            BriefContentSource::TranscriptEntry { entry_id, .. } => {
                self.transcript_by_id(entry_id)?
            }
            _ => None,
        };
        let source_message = match &content_source {
            Some(entry) if entry.kind == crate::types::TranscriptEntryKind::IncomingMessage => {
                entry
                    .related_message_id
                    .as_deref()
                    .map(|id| self.message_by_id(id))
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

    pub(crate) fn tools(&self, limit: usize) -> Result<Vec<ToolExecutionRecord>> {
        self.records(
            self.evidence::<ToolExecutionRecord>(EvidenceKind::ToolExecution, limit)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn tool_by_id(&self, id: &str) -> Result<Option<ToolExecutionRecord>> {
        self.record(
            self.evidence_by_id::<ToolExecutionRecord>(EvidenceKind::ToolExecution, id)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn turns(&self, limit: usize) -> Result<Vec<TurnRecord>> {
        self.records(
            repositories::context_recent_turns_on(self.connection, self.agent_id, limit)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn turns_for_owner(
        &self,
        owner: &TurnOwner,
        limit: usize,
    ) -> Result<Vec<TurnRecord>> {
        if let TurnOwner::AgentLifecycle { agent_id } = owner {
            self.check_scope(agent_id)?;
        }
        self.records(
            repositories::context_owner_turns_on(self.connection, self.agent_id, owner, limit)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn turn_by_id(&self, id: &str) -> Result<Option<TurnRecord>> {
        self.record(
            repositories::context_turn_by_id_on(self.connection, Some(self.agent_id), id)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn transcript(&self, limit: usize) -> Result<Vec<TranscriptEntry>> {
        self.records(
            repositories::context_recent_transcript_on(
                self.connection,
                Some(self.agent_id),
                limit,
            )?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn transcript_for_turns(&self, ids: &[String]) -> Result<Vec<TranscriptEntry>> {
        self.records(
            repositories::context_transcript_for_turns_on(self.connection, self.agent_id, ids)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn transcript_by_id(&self, id: &str) -> Result<Option<TranscriptEntry>> {
        self.record(
            repositories::context_transcript_by_id_on(self.connection, Some(self.agent_id), id)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn execution_protocol_state(
        &self,
        agent_id: &str,
    ) -> Result<Option<ExecutionProtocolState>> {
        self.check_scope(agent_id)?;
        self.record(
            context_execution_protocol_on(self.connection, self.agent_id)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn active_waits(&self, agent_id: &str) -> Result<Vec<WaitConditionRecord>> {
        self.check_scope(agent_id)?;
        let mut query_timer = attribution::WAIT_QUERY_AGENT.start();
        let records = self.records(
            repositories::context_active_waits_on(self.connection, self.agent_id)?,
            |record| &record.agent_id,
        )?;
        query_timer.rows(records.len());
        drop(query_timer);
        let mut filter_timer = attribution::WAIT_FILTER.start();
        filter_timer.rows(records.len());
        let ids = records
            .iter()
            .filter_map(|record| record.work_item_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut lookup = attribution::WAIT_ITEM.start();
        let work_items = self.records(
            repositories::context_work_items_by_ids_on(self.connection, self.agent_id, &ids)?,
            |record| &record.agent_id,
        )?;
        lookup.rows(work_items.len());
        drop(lookup);
        Ok(filter_context_active_waits(records, work_items))
    }

    pub(crate) fn work_queue(&self) -> Result<WorkQueueReadModel> {
        let _timer = attribution::WORK_QUEUE.start();
        let current_id = self
            .state_facts()?
            .0
            .and_then(|state| state.current_work_item_id);
        let mut timer = attribution::WORK_ITEMS.start();
        let mut items = self.records(
            repositories::context_open_work_items_on(self.connection, self.agent_id)?,
            |record| &record.agent_id,
        )?;
        items.extend(self.records(
            repositories::context_non_open_work_items_on(
                self.connection,
                self.agent_id,
                WORK_QUEUE_NON_OPEN_WINDOW,
            )?,
            |record| &record.agent_id,
        )?);
        timer.rows(items.len());
        drop(timer);
        let triggers = self.external_triggers()?;
        let waits = self.active_waits(self.agent_id)?;
        let continuations = self.records(
            repositories::context_active_continuations_on(self.connection, self.agent_id)?,
            |record| &record.agent_id,
        )?;
        Ok(derive_context_work_queue(
            current_id,
            items
                .into_iter()
                .map(|item| (item.id.clone(), item))
                .collect(),
            triggers,
            waits,
            continuations,
        ))
    }

    pub(crate) fn work_item(&self, id: &str) -> Result<Option<WorkItemRecord>> {
        self.record(
            repositories::context_work_item_by_id_on(self.connection, self.agent_id, id)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn external_triggers(&self) -> Result<Vec<ExternalTriggerRecord>> {
        self.records(
            repositories::context_external_triggers_on(self.connection, self.agent_id)?,
            |record| &record.target_agent_id,
        )
    }

    pub(crate) fn active_tasks(&self, agent_id: &str) -> Result<Vec<TaskRecord>> {
        self.check_scope(agent_id)?;
        self.records(
            repositories::context_tasks_query_on(
                self.connection,
                self.agent_id,
                "owner_agent_id = ?1 AND status IN ('queued', 'running', 'cancelling')",
                [self.agent_id],
                usize::MAX,
            )?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn delivery_summaries(&self, limit: usize) -> Result<Vec<DeliverySummaryRecord>> {
        self.records(
            self.evidence::<DeliverySummaryRecord>(EvidenceKind::DeliverySummary, limit)?,
            |record| &record.agent_id,
        )
    }

    pub(crate) fn admitted_settlements(
        &self,
        agent_id: &str,
        activation_id: &str,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.check_scope(agent_id)?;
        let records = self.records(
            context_admitted_settlements_on(self.connection, self.agent_id, activation_id)?,
            |record| &record.agent_id,
        )?;
        ensure!(
            records
                .iter()
                .all(|record| record.activation_id.as_deref() == Some(activation_id)),
            "context settlement activation projection mismatch"
        );
        Ok(records)
    }

    /// Message reads here belong only to the un-migrated settlement aggregate.
    pub(crate) fn admitted_settlement_inputs(
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
            |id| self.message_by_id(id),
        )
    }

    fn evidence<T: serde::de::DeserializeOwned>(
        &self,
        kind: EvidenceKind,
        limit: usize,
    ) -> Result<Vec<T>> {
        repositories::context_recent_evidence_on(self.connection, kind, self.agent_id, limit)?
            .into_iter()
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .collect()
    }

    fn evidence_by_id<T: serde::de::DeserializeOwned>(
        &self,
        kind: EvidenceKind,
        id: &str,
    ) -> Result<Option<T>> {
        repositories::context_evidence_by_id_on(self.connection, kind, self.agent_id, id)?
            .map(|row| serde_json::from_str(&row.payload_json).map_err(Into::into))
            .transpose()
    }

    fn check_scope(&self, actual: &str) -> Result<()> {
        ensure!(
            !self.agent_id.is_empty() && actual == self.agent_id,
            "context input agent scope mismatch"
        );
        Ok(())
    }

    fn records<T>(
        &self,
        records: Vec<T>,
        agent_id: impl for<'r> Fn(&'r T) -> &'r str,
    ) -> Result<Vec<T>> {
        for record in &records {
            self.check_scope(agent_id(record))?;
        }
        Ok(records)
    }

    fn record<T>(
        &self,
        record: Option<T>,
        agent_id: impl for<'r> Fn(&'r T) -> &'r str,
    ) -> Result<Option<T>> {
        if let Some(record) = record.as_ref() {
            self.check_scope(agent_id(record))?;
        }
        Ok(record)
    }
}

/// Preserve the old renderer's exclusion, bound and reply de-duplication before
/// touching reply bodies. The callback is acquisition-only and already scoped.
pub(crate) fn prepare_admitted_settlement_inputs(
    records: Vec<TaskResultSettlementRecord>,
    current_message_id: &str,
    reply_limit: usize,
    mut message_by_id: impl FnMut(&str) -> Result<Option<MessageEnvelope>>,
) -> Result<Vec<AdmittedSettlementInput>> {
    let mut pending = Vec::new();
    let mut seen_replies = BTreeSet::new();
    for record in records {
        if record.message_id == current_message_id {
            continue;
        }
        let message = message_by_id(&record.message_id)?;
        if message
            .as_ref()
            .and_then(crate::wake_contract::agent_message_reply_reference)
            == Some(current_message_id)
        {
            continue;
        }
        let reply = if pending.len() < reply_limit {
            message
                .as_ref()
                .and_then(crate::wake_contract::agent_message_reply_reference)
                .filter(|id| seen_replies.insert((*id).to_owned()))
                .map(&mut message_by_id)
                .transpose()?
                .flatten()
        } else {
            None
        };
        pending.push(AdmittedSettlementInput {
            record,
            message,
            reply,
        });
    }
    Ok(pending)
}

#[cfg(test)]
#[path = "context_inputs_tests.rs"]
mod tests;
