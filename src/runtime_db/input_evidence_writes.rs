//! Scoped canonical writer acknowledgements. Every helper borrows the original
//! transaction; callers supply their already-reserved identity namespace.

use anyhow::{ensure, Context, Result};
use rusqlite::{OptionalExtension, Transaction};

use crate::{
    runtime::resident_input_evidence::{CommittedInputEvidenceDelta, InputEvidenceScope},
    runtime_db::input_evidence_snapshot::{
        register_committed_input_evidence_tx, CanonicalInputEvidence,
    },
    types::{AgentIdentityRecord, AgentRegistryStatus},
};

#[derive(Debug, Clone)]
pub(crate) struct InputEvidenceCommit<T> {
    pub record: T,
    pub input_delta: CommittedInputEvidenceDelta,
    pub agent_state: Option<super::transitions::AgentStateMutation>,
    pub work_delta: Option<crate::runtime::resident_work::CommittedWorkDelta>,
    pub task_delta: Option<crate::runtime::resident_task::CommittedTaskDelta>,
    pub work_material_budget_views: Option<
        std::sync::Arc<Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>>,
    >,
    pub task_material_budget_views:
        Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

pub(crate) fn validate_input_evidence_scope_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    agent_id: &str,
) -> Result<()> {
    ensure!(
        scope.agent_id == agent_id && scope.identity_incarnation > 0,
        "input evidence writer scope differs from the reserved namespace"
    );
    let payload: String = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?
        .context("input evidence writer requires its durable identity")?;
    let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
    ensure!(
        identity.agent_id == agent_id
            && identity.incarnation == scope.identity_incarnation
            && identity.status == AgentRegistryStatus::Active,
        "input evidence writer identity incarnation is no longer current"
    );
    Ok(())
}

/// A conservative owner group may include an uncreated identity. This member
/// has no business writes or evidence delta; only its original gate is shared.
pub(crate) fn validate_readonly_input_reservation_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<()> {
    let facts =
        super::context_inputs::ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    if facts.identity.is_some() {
        super::input_evidence_snapshot::require_original_input_recovery_scope(tx, scope)?;
    } else {
        ensure!(
            facts.state.is_none(),
            "read-only input reservation has State without a durable identity"
        );
    }
    Ok(())
}

pub(crate) fn acknowledge_input_evidence_tx<T>(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    record: T,
    facts: &[CanonicalInputEvidence],
) -> Result<InputEvidenceCommit<T>> {
    let input_delta = register_committed_input_evidence_tx(tx, scope, facts)?;
    let agent_state = if facts
        .iter()
        .any(|fact| matches!(fact, CanonicalInputEvidence::Message(_)))
    {
        synchronize_message_count_tx(tx, &scope.agent_id)?
    } else {
        None
    };
    Ok(InputEvidenceCommit {
        work_delta: None,
        task_delta: None,
        work_material_budget_views: None,
        task_material_budget_views: Vec::new(),
        record,
        input_delta,
        agent_state,
    })
}

/// Message count is a derived State fact of the committing transaction. The
/// root receives the exact persisted value, including non-admission producers.
pub(super) fn synchronize_message_count_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<Option<super::transitions::AgentStateMutation>> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id=?1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let previous: crate::types::AgentState = serde_json::from_str(&payload)?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM messages WHERE agent_id=?1",
        [agent_id],
        |row| row.get(0),
    )?;
    let count = usize::try_from(count).context("canonical message count cannot be represented")?;
    if previous.total_message_count == count {
        return Ok(None);
    }
    let mut record = previous.clone();
    record.total_message_count = count;
    super::evidence::upsert_agent_state_tx(tx, &record)?;
    Ok(Some(super::transitions::AgentStateMutation {
        expected: Some(Box::new(previous)),
        record: Box::new(record),
    }))
}

/// Multiple pieces of one transaction may refresh the same related alias.
/// The later exact readback wins by monotonic revision; an equal revision may
/// never claim different bytes. This does not combine different transactions.
pub(crate) fn merge_input_evidence_delta(
    into: &mut CommittedInputEvidenceDelta,
    delta: CommittedInputEvidenceDelta,
) -> Result<()> {
    if let Some(advance) = delta.query_advance {
        if let Some(existing) = &into.query_advance {
            ensure!(std::sync::Arc::ptr_eq(existing, &advance), "one Input delta has different original birth advances");
        } else {
            into.query_advance = Some(advance);
        }
    }
    if let Some(invalidation) = delta.output_recall_invalidation {
        if let Some(existing) = &into.output_recall_invalidation {
            existing.merge(&invalidation)?;
        } else {
            into.output_recall_invalidation = Some(invalidation);
        }
    }
    for record in delta.records {
        ensure!(
            !into
                .removals
                .iter()
                .any(|removed| removed.key == record.locator.key),
            "one evidence transaction both prunes and publishes the same record"
        );
        if let Some(index) = into
            .records
            .iter()
            .position(|old| old.locator.key == record.locator.key)
        {
            let old = &into.records[index];
            ensure!(
                old.locator.scope == record.locator.scope,
                "mixed evidence scopes"
            );
            ensure!(
                old.locator.revision <= record.locator.revision,
                "evidence transaction delta moved backwards"
            );
            if old.locator.revision == record.locator.revision {
                ensure!(old == &record, "same evidence revision has different facts");
            }
            into.records[index] = record;
        } else {
            into.records.push(record);
        }
    }
    for removal in delta.removals {
        ensure!(
            !into
                .records
                .iter()
                .any(|record| record.locator.key == removal.key),
            "one evidence transaction both publishes and prunes the same record"
        );
        into.removals.push(removal);
    }
    into.execution.merge(delta.execution)?;
    into.turn_transcript_keys.extend(delta.turn_transcript_keys);
    into.message_turn_keys.extend(delta.message_turn_keys);
    Ok(())
}

/// Used only by the retention coordinator to plan reservations. The SQL
/// transaction revalidates the exact supplied scope before deleting any rows.
pub(crate) fn current_input_evidence_scopes_tx(
    tx: &Transaction<'_>,
    agents: &std::collections::BTreeSet<String>,
) -> Result<Vec<InputEvidenceScope>> {
    let mut scopes = Vec::new();
    for agent in agents {
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
                [agent],
                |row| row.get(0),
            )
            .optional()?;
        let Some(payload) = payload else { continue };
        let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
        ensure!(
            identity.agent_id == *agent && identity.incarnation > 0,
            "retention identity projection mismatch"
        );
        if identity.status == AgentRegistryStatus::Active {
            scopes.push(InputEvidenceScope {
                agent_id: agent.clone(),
                identity_incarnation: identity.incarnation,
            });
        }
    }
    Ok(scopes)
}

#[cfg(test)]
#[path = "input_evidence_writes_tests.rs"]
mod tests;
