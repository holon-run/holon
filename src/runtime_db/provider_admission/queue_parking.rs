//! Durable queue parking conditions, independent of queue and wait lifecycles.

use super::*;

#[path = "stopped_pause_memory.rs"]
mod stopped_pause_memory;
pub(crate) use stopped_pause_memory::{
    load_stopped_provider_pause_memory_tx, StoppedProviderPauseMemory,
};

/// Sealed borrowed view of the actual typed ACK, never a status-based address.
pub(crate) struct StoppedProviderPauseAck<'a> {
    queue: &'a crate::runtime_db::transitions::PreparedQueueTransition,
    frozen: &'a FrozenStoppedProviderPause,
}

impl<'a> StoppedProviderPauseAck<'a> {
    pub(in crate::runtime_db) fn from_queue(
        queue: &'a crate::runtime_db::transitions::PreparedQueueTransition,
    ) -> Option<Self> {
        let frozen = queue.stopped_provider_pause.as_ref()?;
        frozen
            .is_original_ack_shape(queue)
            .then_some(Self { queue, frozen })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProviderQueueParking {
    pub agent_id: String,
    pub message_id: String,
    pub chain_fingerprint: String,
    pub episode_key: String,
    pub not_before: DateTime<Utc>,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ProviderDispatchPause {
    pub parking: ProviderQueueParking,
    pub attempt_id: String,
    pub denial: &'static str,
}

impl ProviderQueueParking {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.agent_id.is_empty()
                && !self.message_id.is_empty()
                && !self.chain_fingerprint.is_empty()
                && !self.episode_key.is_empty()
                && matches!(
                    self.reason.as_str(),
                    "chain_cooling" | "recovery_budget_exhausted"
                ),
            "invalid provider queue parking condition"
        );
        Ok(())
    }
}

pub(crate) fn read_parking(
    connection: &Connection,
    agent_id: &str,
    message_id: &str,
) -> Result<Option<ProviderQueueParking>> {
    let raw = connection
        .query_row(
            "SELECT chain_fingerprint, episode_key, not_before, reason
             FROM provider_queue_parking WHERE agent_id = ?1 AND message_id = ?2",
            params![agent_id, message_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    raw.map(|(chain_fingerprint, episode_key, not_before, reason)| {
        let record = ProviderQueueParking {
            agent_id: agent_id.to_owned(),
            message_id: message_id.to_owned(),
            chain_fingerprint,
            episode_key,
            not_before: not_before
                .parse()
                .context("invalid provider queue parking deadline")?,
            reason,
        };
        record.validate()?;
        Ok(record)
    })
    .transpose()
}

/// Observe these sizes in the caller's shared budget before decoding the proof.
pub(crate) fn stopped_provider_pause_material_tx(
    tx: &Transaction<'_>,
    original: &crate::runtime_db::control_transition::ControlStoppedRun<'_>,
) -> Result<(usize, usize)> {
    let (bytes, records) = crate::runtime_db::control_transition::stopped_run_proof_material_tx(
        tx,
        &crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: original.agent_id.to_owned(),
            identity_incarnation: original.identity_incarnation,
        },
        original.run_id,
        original.activation_id,
    )?;
    let (parking_bytes, parking_records): (usize, usize) = tx.query_row(
        "SELECT COALESCE(SUM(length(CAST(agent_id AS BLOB))
             + length(CAST(message_id AS BLOB)) + length(CAST(chain_fingerprint AS BLOB))
             + length(CAST(episode_key AS BLOB)) + length(CAST(not_before AS BLOB))
             + length(CAST(reason AS BLOB))), 0), COUNT(*)
         FROM provider_queue_parking WHERE agent_id = ?1 AND message_id = ?2",
        params![original.agent_id, original.source.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((
        bytes
            .checked_add(parking_bytes)
            .context("stopped provider pause proof byte overflow")?,
        records
            .checked_add(parking_records)
            .context("stopped provider pause proof count overflow")?,
    ))
}

/// A Stop may win after dispatch is parked. Only acknowledge that original
/// pause; its Queue, Turn, attempt and AgentState remain the Stop's values.
pub(crate) fn acknowledge_stopped_provider_pause_tx(
    tx: &Transaction<'_>,
    original: &crate::runtime_db::control_transition::ControlStoppedRun<'_>,
    pause: &ProviderDispatchPause,
) -> Result<bool> {
    pause.parking.validate()?;
    ensure!(
        original.activation_id == Some(pause.attempt_id.as_str())
            && original.agent_id == pause.parking.agent_id
            && original.source.id == pause.parking.message_id
            && pause.parking.reason == "chain_cooling",
        "stopped provider pause differs from the original dispatch"
    );
    let Some(proof) =
        crate::runtime_db::control_transition::find_committed_control_stop_for_run_tx(
            tx, original,
        )?
    else {
        return Ok(false);
    };
    proof.require_same_transaction(tx)?;
    ensure!(
        read_parking(tx, original.agent_id, &original.source.id)?.as_ref() == Some(&pause.parking),
        "stopped provider pause parking changed before acknowledgement"
    );
    Ok(true)
}

/// Frozen positive address of the original dispatch Stop that won while this
/// pause stayed parked. The prepared Queue ACK owns it exactly once; no field
/// is re-derived from the current database at commit time.
#[derive(Debug, serde::Serialize)]
pub(crate) struct FrozenStoppedProviderPause {
    agent_id: String,
    identity_incarnation: u64,
    owner_epoch: u64,
    run_id: String,
    runtime_lease_id: String,
    activation_id: Option<String>,
    source: crate::types::MessageEnvelope,
    pause: ProviderDispatchPause,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory: Option<StoppedProviderPauseMemory>,
}

impl FrozenStoppedProviderPause {
    /// Freezing rejects a pause that is not the original dispatch's own
    /// parking. It grants no new provenance, Turn or absence exemption.
    pub(crate) fn freeze(
        original: &crate::runtime_db::control_transition::ControlStoppedRun<'_>,
        pause: &ProviderDispatchPause,
    ) -> Result<Self> {
        ensure!(
            original.agent_id == pause.parking.agent_id
                && original.source.id == pause.parking.message_id
                && original.activation_id == Some(pause.attempt_id.as_str())
                && pause.parking.reason == "chain_cooling",
            "stopped provider pause differs from the original dispatch"
        );
        Ok(Self {
            agent_id: original.agent_id.to_owned(),
            identity_incarnation: original.identity_incarnation,
            owner_epoch: original.owner_epoch,
            run_id: original.run_id.to_owned(),
            runtime_lease_id: original.runtime_lease_id.to_owned(),
            activation_id: original.activation_id.map(str::to_owned),
            source: original.source.clone(),
            pause: pause.clone(),
            memory: None,
        })
    }

    fn stopped_run(&self) -> crate::runtime_db::control_transition::ControlStoppedRun<'_> {
        crate::runtime_db::control_transition::ControlStoppedRun {
            agent_id: &self.agent_id,
            identity_incarnation: self.identity_incarnation,
            owner_epoch: self.owner_epoch,
            run_id: &self.run_id,
            runtime_lease_id: &self.runtime_lease_id,
            activation_id: self.activation_id.as_deref(),
            source: &self.source,
        }
    }

    pub(crate) fn with_memory(&mut self, memory: StoppedProviderPauseMemory) -> Result<()> {
        ensure!(
            self.memory.is_none(),
            "stopped ACK already froze its memory"
        );
        memory.require_original(self)?;
        self.memory = Some(memory);
        Ok(())
    }

    /// Classify only the typed Brief/Audit ACK, not arbitrary Queue settlement.
    /// The final transaction must still verify its full Queue and Stop proof.
    pub(crate) fn is_original_ack_shape(
        &self,
        queue: &crate::runtime_db::transitions::PreparedQueueTransition,
    ) -> bool {
        self.original_ack_record(queue).is_ok()
    }

    fn original_ack_record<'a>(
        &self,
        queue: &'a crate::runtime_db::transitions::PreparedQueueTransition,
    ) -> Result<&'a crate::types::QueueEntryRecord> {
        use crate::runtime_db::transitions::{QueueMutation, QueueOperation};
        let command = &queue.command;
        let record = match &command.mutation {
            QueueMutation::Upsert(record) => record,
            _ => anyhow::bail!(
                "stopped provider pause ACK must repeat its Stop-owned queue terminal"
            ),
        };
        ensure!(
            command.agent_id == self.agent_id
                && command.operation == QueueOperation::Settle
                && record.status == crate::types::QueueEntryStatus::Aborted
                && record.agent_id == self.agent_id
                && record.message_id == self.source.id,
            "stopped provider pause ACK differs from its Stop-owned queue terminal"
        );
        ensure!(
            command.agent_state.is_none()
                && command.turn_record.is_none()
                && command.scheduler_claim_work_item.is_none()
                && command.message_evidence.is_empty()
                && command.transcript_entries.is_empty()
                && command.brief_evidence.len() <= 1,
            "stopped provider pause ACK carries Stop-owned or unrelated writes"
        );
        ensure!(
            queue.supervised_invocation.is_none()
                && queue.wait_transition.is_none()
                && queue.task_expectation.is_none()
                && queue.completion.is_none()
                && queue.wait_registration.is_none()
                && queue.terminal_tool_executions.is_empty()
                && queue.extra_wait_conditions.is_empty()
                && queue.delivery.is_none()
                && queue.timer_wake_claim.is_none()
                && queue.task_result_settlement.is_none()
                && queue.execution_protocol.bootstrap.is_none()
                && queue.execution_protocol.commands.is_empty(),
            "stopped provider pause ACK carries Stop-owned or unrelated writes"
        );
        Ok(record)
    }

    /// The frozen ACK may carry only its original Brief/Audit evidence. Every
    /// Stop-owned Queue terminal, Turn, attempt and AgentState value must stay
    /// exactly as the Stop committed it; this carrier grants no exemption.
    pub(crate) fn require_original_ack_shape_tx(
        &self,
        tx: &Transaction<'_>,
        queue: &crate::runtime_db::transitions::PreparedQueueTransition,
    ) -> Result<()> {
        let record = self.original_ack_record(queue)?;
        let payload: String = tx.query_row(
            "SELECT payload_json FROM queue_entries WHERE message_id = ?1",
            [&record.message_id],
            |row| row.get(0),
        )?;
        let committed: crate::types::QueueEntryRecord = serde_json::from_str(&payload)?;
        ensure!(
            committed == *record,
            "queue terminal changed before the stopped provider pause ACK"
        );
        Ok(())
    }
}

/// Observe the frozen proof material in the caller's same ledger and dispatch
/// budget, and retain the returned guard until the queue transaction ends.
pub(crate) fn charge_frozen_stopped_provider_pause_tx(
    tx: &Transaction<'_>,
    frozen: &FrozenStoppedProviderPause,
    scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
    request: &crate::runtime_db::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    memory_budget: Option<&mut crate::runtime_db::dispatch_memory::DispatchMemoryTransactionBudget>,
) -> Result<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
    frozen
        .memory
        .as_ref()
        .context("stopped ACK omitted its memory observation")?
        .require_budget(scope, ledger)?;
    let (bytes, records) = stopped_provider_pause_material_tx(tx, &frozen.stopped_run())?;
    let (memory_bytes, memory_records) = stopped_pause_memory::frozen_material_tx(tx, frozen)?;
    let bytes = bytes
        .checked_add(memory_bytes)
        .context("stopped ACK material overflow")?;
    let records = records
        .checked_add(memory_records)
        .context("stopped ACK records overflow")?;
    if let Some(memory) = memory_budget {
        memory.additional_material(bytes, records, 1)?;
    }
    crate::runtime_db::command_task_terminal::charge_material(
        scope, bytes, records, request, ledger,
    )
}

/// Re-verify the Stop and its unchanged parking inside the caller's final
/// queue transaction. Only success allows the frozen ACK to commit.
pub(crate) fn acknowledge_frozen_stopped_provider_pause_tx(
    tx: &Transaction<'_>,
    frozen: &FrozenStoppedProviderPause,
) -> Result<()> {
    let proof = crate::runtime_db::control_transition::find_committed_control_stop_for_run_tx(
        tx,
        &frozen.stopped_run(),
    )?
    .context("stopped provider pause proof is missing at the final queue transaction")?;
    proof.require_same_transaction(tx)?;
    ensure!(
        read_parking(tx, &frozen.agent_id, &frozen.source.id)?.as_ref()
            == Some(&frozen.pause.parking),
        "stopped provider pause parking changed before acknowledgement"
    );
    stopped_pause_memory::revalidate_tx(tx, frozen, &proof)
}

#[cfg(test)]
#[path = "stopped_pause_tests.rs"]
mod stopped_pause_tests;

impl RuntimeDb {
    /// Replaying an unchanged condition does not write or move its deadline.
    /// The caller supplies the canonical gate's deadline, not a new wake-relative floor.
    pub fn park_provider_queue(&self, record: &ProviderQueueParking) -> Result<bool> {
        record.validate()?;
        self.transaction(|tx| {
            if read_parking(tx, &record.agent_id, &record.message_id)?.as_ref() == Some(record) {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO provider_queue_parking (
                    agent_id, message_id, chain_fingerprint, episode_key, not_before, reason
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(agent_id, message_id) DO UPDATE SET
                    chain_fingerprint = excluded.chain_fingerprint,
                    episode_key = excluded.episode_key,
                    not_before = excluded.not_before,
                    reason = excluded.reason",
                params![
                    record.agent_id,
                    record.message_id,
                    record.chain_fingerprint,
                    record.episode_key,
                    record.not_before.to_rfc3339(),
                    record.reason,
                ],
            )?;
            Ok(true)
        })
    }

    pub fn provider_queue_parking(
        &self,
        agent_id: &str,
        message_id: &str,
    ) -> Result<Option<ProviderQueueParking>> {
        self.with_read(|connection| read_parking(connection, agent_id, message_id))
    }

    pub fn clear_provider_queue_parking(&self, agent_id: &str, message_id: &str) -> Result<()> {
        self.transaction(|tx| {
            tx.execute(
                "DELETE FROM provider_queue_parking WHERE agent_id = ?1 AND message_id = ?2",
                params![agent_id, message_id],
            )?;
            Ok(())
        })
    }

    pub fn clear_agent_provider_queue_parking(&self, agent_id: &str) -> Result<()> {
        self.transaction(|tx| {
            tx.execute(
                "DELETE FROM provider_queue_parking WHERE agent_id = ?1",
                [agent_id],
            )?;
            Ok(())
        })
    }

    pub fn next_provider_queue_recheck_at(&self, agent_id: &str) -> Result<Option<DateTime<Utc>>> {
        self.with_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT parking.not_before
                 FROM provider_queue_parking AS parking
                 JOIN queue_entries AS source
                   ON source.agent_id = parking.agent_id
                  AND source.message_id = parking.message_id
                 WHERE parking.agent_id = ?1 AND source.status IN ('queued', 'interrupted')",
            )?;
            let mut earliest = None;
            // Parse every row: SQL MIN over text could hide a corrupt deadline
            // behind a valid one, or order equivalent timestamp offsets wrongly.
            for raw in statement.query_map([agent_id], |row| row.get::<_, String>(0))? {
                let deadline: DateTime<Utc> = raw?
                    .parse()
                    .context("invalid provider queue parking deadline")?;
                earliest =
                    Some(earliest.map_or(deadline, |current: DateTime<Utc>| current.min(deadline)));
            }
            Ok(earliest)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(dir: &tempfile::TempDir) -> Result<RuntimeDb> {
        RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )
    }

    fn parking(agent_id: &str, message_id: &str, seconds: i64) -> ProviderQueueParking {
        ProviderQueueParking {
            agent_id: agent_id.into(),
            message_id: message_id.into(),
            chain_fingerprint: "chain-a".into(),
            episode_key: "episode-a".into(),
            not_before: "2026-10-24T17:00:00Z".parse::<DateTime<Utc>>().unwrap()
                + Duration::seconds(seconds),
            reason: "chain_cooling".into(),
        }
    }

    fn queue_source(db: &RuntimeDb, agent_id: &str, message_id: &str, status: &str) -> Result<()> {
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO queue_entries (
                    message_id, agent_id, priority, status, created_at, updated_at, payload_json
                 ) VALUES (?1, ?2, 'normal', ?3, ?4, ?4, '{}')
                 ON CONFLICT(message_id) DO UPDATE SET status = excluded.status",
                params![
                    message_id,
                    agent_id,
                    status,
                    timestamp(parking(agent_id, message_id, 0).not_before)
                ],
            )?;
            Ok(())
        })
    }

    #[test]
    fn queue_parking_restart_replay_does_not_write_or_extend_deadline() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = open(&dir)?;
        let mut record = parking("agent-a", "message-a", 60);
        record.not_before += Duration::nanoseconds(123_456);
        queue_source(&db, &record.agent_id, &record.message_id, "queued")?;
        assert!(db.park_provider_queue(&record)?);
        assert!(!db.park_provider_queue(&record)?);
        drop(db);
        let db = open(&dir)?;
        assert!(!db.park_provider_queue(&record)?);
        assert_eq!(
            db.provider_queue_parking("agent-a", "message-a")?,
            Some(record.clone())
        );
        assert_eq!(
            db.next_provider_queue_recheck_at("agent-a")?,
            Some(record.not_before)
        );
        let mut changed = record;
        changed.episode_key = "episode-b".into();
        changed.reason = "recovery_budget_exhausted".into();
        assert!(db.park_provider_queue(&changed)?);
        assert!(!db.park_provider_queue(&changed)?);
        Ok(())
    }

    #[test]
    fn queue_parking_rechecks_and_clears_are_agent_and_message_scoped() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = open(&dir)?;
        queue_source(&db, "agent-a", "same-message", "queued")?;
        queue_source(&db, "agent-a", "other-message", "queued")?;
        for record in [
            parking("agent-a", "same-message", 60),
            parking("agent-a", "other-message", 30),
            parking("agent-b", "same-message", 10),
        ] {
            db.park_provider_queue(&record)?;
        }
        assert_eq!(
            db.next_provider_queue_recheck_at("agent-a")?,
            Some(parking("agent-a", "other-message", 30).not_before)
        );
        db.clear_provider_queue_parking("agent-a", "other-message")?;
        assert!(db
            .provider_queue_parking("agent-a", "same-message")?
            .is_some());
        db.clear_agent_provider_queue_parking("agent-a")?;
        assert_eq!(db.next_provider_queue_recheck_at("agent-a")?, None);
        assert!(db
            .provider_queue_parking("agent-b", "same-message")?
            .is_some());
        Ok(())
    }

    #[test]
    fn queue_parking_rechecks_ignore_claimed_terminal_missing_and_foreign_sources() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = open(&dir)?;
        for status in [
            "dequeued",
            "processed",
            "interjected",
            "aborted",
            "dropped",
            "quarantined",
        ] {
            let message_id = format!("message-{status}");
            queue_source(&db, "agent-a", &message_id, status)?;
            db.park_provider_queue(&parking("agent-a", &message_id, 0))?;
        }
        db.park_provider_queue(&parking("agent-a", "missing-message", 0))?;
        queue_source(&db, "agent-b", "foreign-message", "queued")?;
        db.park_provider_queue(&parking("agent-a", "foreign-message", 0))?;
        queue_source(&db, "agent-a", "queued-message", "queued")?;
        queue_source(&db, "agent-a", "interrupted-message", "interrupted")?;
        db.park_provider_queue(&parking("agent-a", "queued-message", 60))?;
        db.park_provider_queue(&parking("agent-a", "interrupted-message", 30))?;
        assert_eq!(
            db.next_provider_queue_recheck_at("agent-a")?,
            Some(parking("agent-a", "interrupted-message", 30).not_before)
        );
        queue_source(&db, "agent-a", "interrupted-message", "dequeued")?;
        assert_eq!(
            db.next_provider_queue_recheck_at("agent-a")?,
            Some(parking("agent-a", "queued-message", 60).not_before)
        );
        queue_source(&db, "agent-a", "queued-message", "processed")?;
        assert_eq!(db.next_provider_queue_recheck_at("agent-a")?, None);
        assert!(db
            .provider_queue_parking("agent-a", "missing-message")?
            .is_some());
        assert!(db
            .provider_queue_parking("agent-a", "interrupted-message")?
            .is_some());
        Ok(())
    }

    #[test]
    fn queue_parking_invalid_deadlines_fail_closed_even_when_not_the_minimum() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = open(&dir)?;
        queue_source(&db, "agent-a", "corrupt-message", "queued")?;
        queue_source(&db, "agent-a", "valid-message", "interrupted")?;
        let invalid = parking("agent-a", "corrupt-message", 90);
        db.park_provider_queue(&invalid)?;
        db.park_provider_queue(&parking("agent-a", "valid-message", 10))?;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE provider_queue_parking SET not_before = 'not-a-date'
                 WHERE agent_id = 'agent-a' AND message_id = 'corrupt-message'",
                [],
            )?;
            Ok(())
        })?;
        assert!(db
            .provider_queue_parking("agent-a", "corrupt-message")
            .is_err());
        assert!(db.next_provider_queue_recheck_at("agent-a").is_err());
        assert!(db.park_provider_queue(&invalid).is_err());
        assert_eq!(db.next_provider_queue_recheck_at("agent-b")?, None);
        let mut invalid_reason = parking("agent-b", "message", 30);
        invalid_reason.reason = "arbitrary_retry".into();
        assert!(db.park_provider_queue(&invalid_reason).is_err());
        Ok(())
    }
}
