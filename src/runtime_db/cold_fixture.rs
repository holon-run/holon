//! Typed, insert-only setup for stopped, unloaded synthetic fixtures.
//! Qualification is part of each original insert, never a later adoption pass.

use std::collections::BTreeSet;

use anyhow::{ensure, Context, Result};
use rusqlite::{params, Transaction};

use super::{
    context_inputs::ContextDurableRead,
    current_read_writes::{CurrentFamily, OriginalCurrentWrites},
    evidence,
    input_evidence_snapshot::{
        native_birth::{original_material_limits, NativeInputWriteProof},
        register_committed_input_evidence_tx, CanonicalInputEvidence as Fact,
    },
    input_evidence_writes::{
        acknowledge_input_evidence_tx, merge_input_evidence_delta, synchronize_message_count_tx,
        validate_input_evidence_scope_tx,
    },
    repositories, work_snapshot, RuntimeDb,
};
use crate::{
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::{InputEvidenceFamily as Family, InputEvidenceScope},
        resident_work::{WorkCaptureRequest, WorkFamily, WorkKey},
    },
    types::{
        AgentState, AgentStatus, BriefKind, BriefRecord, MessageEnvelope, QueueEntryRecord,
        QueueEntryStatus, ToolExecutionRecord, TranscriptEntry, TurnRecord, WorkItemRecord,
    },
};

/// No SQL callbacks or caller-supplied scopes. Existing rows cannot be adopted.
pub struct ColdFixtureWriter<'db> {
    db: &'db RuntimeDb,
    scope: InputEvidenceScope,
}

/// Only these original evidence inserts are supported by cold setup.
#[derive(Clone, Copy)]
pub enum ColdFixtureEvidence<'a> {
    Message(&'a MessageEnvelope),
    Brief(&'a BriefRecord),
    Turn(&'a TurnRecord),
    Tool(&'a ToolExecutionRecord),
    Transcript(&'a TranscriptEntry),
}

impl ColdFixtureEvidence<'_> {
    fn address(&self) -> (&str, &str, Family, &'static str, &'static str) {
        match self {
            Self::Message(r) => (
                &r.agent_id,
                &r.id,
                Family::Message,
                "messages",
                "evidence_id",
            ),
            Self::Brief(r) => (&r.agent_id, &r.id, Family::Brief, "briefs", "evidence_id"),
            Self::Turn(r) => (
                &r.agent_id,
                &r.turn_id,
                Family::Turn,
                "turn_records",
                "turn_id",
            ),
            Self::Tool(r) => (
                &r.agent_id,
                &r.id,
                Family::ToolExecution,
                "tool_executions",
                "evidence_id",
            ),
            Self::Transcript(r) => (
                &r.agent_id,
                &r.id,
                Family::Transcript,
                "transcript_entries",
                "evidence_id",
            ),
        }
    }

    fn insert_tx(self, tx: &Transaction<'_>) -> Result<Fact> {
        Ok(match self {
            Self::Message(r) => {
                evidence::upsert_message_tx(tx, r)?;
                Fact::Message(evidence::committed_message_tx(tx, &r.agent_id, &r.id)?)
            }
            Self::Brief(r) => {
                evidence::insert_brief_evidence_tx(tx, r)?;
                Fact::Brief(evidence::committed_brief_tx(tx, &r.agent_id, &r.id)?)
            }
            Self::Turn(r) => {
                repositories::upsert_turn_record_tx(tx, r)?;
                Fact::Turn(
                    repositories::committed_turn_record_tx(tx, &r.agent_id, &r.turn_id)?.record,
                )
            }
            Self::Tool(r) => {
                evidence::insert_tool_evidence_tx(tx, r)?;
                Fact::ToolExecution(evidence::committed_tool_tx(tx, &r.agent_id, &r.id)?)
            }
            Self::Transcript(r) => Fact::Transcript(evidence::append_transcript_entry_tx(tx, r)?.0),
        })
    }
}

impl<'db> ColdFixtureWriter<'db> {
    /// The caller must have shut down and drained every runtime for this scope.
    pub fn new(db: &'db RuntimeDb, agent_id: &str) -> Result<Self> {
        let scope = db.transaction(|tx| {
            let facts = ContextDurableRead::new(tx, agent_id).scope_facts()?;
            let identity = facts.identity.context("cold fixture requires identity")?;
            let scope = InputEvidenceScope {
                agent_id: agent_id.to_owned(),
                identity_incarnation: identity.incarnation,
            };
            validate_cold_tx(tx, &scope)?;
            Ok(scope)
        })?;
        Ok(Self { db, scope })
    }

    pub fn insert_work(&self, record: &WorkItemRecord) -> Result<()> {
        self.db.transaction(|tx| {
            validate_cold_tx(tx, &self.scope)?;
            ensure!(
                record.agent_id == self.scope.agent_id,
                "cold Work crossed scope"
            );
            let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &record.id)]);
            let proof = work_snapshot::prepare_work_write_tx(tx, &self.scope, &keys)?;
            ensure!(
                repositories::insert_new_work_item_tx(tx, record)?,
                "cold fixture cannot replace existing Work"
            );
            let state = ContextDurableRead::new(tx, &self.scope.agent_id)
                .scope_facts()?
                .state
                .context("cold fixture requires State")?;
            work_snapshot::register_work_writes_tx(
                tx,
                &proof,
                &keys,
                &BTreeSet::new(),
                &state,
                &work_request(),
            )?;
            Ok(())
        })
    }

    pub fn insert(&self, record: ColdFixtureEvidence<'_>) -> Result<()> {
        self.write_inputs(&[record], None, false)
    }

    /// Message, queued disposition, Current and Input ACKs share one transaction.
    pub fn insert_pending(
        &self,
        message: &MessageEnvelope,
        queue: &QueueEntryRecord,
    ) -> Result<()> {
        ensure!(
            queue.agent_id == message.agent_id
                && queue.message_id == message.id
                && queue.status == QueueEntryStatus::Queued,
            "cold pending queue differs from Message"
        );
        self.write_inputs(&[ColdFixtureEvidence::Message(message)], Some(queue), false)
    }

    /// Bounded prepared inserts for new, unassigned historical Messages.
    /// Source activity and message sequences are allocated once per batch.
    pub fn insert_unassigned_messages(&self, messages: &[MessageEnvelope]) -> Result<()> {
        ensure!(
            !messages.is_empty() && messages.len() <= 128,
            "cold Message batch must contain 1..=128 records"
        );
        let records = messages
            .iter()
            .map(ColdFixtureEvidence::Message)
            .collect::<Vec<_>>();
        self.write_inputs(&records, None, true)
    }

    fn write_inputs(
        &self,
        records: &[ColdFixtureEvidence<'_>],
        queue: Option<&QueueEntryRecord>,
        bulk: bool,
    ) -> Result<()> {
        self.db.transaction(|tx| {
            validate_cold_tx(tx, &self.scope)?;
            let mut keys = BTreeSet::new();
            for record in records {
                let (agent, id, family, table, column) = record.address();
                ensure!(agent == self.scope.agent_id, "cold evidence crossed scope");
                ensure!(keys.insert((family, id)), "duplicate cold evidence address");
                let exists: bool = tx.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column}=?1)"),
                    [id], |row| row.get(0),
                )?;
                ensure!(!exists, "cold fixture cannot adopt existing {table} row {id}");
            }
            if let Some(queue) = queue {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE message_id=?1)",
                    [&queue.message_id], |row| row.get(0),
                )?;
                ensure!(!exists, "cold fixture cannot adopt existing Queue");
            }
            let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
            let mut native = NativeInputWriteProof::prepare_tx(
                tx, &self.scope, &ledger, original_material_limits(&ledger)?, None,
                |visit| {
                    for (family, id) in &keys {
                        visit(*family, id)?;
                    }
                    Ok(())
                },
            )?.context("cold fixture requires Native Input writer")?;
            let current = if let Some(queue) = queue {
                native.with_input_phase(tx, &ledger, None, |material| {
                    OriginalCurrentWrites::prepare(tx, &self.scope, material, |visit| {
                        visit(CurrentFamily::Queue, &queue.message_id)
                    })
                })?
            } else {
                None
            };
            let result = records.iter().find_map(|record| match record {
                ColdFixtureEvidence::Brief(brief) => Some(*brief),
                _ => None,
            });
            let completion = work_snapshot::prepare_work_completion_write_tx(
                tx, &self.scope, result, &work_request(),
            )?;
            let mut next_seq: u64 = tx.query_row(
                "SELECT COALESCE((SELECT last_value FROM runtime_sequences WHERE domain='message' AND scope_key=?1),0)",
                [format!("agent:{}", self.scope.agent_id)], |row| row.get(0),
            )?;
            let mut facts = Vec::with_capacity(records.len());
            if bulk {
                insert_bulk_messages_tx(tx, records, &mut next_seq, &mut facts)?;
            } else {
                for record in records {
                    if let ColdFixtureEvidence::Message(message) = record {
                        validate_message_sequence(message, &mut next_seq)?;
                    }
                    facts.push(record.insert_tx(tx)?);
                }
            }
            if let Some(queue) = queue {
                repositories::upsert_queue_entry_tx(tx, queue)?;
            }
            let mut input_delta = if bulk {
                // Original insert order defines births, not a batch's key sort.
                let mut originals = facts.iter();
                let first = originals.next().context("empty cold Message batch")?;
                let mut delta = register_committed_input_evidence_tx(
                    tx, &self.scope, std::slice::from_ref(first),
                )?;
                for fact in originals {
                    let next = register_committed_input_evidence_tx(
                        tx, &self.scope, std::slice::from_ref(fact),
                    )?;
                    merge_input_evidence_delta(&mut delta, next)?;
                }
                synchronize_message_count_tx(tx, &self.scope.agent_id)?;
                delta
            } else {
                acknowledge_input_evidence_tx(tx, &self.scope, (), &facts)?.input_delta
            };
            let result_ids = result.filter(|r| r.kind == BriefKind::Result && !r.text.is_empty() && r.work_item_id.is_some())
                .map(|r| BTreeSet::from([r.id.clone()])).unwrap_or_default();
            work_snapshot::finish_work_completion_write_tx(
                tx, completion, &result_ids, &work_request(),
            )?;
            if let (Some(current), Some(queue)) = (current, queue) {
                native.with_input_phase(tx, &ledger, None, |material| {
                    current.finish_with_audit_acks(material, &[], |visit| {
                        visit(CurrentFamily::Queue, &queue.message_id)
                    })
                })?;
            }
            native.finish_delta_tx(tx, &mut input_delta, &ledger, None)?;
            Ok(())
        })
    }

    /// Final State is published with the original Work and Scheduling protocols.
    pub fn finish_state(&self, state: &AgentState) -> Result<()> {
        use super::scheduling_snapshot::{
            prepare_scheduling_write_tx, register_scheduling_writes_tx, SchedulingDomainLoadRequest,
        };
        use crate::runtime::resident_scheduling::SchedulingCaptureRequest;
        self.db.transaction(|tx| {
            validate_cold_tx(tx, &self.scope)?;
            ensure!(
                state.id == self.scope.agent_id
                    && state.status == AgentStatus::Stopped
                    && state.current_run_id.is_none(),
                "cold fixture final State must remain stopped in its scope"
            );
            let count: usize = tx.query_row(
                "SELECT COUNT(*) FROM messages WHERE agent_id=?1",
                [&state.id],
                |row| row.get(0),
            )?;
            let pending: usize = tx.query_row(
                "SELECT COUNT(*) FROM queue_entries WHERE agent_id=?1 AND status='queued'",
                [&state.id],
                |row| row.get(0),
            )?;
            ensure!(
                state.total_message_count == count && state.pending == pending,
                "cold State counts differ from canonical Message/Queue"
            );
            let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
            let request = SchedulingDomainLoadRequest {
                capture: SchedulingCaptureRequest::current(),
                record_limit: usize::try_from(ledger.limits().record_limit)?,
                byte_limit: usize::try_from(ledger.limits().history_per_agent_bytes)?,
            };
            let work = work_snapshot::prepare_work_write_tx(tx, &self.scope, &BTreeSet::new())?;
            let scheduling =
                prepare_scheduling_write_tx(tx, &self.scope, &BTreeSet::new(), &request, &ledger)?;
            evidence::upsert_agent_state_tx(tx, state)?;
            work_snapshot::register_work_writes_tx(
                tx,
                &work,
                &BTreeSet::new(),
                &BTreeSet::new(),
                state,
                &work_request(),
            )?;
            register_scheduling_writes_tx(tx, &scheduling, &BTreeSet::new(), state, &request)?;
            Ok(())
        })
    }
}

fn validate_cold_tx(tx: &Transaction<'_>, scope: &InputEvidenceScope) -> Result<()> {
    validate_input_evidence_scope_tx(tx, scope, &scope.agent_id)?;
    let state = ContextDurableRead::new(tx, &scope.agent_id)
        .scope_facts()?
        .state
        .context("cold fixture requires State")?;
    ensure!(
        state.status == AgentStatus::Stopped && state.current_run_id.is_none(),
        "cold fixture requires a stopped, drained runtime"
    );
    Ok(())
}

fn work_request() -> work_snapshot::WorkDomainLoadRequest {
    work_snapshot::WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    }
}

fn validate_message_sequence(message: &MessageEnvelope, next: &mut u64) -> Result<()> {
    *next = next
        .checked_add(1)
        .context("cold Message sequence overflow")?;
    ensure!(
        message.message_seq == Some(*next),
        "cold Message sequence is not canonical"
    );
    Ok(())
}

fn insert_bulk_messages_tx(
    tx: &Transaction<'_>,
    records: &[ColdFixtureEvidence<'_>],
    next_seq: &mut u64,
    facts: &mut Vec<Fact>,
) -> Result<()> {
    let activity_base: i64 = tx.query_row(
        "SELECT COALESCE(MAX(activity_seq),0) FROM conversation_source_revisions",
        [],
        |row| row.get(0),
    )?;
    let mut messages = tx.prepare(
        "INSERT INTO messages(evidence_id,agent_id,turn_id,message_id,message_seq,task_id,work_item_id,created_at,kind,content_ref,content_hash,preview,payload_json)
         VALUES(?1,?2,?3,?1,?4,?5,?6,?7,?8,NULL,?9,?10,?11)",
    )?;
    let mut sources = tx.prepare(
        "INSERT INTO conversation_source_revisions(source_kind,source_id,agent_id,turn_id,activity_seq,revision,updated_at)
         VALUES('operator',?1,?2,NULL,?3,1,?4)",
    )?;
    for (index, record) in records.iter().enumerate() {
        let ColdFixtureEvidence::Message(message) = record else {
            anyhow::bail!("bulk fixture only accepts Messages")
        };
        validate_message_sequence(message, next_seq)?;
        ensure!(
            super::conversation::assigned_turn_id_tx(tx, &message.agent_id, &message.id)?.is_none(),
            "bulk fixture Message already has a canonical Turn assignment"
        );
        let payload = serde_json::to_string(message)?;
        let kind = serde_json::to_value(&message.kind)?
            .as_str()
            .context("Message kind is not a string")?
            .to_owned();
        let created_at = message
            .created_at
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        messages.execute(params![
            message.id,
            message.agent_id,
            message.turn_id,
            message.message_seq,
            message.task_id,
            message.work_item_id,
            created_at,
            kind,
            evidence::content_hash(&payload),
            evidence::evidence_preview(&message.body),
            payload,
        ])?;
        sources.execute(params![
            message.id,
            message.agent_id,
            activity_base + i64::try_from(index)? + 1,
            created_at,
        ])?;
        facts.push(Fact::Message((*message).clone()));
    }
    if let Some(ColdFixtureEvidence::Message(message)) = records.last() {
        evidence::observe_runtime_sequence_tx(
            tx,
            evidence::MESSAGE_SEQUENCE_DOMAIN,
            &format!("agent:{}", message.agent_id),
            *next_seq,
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "cold_fixture_tests.rs"]
mod tests;
