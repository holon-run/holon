//! Bounded cold-bootstrap inputs. Metadata fixes the read view and rejects an
//! oversized accepted queue before any Message payload is materialized.
use anyhow::{ensure, Result};
#[cfg(test)]
use rusqlite::params;
use rusqlite::Transaction;

use super::{
    context_inputs::{ContextDurableRead, ContextScopeFacts},
    repositories, RuntimeDb,
};
use crate::{
    runtime::{
        input_evidence_budget::{InputEvidenceBudgetLedger, PendingBodyCharge},
        resident_input_evidence::InputEvidenceScope,
    },
    types::{MessageEnvelope, QueueEntryStatus},
};

pub(crate) struct BootstrapPendingMetadata {
    pub facts: ContextScopeFacts,
    pub count: usize,
}

pub(crate) struct PreparedPendingQueue {
    pub metadata: BootstrapPendingMetadata,
    pub messages: Vec<MessageEnvelope>,
}

impl RuntimeDb {
    pub(crate) fn bootstrap_scope_facts(
        &self,
        scope: &InputEvidenceScope,
    ) -> Result<ContextScopeFacts> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let facts = current_scope_facts_tx(&tx, scope)?;
        tx.commit()?;
        Ok(facts)
    }

    pub(crate) fn bootstrap_pending_metadata(
        &self,
        scope: &InputEvidenceScope,
        budget: &InputEvidenceBudgetLedger,
    ) -> Result<BootstrapPendingMetadata> {
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let metadata = pending_metadata_tx(&tx, scope, budget)?;
        tx.commit()?;
        Ok(metadata)
    }
}

fn current_scope_facts_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
) -> Result<ContextScopeFacts> {
    current_scope_facts_inner_tx(tx, scope, false)
}
fn current_scope_facts_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    original_recovery: bool,
) -> Result<ContextScopeFacts> {
    let facts = ContextDurableRead::new(tx, &scope.agent_id).scope_facts()?;
    ensure!(
        facts
            .identity
            .as_ref()
            .is_some_and(|identity| identity.agent_id == scope.agent_id
                && identity.incarnation == scope.identity_incarnation
                && (identity.status == crate::types::AgentRegistryStatus::Active
                    || (original_recovery
                        && identity.status == crate::types::AgentRegistryStatus::Deleting))),
        "bootstrap pending inputs require the reserved current identity"
    );
    Ok(facts)
}

pub(crate) fn pending_metadata_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    budget: &InputEvidenceBudgetLedger,
) -> Result<BootstrapPendingMetadata> {
    pending_metadata_inner_tx(tx, scope, budget, false)
}
pub(crate) fn pending_metadata_for_input_recovery_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    budget: &InputEvidenceBudgetLedger,
) -> Result<BootstrapPendingMetadata> {
    pending_metadata_inner_tx(tx, scope, budget, true)
}
fn pending_metadata_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    budget: &InputEvidenceBudgetLedger,
    original_recovery: bool,
) -> Result<BootstrapPendingMetadata> {
    let facts = current_scope_facts_inner_tx(tx, scope, original_recovery)?;
    let (count, payload_bytes, missing): (i64, i64, i64) = tx.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(CAST(q.payload_json AS BLOB)) + COALESCE(length(CAST(m.payload_json AS BLOB)),0)),0),
                COALESCE(SUM(CASE WHEN m.message_id IS NULL OR m.agent_id != q.agent_id THEN 1 ELSE 0 END),0)
         FROM queue_entries q LEFT JOIN messages m ON m.message_id=q.message_id
         WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted')",
        [&scope.agent_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let count = u64::try_from(count)?;
    ensure!(
        count <= budget.limits().record_limit,
        "bootstrap pending queue exceeds input record limit"
    );
    // Full envelope/queue encoded bytes conservatively gate materialization;
    // only exact typed MessageBody bytes below become logical Accepted charges.
    budget.preflight_actual_pending(scope, count, u64::try_from(payload_bytes)?)?;
    ensure!(
        missing == 0,
        "canonical pending queue has no same-agent Message"
    );
    Ok(BootstrapPendingMetadata {
        facts,
        count: usize::try_from(count)?,
    })
}

pub(crate) fn pending_queue_inputs_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedPendingQueue> {
    pending_queue_inputs_inner_tx(tx, scope, budget, false)
}
pub(crate) fn pending_queue_inputs_for_input_recovery_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedPendingQueue> {
    pending_queue_inputs_inner_tx(tx, scope, budget, true)
}
fn pending_queue_inputs_inner_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    budget: &InputEvidenceBudgetLedger,
    original_recovery: bool,
) -> Result<PreparedPendingQueue> {
    let metadata = pending_metadata_inner_tx(tx, scope, budget, original_recovery)?;
    let mut statement = tx.prepare(
        "SELECT q.message_id,q.agent_id,q.status,q.payload_json,m.message_id,m.agent_id,m.message_seq,m.payload_json
         FROM queue_entries q JOIN messages m ON m.message_id=q.message_id
         WHERE q.agent_id=?1 AND q.status IN ('queued','interrupted')
         ORDER BY q.updated_at ASC,q.created_at ASC,q.message_id DESC")?;
    let mut messages = Vec::with_capacity(metadata.count);
    for row in statement.query_map([&scope.agent_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, String>(7)?,
        ))
    })? {
        let (
            queue_id,
            queue_agent,
            status,
            queue_payload,
            message_id,
            message_agent,
            message_seq,
            payload,
        ) = row?;
        let queue = repositories::decode_queue_entry_payload(&queue_payload)?;
        let message = repositories::decode_message_payload(&payload)?;
        ensure!(
            queue.message_id == queue_id
                && queue.agent_id == queue_agent
                && queue.agent_id == scope.agent_id
                && serde_json::to_value(&queue.status)?.as_str() == Some(status.as_str())
                && matches!(
                    queue.status,
                    QueueEntryStatus::Queued | QueueEntryStatus::Interrupted
                ),
            "bootstrap queue columns differ from canonical payload"
        );
        ensure!(
            message.id == message_id
                && message.id == queue_id
                && message.agent_id == message_agent
                && message.agent_id == scope.agent_id
                && message.message_seq.map(i64::try_from).transpose()? == message_seq,
            "bootstrap pending Message columns differ from canonical payload"
        );
        messages.push(message);
    }
    ensure!(
        messages.len() == metadata.count,
        "bootstrap queue changed within its read snapshot"
    );
    messages.sort_by(|left, right| match (left.message_seq, right.message_seq) {
        (Some(left_seq), Some(right_seq)) => left_seq
            .cmp(&right_seq)
            .then_with(|| left.created_at.cmp(&right.created_at)),
        _ => left.created_at.cmp(&right.created_at),
    });
    Ok(PreparedPendingQueue { metadata, messages })
}

pub(crate) fn pending_queue_inputs_and_charges_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    budget: &InputEvidenceBudgetLedger,
) -> Result<PreparedPendingQueue> {
    let pending = pending_queue_inputs_tx(tx, scope, budget)?;
    let charges = pending
        .messages
        .iter()
        .map(|message| {
            Ok(PendingBodyCharge {
                message_id: message.id.clone(),
                body_bytes: crate::runtime::input_evidence_budget::serialized_bytes(&message.body)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    budget.install_actual_pending(scope, &charges)?;
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits,
        runtime::input_evidence_budget::{BudgetResource, InputEvidenceBudgetError},
        runtime_db::evidence::{
            append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx,
        },
        runtime_db::repositories::upsert_queue_entry_tx,
        types::{
            AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
            AgentVisibility, AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority,
            QueueEntryRecord,
        },
    };
    fn fixture() -> Result<(
        tempfile::TempDir,
        RuntimeDb,
        InputEvidenceScope,
        MessageEnvelope,
    )> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )?;
        let scope = InputEvidenceScope {
            agent_id: "bootstrap-inputs".into(),
            identity_incarnation: 1,
        };
        let message = db.transaction(|tx| {
            upsert_agent_identity_tx(
                tx,
                &AgentIdentityRecord::new(
                    &scope.agent_id,
                    AgentKind::Named,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                ),
            )?;
            let mut state = AgentState::new(&scope.agent_id);
            state.pending = 1;
            upsert_agent_state_tx(tx, &state)?;
            let message = MessageEnvelope::new(
                &scope.agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "accepted".into(),
                },
            );
            let (message, _) = append_message_tx(tx, &message)?;
            upsert_queue_entry_tx(
                tx,
                &QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: scope.agent_id.clone(),
                    priority: message.priority.clone(),
                    status: QueueEntryStatus::Queued,
                    created_at: message.created_at,
                    updated_at: message.created_at,
                },
            )?;
            Ok(message)
        })?;
        Ok((dir, db, scope, message))
    }
    #[test]
    fn cold_pending_read_ignores_unselected_history_and_installs_exact_body_charge() -> Result<()> {
        let (_dir, db, scope, message) = fixture()?;
        db.transaction(|tx| {
            let mut history = message.clone();
            history.id = "unselected-history".into();
            history.message_seq = None;
            append_message_tx(tx, &history)?;
            tx.execute(
                "UPDATE messages SET payload_json='not JSON' WHERE message_id='unselected-history'",
                [],
            )?;
            Ok(())
        })?;
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let inputs =
            db.transaction(|tx| pending_queue_inputs_and_charges_tx(tx, &scope, &budget))?;
        assert_eq!(inputs.messages, vec![message.clone()]);
        assert_eq!(inputs.metadata.count, 1);
        assert_eq!(
            budget.report()?.pending.body_bytes,
            u64::try_from(serde_json::to_vec(&message.body)?.len())?
        );
        Ok(())
    }
    #[test]
    fn metadata_budget_rejects_before_pending_payload_decode_or_normalization() -> Result<()> {
        let (_dir, db, scope, message) = fixture()?;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE messages SET payload_json=?2 WHERE message_id=?1",
                params![message.id, "x".repeat(4096)],
            )?;
            Ok(())
        })?;
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits {
            pending_per_agent_bytes: 1,
            ..Default::default()
        })?;
        for error in [
            db.bootstrap_pending_metadata(&scope, &budget)
                .err()
                .unwrap(),
            db.transaction(|tx| pending_queue_inputs_and_charges_tx(tx, &scope, &budget))
                .err()
                .unwrap(),
            db.timers()
                .normalize_wakes_for_recovery_in_scope(&scope, &budget)
                .err()
                .unwrap(),
        ] {
            assert!(
                matches!(
                    error.downcast_ref::<InputEvidenceBudgetError>(),
                    Some(InputEvidenceBudgetError::ResourceExhausted {
                        resource: BudgetResource::PendingAgentBytes,
                        ..
                    })
                ),
                "{error:#}"
            );
        }
        assert_eq!(budget.report()?.pending.count, 0);
        let status: String = db.connection()?.query_row(
            "SELECT status FROM queue_entries WHERE message_id=?1",
            [message.id],
            |r| r.get(0),
        )?;
        assert_eq!(status, "queued");
        Ok(())
    }
}
