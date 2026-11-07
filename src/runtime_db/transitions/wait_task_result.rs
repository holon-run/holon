use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction};

use super::{RuntimeStateTransitionConflict, WaitTaskResultAdmission};
use crate::{
    runtime::resident_input_evidence::InputEvidenceScope,
    runtime_db::{
        task_result_settlement::{
            classify_obligation_with_waits_tx, dropped_reply_source_tx, DroppedReplySource,
            TaskResultObligation, TaskResultSettlementState,
        },
        work_snapshot::canonical_wait_record_tx,
        TaskResultSettlementRecord,
    },
    types::{QueueEntryRecord, WaitConditionRecord, WaitConditionStatus},
};

const MAX_LIVE_WAITS: usize = 65_536;
const MAX_EXACT_WAITS: usize = 64;
const MAX_WITNESS_BYTES: usize = 64 * 1024 * 1024;

// The same current/seed membership as the Work loader, without historical bodies.
const QUALIFIED_WAIT: &str = "
    (EXISTS(SELECT 1 FROM work_domain_records a
       WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family='wait'
         AND a.record_id=c.wait_condition_id AND a.availability='present')
     OR (NOT EXISTS(SELECT 1 FROM work_domain_records a
       WHERE a.agent_id=?1 AND a.identity_incarnation=?2 AND a.family='wait'
         AND a.record_id=c.wait_condition_id)
       AND EXISTS(SELECT 1 FROM work_domain_eligibility e
         JOIN agent_episode_continuity b ON b.seed_id=e.seed_id
         WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.policy_version=1
           AND e.family='wait' AND e.record_id=c.wait_condition_id)))";

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WaitTaskResultWitness {
    pub scope: InputEvidenceScope,
    pub work_revision: u64,
    pub live_waits: Vec<WaitConditionRecord>,
    /// Only the predecessor/source addresses used by this obligation.
    pub exact_waits: Vec<WaitConditionRecord>,
}

impl WaitTaskResultWitness {
    pub(super) fn validate_tx<'a>(
        &'a self,
        tx: &Transaction<'_>,
        agent_id: &str,
    ) -> Result<Vec<&'a WaitConditionRecord>> {
        let conflict = || {
            RuntimeStateTransitionConflict::concurrent_mutation(
                "task_result_wait_witness",
                agent_id,
            )
        };
        if self.scope.agent_id != agent_id
            || self.scope.identity_incarnation == 0
            || self.work_revision == 0
            || self.live_waits.len() > MAX_LIVE_WAITS
            || self.exact_waits.len() > MAX_EXACT_WAITS
            || crate::runtime::input_evidence_budget::serialized_bytes(self)?
                > u64::try_from(MAX_WITNESS_BYTES)?
        {
            return Err(conflict().into());
        }
        // Only scalar normalized metadata is read, not the full Work coverage.
        let revision: Option<u64> = tx
            .query_row(
                "SELECT w.revision FROM work_domain_current w
                 JOIN agent_identities i ON i.agent_id=w.agent_id
                 WHERE w.agent_id=?1 AND w.identity_incarnation=?2
                   AND i.status='active'
                   AND json_extract(i.payload_json,'$.agent_id')=w.agent_id
                   AND json_extract(i.payload_json,'$.status')='active'
                   AND COALESCE(json_extract(i.payload_json,'$.incarnation'),1)=?2",
                params![agent_id, self.scope.identity_incarnation],
                |row| row.get(0),
            )
            .optional()?;
        if revision != Some(self.work_revision) {
            return Err(conflict().into());
        }
        let live_count: usize = tx.query_row(
            &format!(
                "SELECT COUNT(*) FROM wait_conditions c WHERE c.agent_id=?1
                 AND c.status IN ('active','triggered') AND {QUALIFIED_WAIT}"
            ),
            params![agent_id, self.scope.identity_incarnation],
            |row| row.get(0),
        )?;
        if live_count != self.live_waits.len() {
            return Err(conflict().into());
        }
        let mut ids = BTreeSet::new();
        let mut material_bytes = 0usize;
        let mut waits = Vec::with_capacity(self.live_waits.len() + self.exact_waits.len());
        for (live, records) in [(true, &self.live_waits), (false, &self.exact_waits)] {
            for wait in records {
                if wait.agent_id != agent_id
                    || wait.id.is_empty()
                    || !ids.insert(wait.id.as_str())
                    || matches!(
                        wait.status,
                        WaitConditionStatus::Active | WaitConditionStatus::Triggered
                    ) != live
                {
                    return Err(conflict().into());
                }
                let bytes: Option<usize> = tx
                    .query_row(
                        &format!(
                            "SELECT length(CAST(c.payload_json AS BLOB))
                             FROM wait_conditions c
                             WHERE c.agent_id=?1 AND c.wait_condition_id=?3
                               AND {QUALIFIED_WAIT}"
                        ),
                        params![agent_id, self.scope.identity_incarnation, wait.id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(bytes) = bytes else {
                    return Err(conflict().into());
                };
                material_bytes = material_bytes
                    .checked_add(bytes)
                    .filter(|bytes| *bytes <= MAX_WITNESS_BYTES)
                    .ok_or_else(conflict)?;
                if canonical_wait_record_tx(tx, agent_id, &wait.id)?.as_ref() != Some(wait) {
                    return Err(conflict().into());
                }
                waits.push(wait);
            }
        }
        Ok(waits)
    }
}

// A resolved wait outside the live frontier can still reserve this exact result.
// Seed-qualified history may not have materialized Task edges yet. Check the
// normalized Wait columns with the same membership as the live frontier.
pub(super) fn has_resolved_claim_tx(
    tx: &Transaction<'_>,
    guard: &WaitTaskResultObligation,
) -> Result<bool> {
    tx.query_row(
        &format!("SELECT EXISTS(
           SELECT 1 FROM wait_conditions c
           WHERE c.agent_id=?1 AND {QUALIFIED_WAIT}
             AND c.kind='task' AND c.status='resolved'
             AND EXISTS(SELECT 1 FROM json_each(c.wake_sources_json) wake
               WHERE json_extract(wake.value,'$.kind')='task_result'
                 AND json_extract(wake.value,'$.task_id')=?3)
             AND c.work_item_id IS NOT ?4
             AND (
               c.work_item_id IS NULL
               OR EXISTS(SELECT 1 FROM work_items w
                 WHERE w.agent_id=?1 AND w.work_item_id=c.work_item_id AND w.state='open')
               OR (?5 AND c.source='WaitFor' AND c.subject_ref=?3
                   AND json_array_length(c.wake_sources_json)=1))
             AND (
               c.trigger_message_id=?6
               OR (c.source='WaitFor' AND c.subject_ref=?3
                   AND json_array_length(c.wake_sources_json)=1
                   AND EXISTS(SELECT 1 FROM messages m
                     WHERE m.message_id=c.trigger_message_id AND m.agent_id=?1
                       AND m.work_item_id IS c.work_item_id AND m.kind='internal_followup'
                       AND json_extract(m.payload_json,'$.agent_id')=?1
                       AND json_extract(m.payload_json,'$.work_item_id') IS c.work_item_id
                       AND json_extract(m.payload_json,'$.kind')='internal_followup'
                       AND json_extract(m.payload_json,'$.origin.kind')='system'
                       AND json_extract(m.payload_json,'$.origin.subsystem')='task_result_recovery'
                       AND json_extract(m.payload_json,'$.authority_class')='runtime_instruction'
                       AND json_extract(m.payload_json,'$.delivery_surface')='runtime_system'
                       AND json_extract(m.payload_json,'$.admission_context')='runtime_owned'
                       AND json_extract(m.payload_json,'$.source_refs.task_result_message_id')=?6
                       AND json_extract(m.payload_json,'$.source_refs.task_result_identity')=?7
                       AND json_extract(m.payload_json,?10)=?8
                       AND json_extract(m.payload_json,?11) IS NULL
                       AND json_extract(m.payload_json,'$.source_refs.task_result_rejoin_generation')=?9
                       AND (json_extract(m.payload_json,'$.source_refs.wait_id') IS NULL
                            OR json_extract(m.payload_json,'$.source_refs.wait_id')=c.wait_condition_id))))
         )"),
        params![
            guard.expected.agent_id,
            guard.wait_witness.scope.identity_incarnation,
            guard.expected.task_id,
            guard.waiter_work_item_id,
            guard.expected.state == TaskResultSettlementState::CallerAdmitted,
            guard.expected.message_id,
            guard.expected.result_identity,
            guard.expected.source.result_source_ref().1,
            guard.expected.rejoin_generation.to_string(),
            format!("$.source_refs.{}", guard.expected.source.result_source_ref().0),
            if guard.expected.source.parent_turn_id().is_some() {
                "$.source_refs.task_result_command_creation_id"
            } else {
                "$.source_refs.task_result_parent_turn_id"
            },
        ],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WaitTaskResultObligation {
    pub expected: TaskResultSettlementRecord,
    pub current_activation_id: Option<String>,
    pub waiter_work_item_id: Option<String>,
    pub canonical_recovery: bool,
    pub source_message_id: String,
    pub source_queue: Option<QueueEntryRecord>,
    pub dropped_reply_source: Option<DroppedReplySource>,
    pub wait_witness: WaitTaskResultWitness,
}

pub(super) fn validate_obligation_tx(
    tx: &Transaction<'_>,
    admission: &WaitTaskResultAdmission,
) -> Result<()> {
    let guard = &admission.obligation;
    let conflict = || {
        RuntimeStateTransitionConflict::concurrent_mutation(
            "task_result_obligation",
            &guard.expected.result_identity,
        )
    };
    if admission.message.agent_id != guard.expected.agent_id
        || admission.record.agent_id != guard.expected.agent_id
    {
        return Err(conflict().into());
    }
    let waits = guard
        .wait_witness
        .validate_tx(tx, &guard.expected.agent_id)?;
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM task_result_settlements WHERE result_identity = ?1",
            [&guard.expected.result_identity],
            |row| row.get(0),
        )
        .optional()?;
    if payload
        .map(|payload| serde_json::from_str::<TaskResultSettlementRecord>(&payload))
        .transpose()?
        .as_ref()
        != Some(&guard.expected)
        || has_resolved_claim_tx(tx, guard)?
        || classify_obligation_with_waits_tx(
            tx,
            &guard.expected,
            guard.current_activation_id.as_deref(),
            guard.waiter_work_item_id.as_deref(),
            &waits,
        )? != TaskResultObligation::Recoverable
    {
        return Err(conflict().into());
    }
    let source_queue: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
            [&guard.source_message_id],
            |row| row.get(0),
        )
        .optional()?;
    if source_queue
        .map(|payload| serde_json::from_str::<QueueEntryRecord>(&payload))
        .transpose()?
        != guard.source_queue
    {
        return Err(conflict().into());
    }
    if !guard.canonical_recovery {
        return Ok(());
    }
    if let Some(expected) = &guard.dropped_reply_source {
        if dropped_reply_source_tx(tx, &guard.expected)?.as_ref() != Some(expected) {
            return Err(conflict().into());
        }
    }
    // Closing the captured owner need not mutate the waiter or agent CAS.
    // Recheck both owners in the transaction that publishes the repaired wait.
    for owner in [
        guard.expected.work_item_id.as_deref(),
        guard.waiter_work_item_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_items
             WHERE agent_id = ?1 AND work_item_id = ?2 AND state = 'open')",
            params![guard.expected.agent_id, owner],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(conflict().into());
        }
    }
    // Enqueue and WaitFor repair share the canonical owner's wake identity.
    // A concurrent scanner wake must force re-preparation, not duplicate it.
    let duplicate: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages m
         JOIN queue_entries q ON q.message_id=m.message_id
         WHERE m.agent_id=?1 AND m.work_item_id IS ?2 AND m.message_id!=?3
           AND q.status NOT IN ('processed','aborted','dropped','quarantined')
           AND json_extract(m.payload_json,'$.origin.kind')='system'
           AND json_extract(m.payload_json,'$.origin.subsystem')='task_result_recovery'
           AND json_extract(m.payload_json,'$.source_refs.task_result_owner_key') IS ?4)",
        params![
            guard.expected.agent_id,
            admission.message.work_item_id,
            admission.message.id,
            admission.message.source_refs.get("task_result_owner_key"),
        ],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(conflict().into());
    }
    Ok(())
}
