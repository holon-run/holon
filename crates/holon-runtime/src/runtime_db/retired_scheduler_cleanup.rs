use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction};
use serde::Serialize;

use crate::domain::execution_protocol::{
    self, CommandResult, ConversationOutcome, ExecutionAttemptState, ExecutionOutcome,
    ExecutionOutcomeRecord, WorkItemExecutionState, WorkItemOutcome,
};
use crate::ids;
use crate::runtime_db::evidence::{append_audit_event_tx, append_message_tx};
use crate::runtime_db::repositories::upsert_queue_entry_tx;
use crate::runtime_db::transitions::execution_protocol_repository::{
    load_retired_scheduler_state_tx, persist_repaired_state_tx,
};
use crate::types::{
    AuditEvent, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    QueueEntryRecord, QueueEntryStatus,
};

pub(crate) use holon_storage::retired_scheduler_cleanup::retired_scheduler_cleanup_inventory;
pub use holon_storage::retired_scheduler_cleanup::{
    RetiredSchedulerCleanupBlocker, RetiredSchedulerCleanupBlockerKind,
    RetiredSchedulerCleanupInventory,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetiredSchedulerFallbackActionKind {
    InterruptOpenExecutionAttempt,
    ResumeInFlightExecutionWorkItem,
    QuarantineDequeuedQueueEntry,
    EnqueueAgentRecoveryEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetiredSchedulerFallbackAction {
    pub kind: RetiredSchedulerFallbackActionKind,
    pub record_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetiredSchedulerFallbackResult {
    pub agent_id: String,
    pub recovery_message_id: Option<String>,
    pub protected_message_ids: Vec<String>,
    pub actions: Vec<RetiredSchedulerFallbackAction>,
}

/// Reason the retired-scheduler cleanup records on the terminal outcome of an
/// attempt it interrupts.
///
/// Startup ended-memory recovery may seal an unfinished dispatch only when a
/// canonical recovery writer already ended it
/// (`startup_ended_memory::interrupted_outcome_matches`), and the vocabulary of
/// that proof is the `scheduler-recovery --apply` plan, which interrupts the
/// same unsettled activation with `runtime_interrupted`. The cleanup repairs the
/// retired schema so that very plan can run, so an activation it settles has to
/// carry the same canonical reason: a cleanup-private reason left the upgraded
/// daemon unable to seal the dispatch, and `holon serve` aborted before
/// readiness instead of booting (#4500). The repair stays attributable through
/// `RetiredSchedulerFallbackAction::reason`, the
/// `retired_scheduler_cleanup_fallback_applied` audit event, and the
/// `scheduler_execution_recovered` audit reason written below.
pub(super) const RETIRED_SCHEDULER_INTERRUPTED_REASON: &str = "runtime_interrupted";

pub(crate) fn apply_retired_scheduler_cleanup_fallback(
    tx: &Transaction<'_>,
    db: &super::RuntimeDb,
    agent_id: &str,
    maintenance: Option<&super::OfflineInputMaintenance<'_>>,
) -> Result<RetiredSchedulerFallbackResult> {
    let inventory = retired_scheduler_cleanup_inventory(tx)?;
    let blockers = inventory
        .blockers
        .into_iter()
        .filter(|blocker| blocker.agent_id == agent_id)
        .collect::<Vec<_>>();
    if blockers.is_empty() {
        return Ok(RetiredSchedulerFallbackResult {
            agent_id: agent_id.to_string(),
            recovery_message_id: None,
            protected_message_ids: Vec::new(),
            actions: Vec::new(),
        });
    }

    let mut state = load_retired_scheduler_state_tx(tx, agent_id)
        .with_context(|| format!("loading damaged execution partition for agent {agent_id}"))?;
    let previous = state.clone();
    let now = Utc::now();
    let now_text = now.to_rfc3339();
    let recovery_message_id = ids::message_id();
    let mut protected_message_ids = BTreeSet::new();
    let mut actions = Vec::new();

    for attempt in state
        .attempts
        .values()
        .filter(|attempt| attempt.state == ExecutionAttemptState::Open)
    {
        if let Some(message_id) = attempt.source_message_id.as_deref() {
            require_message_evidence(tx, agent_id, message_id)?;
            protected_message_ids.insert(message_id.to_string());
        }
    }
    for blocker in blockers
        .iter()
        .filter(|blocker| blocker.kind == RetiredSchedulerCleanupBlockerKind::DequeuedQueueEntry)
    {
        require_message_evidence(tx, agent_id, &blocker.record_id)?;
        protected_message_ids.insert(blocker.record_id.clone());
    }

    let input = super::input_evidence_snapshot::maintenance::fallback::prepare_tx(
        tx,
        db,
        agent_id,
        &state,
        &blockers,
        &recovery_message_id,
        maintenance,
    )?;

    // This inventory comes from the retired schema, not the runtime state model.
    for blocker in blockers.iter().filter(|blocker| {
        blocker.kind == RetiredSchedulerCleanupBlockerKind::InFlightExecutionWorkItem
    }) {
        let work_item = state
            .work_items
            .get_mut(&blocker.record_id)
            .context("missing legacy WorkItem execution row during offline release")?;
        let generation = work_item
            .generation()
            .checked_add(1)
            .ok_or_else(|| anyhow!("WorkItem recovery generation overflow"))?;
        work_item.state = WorkItemExecutionState::Runnable {
            generation,
            recovery_ref: Some(recovery_message_id.clone()),
        };
        actions.push(RetiredSchedulerFallbackAction {
            kind: RetiredSchedulerFallbackActionKind::ResumeInFlightExecutionWorkItem,
            record_id: blocker.record_id.clone(),
            reason: "retired_scheduler_cleanup_released_in_flight_work_item".into(),
        });
    }

    let open_attempt_ids = state
        .attempts
        .values()
        .filter(|attempt| attempt.state == ExecutionAttemptState::Open)
        .map(|attempt| attempt.attempt_id.clone())
        .collect::<Vec<_>>();
    let mut interrupted_attempts = Vec::with_capacity(open_attempt_ids.len());
    for attempt_id in open_attempt_ids {
        let attempt = state
            .attempts
            .get(&attempt_id)
            .cloned()
            .ok_or_else(|| anyhow!("open execution attempt disappeared from recovery state"))?;
        let recovery_proof = (
            attempt_id.clone(),
            attempt.source_message_id.clone(),
            attempt.binding.work_item_id().map(str::to_owned),
        );
        let outcome_id = format!("retired-scheduler-cleanup:{attempt_id}");
        let outcome = fallback_outcome(&attempt, &now_text);
        let attempt = state
            .attempts
            .get_mut(&attempt_id)
            .expect("attempt was read from the same state");
        attempt.state = ExecutionAttemptState::Interrupted;
        attempt.terminal_outcome_id = Some(outcome_id.clone());
        attempt.terminal_at = Some(now_text.clone());
        state.outcomes.insert(outcome_id, outcome);
        interrupted_attempts.push(recovery_proof);
        actions.push(RetiredSchedulerFallbackAction {
            kind: RetiredSchedulerFallbackActionKind::InterruptOpenExecutionAttempt,
            record_id: attempt_id,
            reason: "retired_scheduler_cleanup_interrupted_unrecoverable_attempt".into(),
        });
    }
    execution_protocol::assert_invariants(&state).map_err(|error| {
        anyhow!("retired scheduler fallback could not repair execution partition: {error}")
    })?;
    persist_repaired_state_tx(tx, &previous, &state)?;

    let mut dequeued = tx.prepare(
        "SELECT payload_json
         FROM queue_entries
         WHERE agent_id = ?1 AND status = 'dequeued'
         ORDER BY message_id",
    )?;
    let dequeued = dequeued
        .query_map([agent_id], |row| row.get::<_, String>(0))?
        .map(|payload| {
            serde_json::from_str::<QueueEntryRecord>(&payload?)
                .context("decoding dequeued queue entry for retired scheduler fallback")
        })
        .collect::<Result<Vec<_>>>()?;
    for mut entry in dequeued {
        entry.status = QueueEntryStatus::Quarantined;
        entry.updated_at = now;
        upsert_queue_entry_tx(tx, &entry)?;
        actions.push(RetiredSchedulerFallbackAction {
            kind: RetiredSchedulerFallbackActionKind::QuarantineDequeuedQueueEntry,
            record_id: entry.message_id,
            reason: "retired_scheduler_cleanup_side_effect_unknown".into(),
        });
    }

    let protected_message_ids = protected_message_ids.into_iter().collect::<Vec<_>>();
    actions.push(RetiredSchedulerFallbackAction {
        kind: RetiredSchedulerFallbackActionKind::EnqueueAgentRecoveryEvent,
        record_id: recovery_message_id.clone(),
        reason: "retired_scheduler_cleanup_requires_agent_reconciliation".into(),
    });
    let mut recovery_message = MessageEnvelope::new(
        agent_id,
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "scheduler_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Json {
            value: serde_json::json!({
                "kind": "retired_scheduler_cleanup_recovery",
                "recovery_message_id": recovery_message_id,
                "protected_message_ids": protected_message_ids,
                "actions": actions,
                "instruction": "Inspect the protected original messages and resume or reconcile interrupted work.",
            }),
        },
    );
    recovery_message.id = recovery_message_id.clone();
    for message_id in &protected_message_ids {
        recovery_message
            .source_refs
            .insert(message_id.clone(), format!("message:{message_id}"));
    }
    let (recovery_message, inserted) = append_message_tx(tx, &recovery_message)?;
    upsert_queue_entry_tx(
        tx,
        &QueueEntryRecord {
            message_id: recovery_message.id.clone(),
            agent_id: agent_id.to_string(),
            priority: Priority::Next,
            status: QueueEntryStatus::Queued,
            created_at: recovery_message.created_at,
            updated_at: recovery_message.created_at,
        },
    )?;
    // The canonical `scheduler-recovery --apply` plan proves every activation it
    // interrupts with one `scheduler_execution_recovered` event, and startup
    // requires that proof before it seals the dispatch's unfinished Start
    // receipt (#4399). The cleanup interrupts the same activations, so it has to
    // leave the same audit; without it the upgraded daemon cannot prove a
    // canonical end for the dispatch and refuses to boot (#4500). `created_at`
    // is pinned to the attempt's terminal time so the event describes exactly
    // the settlement it proves.
    for (attempt_id, message_id, work_item_id) in &interrupted_attempts {
        let mut event = AuditEvent::legacy(
            "scheduler_execution_recovered",
            serde_json::json!({
                "agent_id": agent_id,
                "message_id": message_id,
                "activation_id": attempt_id,
                "work_item_id": work_item_id,
                "reason": "retired_scheduler_cleanup",
            }),
        );
        event.created_at = now;
        append_audit_event_tx(tx, Some(agent_id), &event)?;
    }
    append_audit_event_tx(
        tx,
        Some(agent_id),
        &AuditEvent::legacy(
            "retired_scheduler_cleanup_fallback_applied",
            serde_json::json!({
                "agent_id": agent_id,
                "recovery_message_id": recovery_message.id,
                "protected_message_ids": protected_message_ids,
                "actions": actions,
            }),
        ),
    )?;

    input.finish_tx(tx, &recovery_message, inserted)?;

    Ok(RetiredSchedulerFallbackResult {
        agent_id: agent_id.to_string(),
        recovery_message_id: Some(recovery_message.id),
        protected_message_ids,
        actions,
    })
}

pub(super) fn fallback_outcome(
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
    now: &str,
) -> ExecutionOutcomeRecord {
    ExecutionOutcomeRecord {
        outcome_id: format!("retired-scheduler-cleanup:{}", attempt.attempt_id),
        attempt_id: attempt.attempt_id.clone(),
        outcome: if attempt.binding.is_command() {
            // Command bindings keep their quarantine: the cleanup cannot prove
            // the command had no external effect, and startup has no canonical
            // interrupted shape for a quarantined command. Proven repairs only
            // interrupt turn-shaped activations, so that combination is not a
            // reachable upgrade path today; widening recovery for it needs a
            // separate decision.
            ExecutionOutcome::Command(CommandResult::Quarantined {
                reason: "retired_scheduler_cleanup".into(),
            })
        } else if attempt.binding.work_item_id().is_some() {
            ExecutionOutcome::WorkItem(WorkItemOutcome::Interrupted {
                reason: RETIRED_SCHEDULER_INTERRUPTED_REASON.into(),
            })
        } else {
            ExecutionOutcome::Conversation(ConversationOutcome::Interrupted {
                reason: RETIRED_SCHEDULER_INTERRUPTED_REASON.into(),
            })
        },
        created_at: now.into(),
    }
}

fn require_message_evidence(tx: &Transaction<'_>, agent_id: &str, message_id: &str) -> Result<()> {
    let payload = tx
        .query_row(
            "SELECT payload_json FROM messages WHERE evidence_id = ?1",
            [message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!(
                "retired scheduler fallback cannot preserve source message {message_id}; \
                 refusing automatic recovery"
            )
        })?;
    let message: MessageEnvelope = serde_json::from_str(&payload)
        .with_context(|| format!("decoding protected source message {message_id}"))?;
    if message.id != message_id || message.agent_id != agent_id {
        bail!("retired scheduler fallback source message identity mismatch for {message_id}");
    }
    Ok(())
}
