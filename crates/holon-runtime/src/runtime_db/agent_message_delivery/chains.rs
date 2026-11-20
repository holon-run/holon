//! Shared delivery budgets for agent message chains.
//!
//! A chain is a set of deliveries that descend from one runtime-verified
//! admission source. Every branch shares one fixed allowance; the ledger keeps
//! no per-hop countdown on routing metadata and no client-supplied chain id.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction};

use super::delivery_by_message_id_tx;
use crate::domain::execution_protocol::{ExecutionAttempt, ExecutionAttemptState};
use crate::runtime_db::task_result_settlement::{
    context_admitted_settlements_on, TaskResultSettlementRecord, TaskResultSettlementState,
};
use crate::types::{
    AdmissionContext, AgentMessageDeliveryOutcome, AgentMessageDeliveryRecord, AuthorityClass,
    MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, TaskKind, TaskRecord,
};

/// First-release fixed allowance for one chain, including its first delivery.
pub(crate) const MESSAGE_CHAIN_MAX_DELIVERIES: i64 = 16;

/// Resolve the chain a newly prepared delivery is charged to.
///
/// Replies and forwards inherit the verified source delivery. Otherwise the
/// caller's live activation supplies one deterministic anchor: its main input
/// when that input is chainable, else the first chainable admitted input in
/// admission order. Without an anchor the delivery opens its own chain.
pub(crate) fn resolve_delivery_chain_tx(
    tx: &Transaction<'_>,
    candidate: &AgentMessageDeliveryRecord,
) -> Result<String> {
    // The selected route is retained for admission revalidation even when
    // preparation makes a direct send. Only the outgoing route proves inheritance.
    if let Some(source_id) = candidate
        .routing_context
        .as_ref()
        .and_then(|route| route.in_reply_to_message_id.as_deref())
        .filter(|source_id| {
            candidate
                .caller
                .inherited_routing_context
                .as_ref()
                .is_some_and(|route| route.message_id == *source_id)
        })
    {
        return source_chain_tx(tx, source_id).map(|anchor| anchor.chain_id);
    }
    if let (Some(agent_id), Some(turn_id)) = (
        candidate.caller.caller_agent_id.as_deref(),
        candidate.caller.current_turn_id.as_deref(),
    ) {
        if let Some(anchor) = activation_anchor_tx(tx, agent_id, turn_id)? {
            return Ok(anchor.chain_id);
        }
    }
    Ok(candidate.delivery_id.clone())
}

struct LiveActivation {
    attempt_id: String,
    recovery_of_attempt_id: Option<String>,
    source_message_id: String,
    turn: crate::types::TurnRecord,
}

struct ChainAnchor {
    chain_id: String,
}

fn source_chain_tx(tx: &Transaction<'_>, source_message_id: &str) -> Result<ChainAnchor> {
    let delivery = delivery_by_message_id_tx(tx, source_message_id)?.with_context(|| {
        format!("message chain source {source_message_id} is missing its canonical delivery")
    })?;
    if delivery.outcome != AgentMessageDeliveryOutcome::Accepted {
        bail!("message chain source {source_message_id} has no accepted canonical delivery");
    }
    chain_of_delivery_tx(tx, &delivery)
}

/// Pre-upgrade accepted deliveries carry no chain id. Their own delivery id is
/// the fixed compatibility anchor, and the first continuation counts as the
/// second delivery of that chain.
fn chain_of_delivery_tx(
    tx: &Transaction<'_>,
    delivery: &AgentMessageDeliveryRecord,
) -> Result<ChainAnchor> {
    let Some(chain_id) = delivery.message_chain_id.clone() else {
        return Ok(ChainAnchor {
            chain_id: delivery.delivery_id.clone(),
        });
    };
    let recorded: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_message_chain_budgets WHERE chain_id = ?1)",
        [&chain_id],
        |row| row.get(0),
    )?;
    if !recorded {
        bail!(
            "delivery {} records message chain {chain_id} without its budget row",
            delivery.delivery_id
        );
    }
    Ok(ChainAnchor { chain_id })
}

fn activation_anchor_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<ChainAnchor>> {
    let Some(activation) = live_activation_tx(tx, agent_id, turn_id)? else {
        return Ok(None);
    };
    let mut ordered: Vec<&str> = Vec::with_capacity(activation.turn.input_message_ids.len() + 1);
    ordered.push(activation.source_message_id.as_str());
    for message_id in &activation.turn.input_message_ids {
        if !ordered.contains(&message_id.as_str()) {
            ordered.push(message_id.as_str());
        }
    }
    for message_id in ordered {
        let payload = tx
            .query_row(
                "SELECT payload_json FROM messages WHERE agent_id = ?1 AND message_id = ?2",
                params![agent_id, message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .with_context(|| {
                format!("admitted message {message_id} is missing its durable envelope")
            })?;
        let message: MessageEnvelope = serde_json::from_str(&payload)?;
        if let Some(anchor) = chainable_source_tx(tx, agent_id, &activation, &message)? {
            return Ok(Some(anchor));
        }
    }
    // Some exact rejoin results are attached as TaskResult projections rather
    // than as queue inputs. Only the current activation's canonical admissions
    // participate, in the same stable order as the runtime context.
    for record in context_admitted_settlements_on(tx, agent_id, &activation.attempt_id)? {
        if let Some(anchor) = task_result_request_chain_tx(tx, &record)? {
            return Ok(Some(anchor));
        }
    }
    Ok(None)
}

fn live_activation_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    turn_id: &str,
) -> Result<Option<LiveActivation>> {
    let payload = tx
        .query_row(
            "SELECT payload_json FROM execution_protocol_attempts
             WHERE agent_id = ?1 AND lifecycle_state = 'open'",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let attempt: ExecutionAttempt = serde_json::from_str(&payload)?;
    if attempt.agent_id != agent_id
        || attempt.state != ExecutionAttemptState::Open
        || attempt.terminal_outcome_id.is_some()
        || attempt.terminal_at.is_some()
    {
        return Ok(None);
    }
    let state = tx
        .query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<super::ReplyRouteExecutionState>(&payload))
        .transpose()?;
    let Some(state) = state else {
        return Ok(None);
    };
    let Some(binding) = state.current_execution_binding.as_ref() else {
        return Ok(None);
    };
    if state.current_turn_id.as_deref() != Some(turn_id)
        || binding.turn_id != turn_id
        || binding.activation_id.as_deref() != Some(attempt.attempt_id.as_str())
        || attempt.source_message_id.as_deref() != Some(binding.source_message_id.as_str())
    {
        return Ok(None);
    }
    let payload = tx
        .query_row(
            "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND turn_id = ?2",
            params![agent_id, turn_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let turn: crate::types::TurnRecord = serde_json::from_str(&payload)?;
    if turn.agent_id != agent_id || turn.turn_id != turn_id || turn.terminal.is_some() {
        return Ok(None);
    }
    let owner_matches = match (&attempt.binding, turn.effective_owner()) {
        (
            crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id },
            crate::types::TurnOwner::WorkItem {
                work_item_id: owner,
            },
        ) => work_item_id == &owner,
        (
            crate::domain::execution_protocol::ExecutionBinding::Conversation { interaction_id },
            crate::types::TurnOwner::Conversation {
                interaction_id: owner,
            },
        ) => interaction_id == &owner,
        (
            crate::domain::execution_protocol::ExecutionBinding::AgentLifecycle { agent_id },
            crate::types::TurnOwner::AgentLifecycle { agent_id: owner },
        ) => agent_id == &owner,
        (
            crate::domain::execution_protocol::ExecutionBinding::Command,
            crate::types::TurnOwner::Command,
        ) => true,
        _ => false,
    };
    if !owner_matches {
        return Ok(None);
    }
    if !turn
        .input_message_ids
        .iter()
        .any(|id| id == &binding.source_message_id)
    {
        bail!(
            "live activation {} does not admit its bound source message",
            attempt.attempt_id
        );
    }
    Ok(Some(LiveActivation {
        attempt_id: attempt.attempt_id,
        recovery_of_attempt_id: attempt.recovery_of_attempt_id,
        source_message_id: binding.source_message_id.clone(),
        turn,
    }))
}

fn chainable_source_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    activation: &LiveActivation,
    message: &MessageEnvelope,
) -> Result<Option<ChainAnchor>> {
    if message.agent_id != agent_id
        || message.admission_context != Some(AdmissionContext::RuntimeOwned)
    {
        return Ok(None);
    }
    if message.kind == MessageKind::TaskResult
        && message.delivery_surface == Some(MessageDeliverySurface::TaskRejoin)
        && message.authority_class == AuthorityClass::RuntimeInstruction
        && matches!(&message.origin, MessageOrigin::Task { task_id }
            if message.task_id.as_deref() == Some(task_id))
    {
        return task_result_chain_tx(tx, activation, message, &message.id);
    }
    if message.kind != MessageKind::InternalFollowup
        || message.delivery_surface != Some(MessageDeliverySurface::RuntimeSystem)
        || !matches!(
            message.origin,
            MessageOrigin::System { .. } | MessageOrigin::Task { .. }
        )
    {
        return Ok(None);
    }
    let routed = message.routing_context.as_ref().is_some_and(|route| {
        route.message_id == message.id
            && route.recipient_agent_id == agent_id
            && route.sender_agent_id.is_some()
    });
    if routed {
        let delivery = delivery_by_message_id_tx(tx, &message.id)?.with_context(|| {
            format!(
                "admitted agent message {} is missing its canonical delivery",
                message.id
            )
        })?;
        if !delivery.authenticates_agent_message(message) {
            bail!(
                "admitted agent message {} is not authenticated by its delivery",
                message.id
            );
        }
        return chain_of_delivery_tx(tx, &delivery).map(Some);
    }
    if message.authority_class == AuthorityClass::RuntimeInstruction
        && matches!(&message.origin, MessageOrigin::System { subsystem }
            if subsystem == "task_result_recovery")
    {
        if let Some(result_message_id) = message.source_refs.get("task_result_message_id") {
            return task_result_chain_tx(tx, activation, message, result_message_id);
        }
    }
    Ok(None)
}

/// A precise existing-agent `TaskResult` continues the chain of the request
/// delivery it settles. Ordinary task results and unadmitted history do not.
fn task_result_chain_tx(
    tx: &Transaction<'_>,
    activation: &LiveActivation,
    message: &MessageEnvelope,
    result_message_id: &str,
) -> Result<Option<ChainAnchor>> {
    let payload = tx
        .query_row(
            "SELECT payload_json FROM task_result_settlements WHERE message_id = ?1",
            [result_message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let record: TaskResultSettlementRecord = serde_json::from_str(&payload)?;
    let admitted = record
        .activation_id
        .as_deref()
        .is_some_and(|activation_id| {
            activation_id == activation.attempt_id
                || activation.recovery_of_attempt_id.as_deref() == Some(activation_id)
        });
    if !admitted
        || record.state != TaskResultSettlementState::CallerAdmitted
        || record.agent_id != activation.turn.agent_id
        || record.message_id != result_message_id
    {
        return Ok(None);
    }
    let source_matches = if message.kind == MessageKind::TaskResult {
        message.task_id.as_deref() == Some(record.task_id.as_str())
    } else {
        message.source_refs.get("task_result_identity") == Some(&record.result_identity)
            && message.source_refs.get("task_result_rejoin_generation")
                == Some(&record.rejoin_generation.to_string())
            && record
                .source
                .matches_result_source_refs(&message.source_refs)
    };
    if !source_matches {
        return Ok(None);
    }
    task_result_request_chain_tx(tx, &record)
}

fn task_result_request_chain_tx(
    tx: &Transaction<'_>,
    record: &TaskResultSettlementRecord,
) -> Result<Option<ChainAnchor>> {
    let payload = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1",
            [&record.task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| format!("settled task {} is missing", record.task_id))?;
    let task: TaskRecord = serde_json::from_str(&payload)?;
    if task.kind != TaskKind::AgentMessageWait || task.agent_id != record.agent_id {
        return Ok(None);
    }
    let Some(request_delivery_id) = task
        .detail
        .as_ref()
        .and_then(|detail| detail.get("request_delivery_id"))
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    let delivery = tx
        .query_row(
            "SELECT payload_json FROM agent_message_deliveries WHERE delivery_id = ?1",
            [request_delivery_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| {
            format!(
                "task {} records request delivery {request_delivery_id} without a ledger row",
                task.id
            )
        })?;
    let delivery: AgentMessageDeliveryRecord = serde_json::from_str(&delivery)?;
    if delivery.outcome != AgentMessageDeliveryOutcome::Accepted
        || delivery.caller.current_task_id.as_deref() != Some(task.id.as_str())
        || delivery.caller.caller_agent_id.as_deref() != Some(task.agent_id.as_str())
    {
        bail!(
            "task {} request delivery {request_delivery_id} is not an accepted request of that task",
            task.id
        );
    }
    chain_of_delivery_tx(tx, &delivery).map(Some)
}

/// Check the limit before any target queue or wait writes in the admission transaction.
pub(crate) fn exhausted_chain_limit_tx(
    tx: &Transaction<'_>,
    chain_id: &str,
) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT max_deliveries FROM agent_message_chain_budgets
             WHERE chain_id = ?1 AND accepted_count = max_deliveries",
            [chain_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Charge only after the queue write applies, in the same transaction that
/// checked the limit and will commit the delivery ledger.
pub(crate) fn charge_chain_budget_tx(tx: &Transaction<'_>, chain_id: &str) -> Result<()> {
    let changed = tx.execute(
        "UPDATE agent_message_chain_budgets
         SET accepted_count = accepted_count + 1
         WHERE chain_id = ?1 AND accepted_count < max_deliveries",
        [chain_id],
    )?;
    if changed == 1 {
        return Ok(());
    }
    // A compatibility anchor already stands for one accepted delivery.
    let compatibility_anchor: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_message_deliveries
         WHERE delivery_id = ?1
           AND json_extract(payload_json, '$.outcome') = 'accepted'
           AND json_extract(payload_json, '$.message_chain_id') IS NULL)",
        [chain_id],
        |row| row.get(0),
    )?;
    let initial_count = if compatibility_anchor { 2 } else { 1 };
    tx.execute(
        "INSERT INTO agent_message_chain_budgets
           (chain_id, max_deliveries, accepted_count, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            chain_id,
            MESSAGE_CHAIN_MAX_DELIVERIES,
            initial_count,
            super::timestamp(Utc::now()),
        ],
    )?;
    Ok(())
}
