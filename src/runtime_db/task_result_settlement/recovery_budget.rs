use super::*;

pub(crate) const TASK_RESULT_RECOVERY_LIMIT: u32 = 5;

// Filter before LIMIT; final admission still checks the exact timestamp.
pub(super) fn candidates_for_owner_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    work_item_id: Option<&str>,
    root_attempt_id: &str,
    now: DateTime<Utc>,
    limit: usize,
) -> Result<Vec<TaskResultSettlementRecord>> {
    records_tx(
        tx,
        "SELECT settlements.payload_json
         FROM task_result_settlements AS settlements
         WHERE settlements.agent_id = ?1 AND settlements.work_item_id IS ?2
           AND (
             settlements.state = 'persisted_pending'
             OR (settlements.state = 'caller_admitted' AND NOT EXISTS (
               SELECT 1 FROM execution_protocol_attempts AS attempts
               WHERE attempts.agent_id = settlements.agent_id
                 AND attempts.attempt_id = settlements.activation_id
                 AND attempts.lifecycle_state = 'open'
             ))
           )
           AND (
             EXISTS (
               SELECT 1 FROM task_result_recovery_admissions AS admissions
               WHERE admissions.result_identity = settlements.result_identity
                 AND admissions.recovery_epoch = settlements.recovery_epoch
                 AND admissions.root_attempt_id = ?3
             )
             OR (
               settlements.recovery_paused_at IS NULL
               AND settlements.recovery_attempts < ?4
               AND (
                 settlements.last_recovery_admitted_at IS NULL
                 OR julianday(settlements.last_recovery_admitted_at,
                   printf('+%d seconds', 30 * (1 << MIN(settlements.recovery_attempts, ?4 - 1))))
                   <= julianday(?5)
               )
             )
           )
         ORDER BY settlements.created_at ASC, settlements.result_identity ASC
         LIMIT ?6",
        params![
            agent_id,
            work_item_id,
            root_attempt_id,
            TASK_RESULT_RECOVERY_LIMIT,
            timestamp(now),
            limit.min(64) as i64,
        ],
    )
}

fn recheck_delay(attempts: u32) -> Duration {
    Duration::seconds(30 * (1_i64 << attempts.min(TASK_RESULT_RECOVERY_LIMIT - 1)))
}

fn by_identity_tx(
    tx: &Transaction<'_>,
    identity: &str,
) -> Result<Option<TaskResultSettlementRecord>> {
    tx.query_row(
        "SELECT payload_json FROM task_result_settlements WHERE result_identity = ?1",
        [identity],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
    .transpose()
}

pub(super) fn recovery_admission_eligible_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
    root_attempt_id: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    if record.state == TaskResultSettlementState::Settled {
        return Ok(false);
    }
    let charged = tx.query_row(
        "SELECT EXISTS (
           SELECT 1 FROM task_result_recovery_admissions
           WHERE result_identity = ?1 AND recovery_epoch = ?2 AND root_attempt_id = ?3
         )",
        params![
            record.result_identity,
            record.recovery_epoch,
            root_attempt_id
        ],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(charged
        || (record.recovery_paused_at.is_none()
            && record.recovery_attempts < TASK_RESULT_RECOVERY_LIMIT
            && record.last_recovery_admitted_at.is_none_or(|admitted_at| {
                admitted_at + recheck_delay(record.recovery_attempts) <= now
            })))
}

/// Call only for a result selected into an actual recovery root's canonical
/// snapshot, in that admission transaction. Provider fallback uses the same root.
/// Returning false means that the new root must not admit this result.
pub(crate) fn charge_recovery_admission_tx(
    tx: &Transaction<'_>,
    result_identity: &str,
    root_attempt_id: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    anyhow::ensure!(
        !root_attempt_id.is_empty(),
        "recovery root attempt is required"
    );
    let Some(mut record) = by_identity_tx(tx, result_identity)? else {
        return Ok(false);
    };
    if !recovery_admission_eligible_tx(tx, &record, root_attempt_id, now)? {
        return Ok(false);
    }
    let inserted = tx.execute(
        "INSERT INTO task_result_recovery_admissions
         (result_identity, recovery_epoch, root_attempt_id, admitted_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(result_identity, recovery_epoch, root_attempt_id) DO NOTHING",
        params![
            result_identity,
            record.recovery_epoch,
            root_attempt_id,
            timestamp(now)
        ],
    )?;
    if inserted == 0 {
        return Ok(true);
    }
    record.recovery_attempts += 1;
    record.last_recovery_root_attempt_id = Some(root_attempt_id.to_owned());
    record.last_recovery_admitted_at = Some(now);
    record.updated_at = now;
    if record.recovery_attempts == TASK_RESULT_RECOVERY_LIMIT {
        // The final admitted root may still deliver successfully. Only automatic
        // admission is paused; reporting waits until that root is no longer open.
        record.recovery_paused_at = Some(now);
        record.deferred_reason = Some("recovery_budget_exhausted".into());
        record.next_recheck_at = None;
    } else {
        record.next_recheck_at = Some(now + recheck_delay(record.recovery_attempts));
    }
    update_tx(tx, &record)?;
    Ok(true)
}

impl TaskResultSettlementRepository<'_> {
    pub(crate) fn recovery_root_is_charged(
        &self,
        record: &TaskResultSettlementRecord,
        root_attempt_id: &str,
    ) -> Result<bool> {
        self.db.with_read(|connection| {
            connection
                .query_row(
                    "SELECT EXISTS (
                       SELECT 1 FROM task_result_recovery_admissions
                       WHERE result_identity = ?1 AND recovery_epoch = ?2 AND root_attempt_id = ?3
                     )",
                    params![
                        record.result_identity,
                        record.recovery_epoch,
                        root_attempt_id
                    ],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        })
    }

    pub(crate) fn unreported_paused(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<TaskResultSettlementRecord>> {
        self.db.with_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT payload_json FROM task_result_settlements
                 WHERE agent_id = ?1 AND state != 'settled'
                   AND recovery_paused_at IS NOT NULL AND recovery_diagnostic_at IS NULL
                   AND NOT EXISTS (
                     SELECT 1 FROM execution_protocol_attempts attempts
                     WHERE attempts.agent_id = task_result_settlements.agent_id
                       AND attempts.attempt_id = task_result_settlements.activation_id
                       AND attempts.lifecycle_state = 'open'
                   )
                 ORDER BY recovery_paused_at, result_identity LIMIT ?2",
            )?;
            let rows = statement.query_map(params![agent_id, limit as i64], |row| {
                row.get::<_, String>(0)
            })?;
            rows.map(|row| serde_json::from_str(&row?).map_err(Into::into))
                .collect()
        })
    }

    pub(crate) fn mark_recovery_diagnostic(
        &self,
        result_identity: &str,
        epoch: u64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.db.transaction(|tx| {
            if let Some(mut record) = by_identity_tx(tx, result_identity)? {
                if record.recovery_epoch == epoch
                    && record.recovery_paused_at.is_some()
                    && record.recovery_diagnostic_at.is_none()
                {
                    record.recovery_diagnostic_at = Some(now);
                    record.updated_at = now;
                    update_tx(tx, &record)?;
                }
            }
            Ok(())
        })
    }

    /// Caller must be the operator-only, identity-targeted repair control path.
    pub(crate) fn rearm_recovery(
        &self,
        agent_id: &str,
        result_identity: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.db.transaction(|tx| {
            let Some(mut record) = by_identity_tx(tx, result_identity)? else {
                bail!("task result identity not found: {result_identity}");
            };
            anyhow::ensure!(record.agent_id == agent_id, "task result owner mismatch");
            if record.state == TaskResultSettlementState::Settled
                || record.recovery_paused_at.is_none()
            {
                return Ok(false);
            }
            record.recovery_epoch = record
                .recovery_epoch
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("task result recovery epoch overflow"))?;
            record.recovery_attempts = 0;
            record.last_recovery_root_attempt_id = None;
            record.last_recovery_admitted_at = None;
            record.recovery_paused_at = None;
            record.recovery_diagnostic_at = None;
            record.deferred_reason = Some("operator_rearmed".into());
            record.deferred_at = Some(now);
            record.next_recheck_at = Some(now + recheck_delay(0));
            record.updated_at = now;
            update_tx(tx, &record)?;
            let mut event = crate::types::AuditEvent::legacy(
                "task_result_recovery_rearmed",
                serde_json::json!({
                    "agent_id": agent_id,
                    "result_identity": result_identity,
                    "recovery_epoch": record.recovery_epoch,
                    "task_id": record.task_id,
                    "work_item_id": record.work_item_id,
                }),
            );
            event.created_at = now;
            crate::runtime_db::evidence::append_audit_event_tx(tx, Some(agent_id), &event)?;
            Ok(true)
        })
    }
}
