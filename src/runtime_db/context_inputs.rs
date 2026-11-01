//! Read-only, transaction-bound inputs for one coherent context acquisition.

use std::{borrow::Cow, collections::BTreeSet, sync::Arc};

use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::{
    diagnostics::attribution,
    domain::execution_protocol::ExecutionProtocolState,
    runtime::resident_input_evidence::InputEvidenceScope,
    storage::{
        clone_queue_work_item_key, derive_context_work_queue, filter_context_active_waits,
        QueueWorkBuilder, WorkQueueReadModel, WORK_QUEUE_NON_OPEN_WINDOW,
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
    pub state: Option<Arc<AgentState>>,
    pub control_revision: Option<u64>,
}

impl ContextScopeFacts {
    pub(crate) fn into_owned_state(self) -> Result<Option<AgentState>> {
        self.state.map(own_state).transpose()
    }
}

pub(crate) fn own_state(state: Arc<AgentState>) -> Result<AgentState> {
    match Arc::try_unwrap(state) {
        Ok(state) => Ok(state),
        Err(state) => {
            crate::diagnostics::resource_io::admit_cloned_value(state.as_ref())?;
            Ok(state.as_ref().clone())
        }
    }
}

/// This proves a body decode, not scope authority. Reuse rechecks the current
/// SQL bytes, revision and incarnation before establishing a new scope read.
#[derive(Clone)]
pub(crate) struct ContextStateDecode {
    state: Arc<AgentState>,
    scope: Arc<InputEvidenceScope>,
    control_revision: u64,
    digest: [u8; 32],
    _material: Option<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

/// One canonical scope read, borrowed only within its unchanged transaction.
pub(crate) struct ContextScopeRead<'a> {
    transaction: &'a Transaction<'a>,
    facts: ContextScopeFacts,
    fingerprint: [u8; 32],
    decoded_state: Option<ContextStateDecode>,
}

impl ContextScopeRead<'_> {
    pub(crate) fn facts(&self) -> &ContextScopeFacts {
        &self.facts
    }

    pub(crate) fn decoded_state(&self) -> Option<ContextStateDecode> {
        self.decoded_state.clone()
    }

    pub(crate) fn into_facts(
        self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<ContextScopeFacts> {
        self.validate(tx, scope)?;
        Ok(self.facts)
    }

    pub(crate) fn validate(&self, tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<()> {
        ensure!(
            self.facts
                .state
                .as_ref()
                .is_some_and(|state| state.id == scope.agent_id),
            "context scope read belongs to another transaction or scope"
        );
        self.validate_identity_scope(tx, scope)
    }

    // Native birth can precede the first State. Its identity proof still
    // freezes State absence and rejects any intervening canonical write.
    pub(crate) fn validate_identity_scope(
        &self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<()> {
        let identity = self.facts.identity.as_ref();
        ensure!(
            std::ptr::eq(tx, self.transaction)
                && identity.is_some_and(|identity| identity.agent_id == scope.agent_id
                    && identity.incarnation == scope.identity_incarnation)
                && self
                    .facts
                    .state
                    .as_ref()
                    .is_none_or(|state| state.id == scope.agent_id),
            "context scope read belongs to another transaction or scope"
        );
        ensure!(
            scope_read_fingerprint(tx, &scope.agent_id)? == self.fingerprint,
            "context scope changed after its validated read"
        );
        let identity = identity.expect("validated scope identity");
        let expected = match identity.status {
            crate::types::AgentRegistryStatus::Active => "active",
            crate::types::AgentRegistryStatus::Deleting => "deleting",
            crate::types::AgentRegistryStatus::Deleted => "deleted",
        };
        let matches: bool = tx.query_row(
            "SELECT status FROM agent_identities WHERE agent_id=?1",
            [&scope.agent_id],
            |row| Ok(row.get_ref(0)?.as_str()? == expected),
        )?;
        ensure!(
            matches,
            "context identity normalized/payload projection mismatch"
        );
        Ok(())
    }
}

fn scope_read_fingerprint(connection: &Connection, agent: &str) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    Ok(connection
        .query_row(
            "SELECT i.agent_id,i.payload_json,i.status,s.payload_json,s.control_revision
             FROM agent_identities i LEFT JOIN agent_states s ON s.agent_id=i.agent_id
             WHERE i.agent_id=?1",
            [agent],
            |row| {
                let mut hash = Sha256::new();
                for index in 0..4 {
                    match row.get_ref(index)? {
                        rusqlite::types::ValueRef::Null => hash.update([0]),
                        value => {
                            let value = value.as_str()?;
                            hash.update([1]);
                            hash.update(value.len().to_le_bytes());
                            hash.update(value.as_bytes());
                        }
                    }
                }
                hash.update(
                    row.get::<_, Option<i64>>(4)?
                        .map(i64::to_le_bytes)
                        .unwrap_or_default(),
                );
                Ok(hash.finalize().into())
            },
        )
        .optional()?
        .unwrap_or_default())
}

fn state_payload_digest(payload: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(payload.as_bytes()).into()
}

/// Qualified resident sources are borrowed; cold sources own their decoded rows.
#[derive(Debug, Clone)]
pub(crate) struct CompletionBriefInput<'a> {
    pub brief: Cow<'a, BriefRecord>,
    pub content_source: Option<Cow<'a, TranscriptEntry>>,
    pub source_message: Option<Cow<'a, MessageEnvelope>>,
}

impl CompletionBriefInput<'_> {
    pub(crate) fn into_brief(self) -> Result<BriefRecord> {
        match self.brief {
            Cow::Borrowed(brief) => {
                crate::diagnostics::resource_io::admit_cloned_value(brief)?;
                Ok(brief.clone())
            }
            Cow::Owned(brief) => Ok(brief),
        }
    }
}

/// Temporary D5 settlement aggregate, separate from resident Message lookup.
#[derive(Debug, Clone)]
pub(crate) struct AdmittedSettlementInput<M = MessageEnvelope> {
    pub record: TaskResultSettlementRecord,
    pub message: Option<M>,
    pub reply: Option<M>,
}

/// This adapter cannot open a connection, write, or read a filesystem resource.
pub(crate) struct ContextDurableRead<'a> {
    connection: &'a Transaction<'a>,
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
        let identity = self.identity_fact()?;
        let (state, control_revision) = self.state_facts()?;
        Ok(ContextScopeFacts {
            identity,
            state,
            control_revision,
        })
    }

    pub(crate) fn owned_state(&self) -> Result<Option<AgentState>> {
        let (state, _) = self.state_facts()?;
        state.map(own_state).transpose()
    }

    pub(crate) fn scope_read(&self) -> Result<ContextScopeRead<'a>> {
        self.scope_read_using_decode(None)
    }

    pub(crate) fn scope_read_using_decode(
        &self,
        proof: Option<&ContextStateDecode>,
    ) -> Result<ContextScopeRead<'a>> {
        let identity = self.identity_fact()?;
        let (state, control_revision, decoded_state) =
            self.state_facts_using_decode(identity.as_ref(), proof)?;
        let facts = ContextScopeFacts {
            identity,
            state,
            control_revision,
        };
        let fingerprint = scope_read_fingerprint(self.connection, self.agent_id)?;
        Ok(ContextScopeRead {
            transaction: self.connection,
            facts,
            fingerprint,
            decoded_state,
        })
    }

    pub(crate) fn identity_fact(&self) -> Result<Option<AgentIdentityRecord>> {
        ensure!(!self.agent_id.is_empty(), "context agent scope is empty");
        let identity = self
            .scope_payload(
                "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                false,
                decode_agent_identity_payload,
            )?
            .map(|(identity, _)| identity);
        self.record(identity, |record| &record.agent_id)
    }

    fn state_facts(&self) -> Result<(Option<Arc<AgentState>>, Option<u64>)> {
        let (state, revision, _) = self.state_facts_using_decode(None, None)?;
        Ok((state, revision))
    }

    fn state_facts_using_decode(
        &self,
        identity: Option<&AgentIdentityRecord>,
        proof: Option<&ContextStateDecode>,
    ) -> Result<(
        Option<Arc<AgentState>>,
        Option<u64>,
        Option<ContextStateDecode>,
    )> {
        let mut statement = self.connection.prepare(
            "SELECT payload_json, control_revision FROM agent_states WHERE agent_id = ?1",
        )?;
        let mut rows = statement.query([self.agent_id])?;
        let Some(row) = rows.next()? else {
            return Ok((None, None, None));
        };
        let payload = repositories::borrowed_payload(row)?;
        let revision: i64 = row.get(1)?;
        let digest = state_payload_digest(payload);
        let reusable = proof.filter(|proof| {
            identity.is_some_and(|identity| {
                identity.agent_id == proof.scope.agent_id
                    && identity.incarnation == proof.scope.identity_incarnation
            }) && proof.scope.agent_id == self.agent_id
                && i64::try_from(proof.control_revision).ok() == Some(revision)
                && proof.digest == digest
        });
        let state = match reusable {
            Some(proof) => Arc::clone(&proof.state),
            None => {
                // A typed State or serialization match cannot mint this proof.
                // This decoder owns one State, not a completion/resolver tree.
                // Admit raw JSON before SQLite traversal and owning nodes before
                // canonical decode, as the Scheduling scope reader does.
                crate::diagnostics::resource_io::charge_owned_metadata(
                    u64::try_from(payload.len())?
                        .checked_add(256)
                        .context("Context State raw bytes overflow")?,
                    1,
                )?;
                let nodes: usize = match self.connection.query_row(
                    "SELECT COUNT(*) FROM json_tree((SELECT payload_json FROM agent_states WHERE agent_id=?1))",
                    [self.agent_id],
                    |row| row.get(0),
                ) {
                    Ok(nodes) => nodes,
                    Err(error) => {
                        // Keep canonical field/syntax error precedence on corrupt JSON.
                        crate::diagnostics::resource_io::admit_json_payload(payload)?;
                        decode_agent_state_payload(payload)?;
                        return Err(error.into());
                    }
                };
                crate::diagnostics::resource_io::charge_owned_metadata(
                    u64::try_from(nodes)?
                        .checked_mul(u64::try_from(std::mem::size_of::<serde_json::Value>())?)
                        .context("Context State node bytes overflow")?,
                    nodes,
                )?;
                Arc::new(decode_agent_state_payload(payload)?)
            }
        };
        self.check_scope(&state.id)?;
        let control_revision = u64::try_from(revision)?;
        let decoded_state = match reusable {
            Some(proof) => Some(proof.clone()),
            None => identity
                .map(|identity| -> Result<_> {
                    crate::diagnostics::resource_io::admit_cloned_value(&identity.agent_id)?;
                    Ok(ContextStateDecode {
                        state: Arc::clone(&state),
                        scope: Arc::new(InputEvidenceScope {
                            agent_id: identity.agent_id.clone(),
                            identity_incarnation: identity.incarnation,
                        }),
                        control_revision,
                        digest,
                        _material:
                            crate::diagnostics::resource_io::ResourceReadMaterial::active_guard()?,
                    })
                })
                .transpose()?,
        };
        Ok((Some(state), Some(control_revision), decoded_state))
    }

    fn scope_payload<T>(
        &self,
        sql: &str,
        revision: bool,
        decode: impl FnOnce(&str) -> Result<T>,
    ) -> Result<Option<(T, Option<i64>)>> {
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = statement.query([self.agent_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let payload = repositories::borrowed_payload(row)?;
        // Keep the original column/error order: revision precedes JSON decode.
        let revision = revision.then(|| row.get::<_, i64>(1)).transpose()?;
        crate::diagnostics::resource_io::admit_json_payload(payload)?;
        Ok(Some((decode(payload)?, revision)))
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
    ) -> Result<Option<CompletionBriefInput<'static>>> {
        let brief = {
            let mut statement = self.connection.prepare(
                "SELECT payload_json FROM briefs
             WHERE agent_id=?1 AND work_item_id=?2 AND kind='result'
               AND length(json_extract(payload_json,'$.text')) > 0
             ORDER BY created_at DESC,evidence_id DESC LIMIT 1",
            )?;
            let mut rows = statement.query(rusqlite::params![self.agent_id, work_item_id])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let payload = repositories::borrowed_payload(row)?;
            crate::diagnostics::resource_io::admit_json_payload(payload)?;
            serde_json::from_str::<BriefRecord>(payload)?
        };
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
            brief: Cow::Owned(brief),
            content_source: content_source.map(Cow::Owned),
            source_message: source_message.map(Cow::Owned),
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
            .and_then(|state| state.current_work_item_id.clone());
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
        let mut latest = QueueWorkBuilder::default();
        for item in items {
            latest.push(clone_queue_work_item_key(&item.id)?, item)?;
        }
        derive_context_work_queue(
            current_id.as_deref(),
            latest.finish(),
            triggers,
            waits,
            continuations,
        )
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

    /// Named legacy acquisition, not a resident hit. Selected Work/Input and
    /// this transaction's owner/revision are validated by the loaded caller.
    pub(crate) fn completion_delivery_summaries<'id>(
        &self,
        work_item_ids: impl Iterator<Item = &'id str>,
    ) -> Result<Vec<DeliverySummaryRecord>> {
        crate::diagnostics::resource_io::check_active_budget()?;
        let mut selected = Vec::new();
        let mut work_item_ids = work_item_ids.peekable();
        if work_item_ids.peek().is_none() {
            return Ok(selected);
        }
        let mut statement = self.connection.prepare(
            "SELECT payload_json FROM delivery_summaries
             WHERE agent_id=?1 AND work_item_id=?2
             ORDER BY created_at DESC,evidence_id DESC",
        )?;
        for work_item_id in work_item_ids {
            let mut rows = statement.query(rusqlite::params![self.agent_id, work_item_id])?;
            while let Some(row) = rows.next()? {
                // Borrow SQLite's row; admit before Rust String/JSON allocation.
                let payload = row.get_ref(0)?.as_str()?;
                crate::diagnostics::resource_io::admit_json_payload(payload)?;
                let summary: DeliverySummaryRecord = serde_json::from_str(payload)?;
                self.check_scope(&summary.agent_id)?;
                ensure!(
                    summary.work_item_id == work_item_id,
                    "completion delivery Work selector mismatch"
                );
                if !summary.text.is_empty() {
                    selected.push(summary);
                    break;
                }
            }
        }
        Ok(selected)
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
pub(crate) fn prepare_admitted_settlement_inputs<M: std::borrow::Borrow<MessageEnvelope>>(
    records: Vec<TaskResultSettlementRecord>,
    current_message_id: &str,
    reply_limit: usize,
    mut message_by_id: impl FnMut(&str) -> Result<Option<M>>,
) -> Result<Vec<AdmittedSettlementInput<M>>> {
    let mut pending = Vec::new();
    let mut seen_replies = BTreeSet::new();
    for record in records {
        if record.message_id == current_message_id {
            continue;
        }
        let message = message_by_id(&record.message_id)?;
        if message
            .as_ref()
            .map(std::borrow::Borrow::borrow)
            .and_then(crate::wake_contract::agent_message_reply_reference)
            == Some(current_message_id)
        {
            continue;
        }
        let reply = if pending.len() < reply_limit {
            message
                .as_ref()
                .map(std::borrow::Borrow::borrow)
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
