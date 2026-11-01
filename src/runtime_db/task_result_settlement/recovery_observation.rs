//! Same-transaction recovery predicates for a finite Task capture.
use super::*;
use crate::runtime::task_reads::{
    durable_recovery_source_digest, exact_recovery_source_digest, TaskRecoveryCandidate,
    TaskRecoveryReplayObservation, TaskRecoverySourceObservation,
};

pub(in crate::runtime_db) fn paused_replay_root_is_charged_tx(
    tx: &Transaction<'_>,
    source: &MessageEnvelope,
    record: &TaskResultSettlementRecord,
) -> Result<bool> {
    let root = format!("activation:message:{}", source.id);
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_result_recovery_admissions
         WHERE result_identity=?1 AND recovery_epoch=?2 AND root_attempt_id=?3)",
        params![record.result_identity, record.recovery_epoch, root],
        |row| row.get(0),
    )?)
}

pub(in crate::runtime_db) fn observe_recovery_source_tx(
    tx: &Transaction<'_>,
    source: &MessageEnvelope,
    paused: &[&TaskResultSettlementRecord],
    now: DateTime<Utc>,
) -> Result<TaskRecoverySourceObservation> {
    let candidate = obligation::exact_waiter_recovery_source_tx(tx, source, None)?;
    let wait = if candidate.is_some() {
        exact_waiter::exact_wait_for_source_tx(tx, source, None)?
    } else {
        None
    };
    let wait_has_successor = match &wait {
        Some(wait) => tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM wait_conditions newer
             WHERE newer.agent_id=?1 AND newer.work_item_id IS ?2
               AND newer.rowid>(SELECT rowid FROM wait_conditions WHERE wait_condition_id=?3))",
            params![source.agent_id, source.work_item_id, wait.id],
            |row| row.get(0),
        )?,
        None => false,
    };
    let queue_interrupted = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM queue_entries
         WHERE agent_id=?1 AND message_id=?2 AND status='interrupted')",
        params![source.agent_id, source.id],
        |row| row.get(0),
    )?;
    let agent_not_stopped = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_states WHERE agent_id=?1 AND status!='stopped')",
        [&source.agent_id],
        |row| row.get(0),
    )?;
    let mut paused_replays = BTreeMap::new();
    for record in paused {
        anyhow::ensure!(
            record.agent_id == source.agent_id,
            "recovery observation crosses Task owner"
        );
        let root_is_charged = paused_replay_root_is_charged_tx(tx, source, record)?;
        let interrupted_anchor = if root_is_charged {
            live_turn::interrupted_admitted_result_anchor_tx(
                tx,
                &source.agent_id,
                &record.message_id,
                now,
            )?
        } else {
            None
        };
        paused_replays.insert(
            record.result_identity.clone(),
            TaskRecoveryReplayObservation {
                recovery_epoch: record.recovery_epoch,
                root_is_charged,
                interrupted_anchor,
            },
        );
    }
    Ok(TaskRecoverySourceObservation {
        exact_source_digest: Some(exact_recovery_source_digest(source)?),
        durable_source_digest: Some(durable_recovery_source_digest(source)?),
        candidate: candidate.map(|record| TaskRecoveryCandidate {
            result_identity: record.result_identity,
            task_id: record.task_id,
        }),
        exact_wait_id: wait.map(|wait| wait.id),
        wait_has_successor,
        queue_interrupted,
        agent_not_stopped,
        settled: obligation::settled_recovery_source_tx(tx, source)?,
        paused_replays,
    })
}
