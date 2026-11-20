//! Move an existing consumption grant, never discover a new one.
use super::*;
use std::collections::BTreeSet;

pub(crate) fn transfer_provider_admitted_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    predecessors: &BTreeSet<String>,
    activation_id: &str,
    now: DateTime<Utc>,
) -> Result<Vec<TaskResultSettlementRecord>> {
    let mut transferred = Vec::new();
    for predecessor in predecessors {
        for mut record in admitted_for_activation_tx(tx, agent_id, predecessor)? {
            // The captured task owner is deliberately not compared with the
            // inherited execution owner: exact waiters can cross WorkItems.
            if !record_matches_durable_task_tx(tx, &record)? {
                settle_invalid_or_stale_tx(tx, &mut record, now)?;
                continue;
            }
            record.activation_id = Some(activation_id.to_owned());
            record.updated_at = now;
            update_tx(tx, &record)?;
            transferred.push(record);
        }
    }
    Ok(transferred)
}
