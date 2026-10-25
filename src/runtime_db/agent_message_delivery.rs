//! Durable internal Agent message delivery ledger.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, Transaction};

use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, WorkItemExecutionRecord,
        WorkItemExecutionState,
    },
    runtime_db::agent_relations::canonical_relations_from_connection,
    runtime_db::AgentMessageDeliveryRepository,
    runtime_error::RuntimeError,
    types::{
        AdmissionContext, AgentCanonicalDurability, AgentIdentityLifecycle, AgentIdentityRecord,
        AgentLifecycleAttachment, AgentMessageAdmissionEvidence, AgentMessageDeliveryError,
        AgentMessageDeliveryOutcome, AgentMessageDeliveryReceipt, AgentMessageDeliveryRecord,
        AgentMessageDeliveryRejectionCode, AgentMessageDeliveryState, AgentMessageDerivedGrant,
        AgentMessagePrincipalKind, AgentMessageRoutingContext, AgentPolicyEffect,
        AgentRegistryStatus, AgentState, AgentStatus, MessageDeliverySurface, MessageEnvelope,
        MessageKind, MessageOrigin, TurnOwner, TurnRecord,
    },
};

const DELIVERY_DIAGNOSTIC_LIMIT: usize = 512;

pub(crate) struct ResolvedAgentMessageReplyContext {
    pub activation_id: String,
    pub turn_id: String,
    pub routing_context: AgentMessageRoutingContext,
}

// A projection of canonical admission evidence, never a second routing ledger.
struct ReplyRouteRegistry {
    activation_id: String,
    turn_id: String,
    routes: Vec<AgentMessageRoutingContext>,
    latest_input_routed: bool,
}

#[derive(serde::Deserialize)]
struct ReplyRouteExecutionState {
    current_turn_id: Option<String>,
    current_execution_binding: Option<crate::types::WorkItemExecutionBinding>,
}

fn invalid_reply_context(reason: &str) -> anyhow::Error {
    RuntimeError::policy("agent_reply_context_invalid", reason)
        .with_recovery_hint(
            "select a runtime-provided message ID admitted in the current live turn",
        )
        .into()
}

fn batch_admits_reply_source(
    tx: &Transaction<'_>,
    turn: &TurnRecord,
    activation_id: &str,
    anchor_id: &str,
    message: &MessageEnvelope,
) -> Result<bool> {
    let idle_admitted = turn.idle_input_batch.as_ref().is_some_and(|batch| {
        batch.anchor_message_id == anchor_id
            && batch.items.iter().any(|item| item.message_id == anchor_id)
            && batch.items.iter().any(|item| {
                item.message_id == message.id
                    && item.source_ref == format!("message:{}", message.id)
            })
    });
    let live_admitted = turn.live_input_batches.iter().any(|batch| {
        batch.activation_id == activation_id
            && batch.items.iter().any(|item| {
                item.message_id == message.id
                    && item.source_ref == format!("message:{}", message.id)
            })
    });
    if !turn.input_message_ids.contains(&message.id) || !(idle_admitted || live_admitted) {
        return Ok(false);
    }
    let Some(ingress_turn_id) = message.turn_id.as_deref() else {
        return Ok(true);
    };
    if ingress_turn_id == turn.turn_id {
        return Ok(true);
    }
    // Enqueue assigns an ingress turn ID before execution. A live follower
    // still needs its real accepted delivery, not just a copied input ID.
    let delivery = tx
        .query_row(
            "SELECT payload_json FROM agent_message_deliveries WHERE message_id = ?1",
            [&message.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_delivery(&payload))
        .transpose()?;
    if delivery.is_none_or(|delivery| {
        delivery.target_agent_id != message.agent_id
            || delivery.message_id.as_deref() != Some(message.id.as_str())
            || delivery.outcome != AgentMessageDeliveryOutcome::Accepted
            || (live_admitted && delivery.accepted_at.is_none())
            || !((delivery.activation_id.is_none() && delivery.turn_id.is_none())
                || (live_admitted
                    && delivery.activation_id.as_deref() == Some(activation_id)
                    && delivery.turn_id.as_deref() == Some(turn.turn_id.as_str())))
            || delivery.routing_context != message.routing_context
    }) {
        return Ok(false);
    }
    let foreign_binding = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM turn_records WHERE turn_id = ?1)
             OR EXISTS(SELECT 1 FROM execution_protocol_attempts
                       WHERE json_extract(payload_json, '$.turn_id') = ?1)",
        [ingress_turn_id],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(!foreign_binding)
}

fn load_reply_routes_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    expected_turn_id: Option<&str>,
) -> Result<Option<ReplyRouteRegistry>> {
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
    let state = tx
        .query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<ReplyRouteExecutionState>(&payload))
        .transpose()?;
    let Some(state) = state else {
        return Ok(None);
    };
    let Some(binding) = state.current_execution_binding.as_ref() else {
        return Ok(None);
    };
    let turn_id = binding.turn_id.as_str();
    if attempt.agent_id != agent_id
        || attempt.state != ExecutionAttemptState::Open
        || attempt.terminal_outcome_id.is_some()
        || attempt.terminal_at.is_some()
        || state.current_turn_id.as_deref() != Some(turn_id)
        || binding.activation_id.as_deref() != Some(attempt.attempt_id.as_str())
        || attempt.source_message_id.as_deref() != Some(binding.source_message_id.as_str())
        || !matches!(
            &binding.admission_provenance,
            Some(crate::types::ExecutionAdmissionProvenance::Canonical { activation_id, .. })
                if activation_id == &attempt.attempt_id
        )
        || expected_turn_id.is_some_and(|expected| expected != turn_id)
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
    let turn: TurnRecord = serde_json::from_str(&payload)?;
    let owner_matches = match (&attempt.binding, turn.effective_owner()) {
        (
            ExecutionBinding::WorkItem { work_item_id },
            TurnOwner::WorkItem {
                work_item_id: owner,
            },
        ) => work_item_id == &owner,
        (
            ExecutionBinding::Conversation { interaction_id },
            TurnOwner::Conversation {
                interaction_id: owner,
            },
        ) => interaction_id == &owner,
        (
            ExecutionBinding::AgentLifecycle { agent_id },
            TurnOwner::AgentLifecycle { agent_id: owner },
        ) => agent_id == &owner,
        (ExecutionBinding::Command, TurnOwner::Command) => true,
        _ => false,
    };
    if turn.agent_id != agent_id
        || turn.turn_id != turn_id
        || turn.terminal.is_some()
        || !owner_matches
        || !turn.input_message_ids.contains(&binding.source_message_id)
        || binding
            .owner
            .as_ref()
            .is_some_and(|owner| *owner != turn.effective_owner())
    {
        return Ok(None);
    }
    let fences = crate::runtime_db::transitions::authority_fences_tx(tx, agent_id)?;
    // Ordinary state persistence also advances control_revision; canonical
    // authority validation checks Stopped/identity instead of treating it as revocation.
    if attempt.admitted_fences.host_registry_revision != fences.host_registry_revision {
        return Ok(None);
    }
    if let ExecutionBinding::WorkItem { work_item_id } = &attempt.binding {
        let payload = tx
            .query_row(
                "SELECT payload_json FROM execution_protocol_work_items
                 WHERE agent_id = ?1 AND work_item_id = ?2",
                params![agent_id, work_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let work: WorkItemExecutionRecord = serde_json::from_str(&payload)?;
        if !matches!(&work.state, WorkItemExecutionState::InFlight { attempt_id, .. } if attempt_id == &attempt.attempt_id)
        {
            return Ok(None);
        }
    }
    let replay_source =
        if let (Some(replay), Some(prior_id)) = (&turn.replay, &attempt.recovery_of_attempt_id) {
            let prior = tx
                .query_row(
                    "SELECT payload_json FROM execution_protocol_attempts
                 WHERE agent_id = ?1 AND attempt_id = ?2",
                    params![agent_id, prior_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|payload| serde_json::from_str::<ExecutionAttempt>(&payload))
                .transpose()?;
            if prior.as_ref().is_some_and(|prior| {
                prior.state == ExecutionAttemptState::Interrupted
                    && prior.agent_id == agent_id
                    && prior.source_message_id.as_deref() == Some(replay.source_message_id.as_str())
                    && attempt.source_message_id == prior.source_message_id
                    && prior.binding == attempt.binding
            }) {
                let anchor = tx
                    .query_row(
                        "SELECT payload_json FROM messages WHERE agent_id = ?1 AND message_id = ?2",
                        params![agent_id, binding.source_message_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .map(|payload| serde_json::from_str::<MessageEnvelope>(&payload))
                    .transpose()?;
                tx.query_row(
                    "SELECT payload_json FROM turn_records WHERE agent_id = ?1 AND turn_id = ?2",
                    params![agent_id, replay.source_turn_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|payload| serde_json::from_str::<TurnRecord>(&payload))
                .transpose()?
                .filter(|source| {
                    source.agent_id == agent_id
                        && source.turn_id == replay.source_turn_id
                        && prior.as_ref().is_some_and(|prior| {
                            match source.idle_input_batch.as_ref() {
                                Some(batch) => {
                                    // Replays retain the anchor's ingress stamp, not the live turn.
                                    // Only the predecessor's admitted snapshot proves attachment.
                                    (prior.turn_id.as_deref() == Some(source.turn_id.as_str())
                                        || prior.turn_id.is_none()
                                        || anchor.as_ref().is_some_and(|anchor| {
                                            anchor.agent_id == agent_id
                                                && anchor.id == binding.source_message_id
                                                && anchor.turn_id == prior.turn_id
                                        }))
                                        && batch.activation_id.as_deref()
                                            == Some(prior.attempt_id.as_str())
                                        && batch.anchor_message_id == binding.source_message_id
                                        && batch.items.first().map(|item| item.message_id.as_str())
                                            == Some(binding.source_message_id.as_str())
                                        && source
                                            .trigger
                                            .as_ref()
                                            .and_then(|trigger| trigger.message_id.as_deref())
                                            == Some(binding.source_message_id.as_str())
                                }
                                None if !source.live_input_batches.is_empty() => {
                                    (prior.turn_id.as_deref() == Some(source.turn_id.as_str())
                                        || prior.turn_id.is_none()
                                        || anchor.as_ref().is_some_and(|anchor| {
                                            anchor.agent_id == agent_id
                                                && anchor.id == binding.source_message_id
                                                && anchor.turn_id == prior.turn_id
                                        }))
                                        && source
                                            .live_input_batches
                                            .iter()
                                            .any(|batch| batch.activation_id == prior.attempt_id)
                                        && source
                                            .trigger
                                            .as_ref()
                                            .and_then(|trigger| trigger.message_id.as_deref())
                                            == Some(binding.source_message_id.as_str())
                                }
                                None => prior.turn_id.as_deref() == Some(source.turn_id.as_str()),
                            }
                        })
                        && source.effective_owner() == turn.effective_owner()
                        && source
                            .input_message_ids
                            .contains(&binding.source_message_id)
                })
            } else {
                None
            }
        } else {
            None
        };
    let mut routes = Vec::new();
    let mut latest_input_routed = false;
    for message_id in &turn.input_message_ids {
        let payload = tx
            .query_row(
                "SELECT payload_json FROM messages WHERE agent_id = ?1 AND message_id = ?2",
                params![agent_id, message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                invalid_reply_context("admitted input is missing its durable envelope")
            })?;
        let message: MessageEnvelope = serde_json::from_str(&payload)?;
        latest_input_routed = false;
        // Copied replay evidence uses its predecessor. New live admissions
        // after recovery are proved by the current canonical attachment.
        let current_batch_source = (turn.replay.is_none()
            && attempt.recovery_of_attempt_id.is_none())
            || replay_source.as_ref().is_some_and(|source| {
                !source.input_message_ids.contains(message_id)
                    && turn.live_input_batches.iter().any(|batch| {
                        batch.activation_id == attempt.attempt_id
                            && batch
                                .items
                                .iter()
                                .any(|item| &item.message_id == message_id)
                    })
            });
        let batch_source = (current_batch_source
            && batch_admits_reply_source(
                tx,
                &turn,
                &attempt.attempt_id,
                &binding.source_message_id,
                &message,
            )?)
            || match replay_source.as_ref() {
                Some(source) => batch_admits_reply_source(
                    tx,
                    source,
                    attempt
                        .recovery_of_attempt_id
                        .as_deref()
                        .unwrap_or_default(),
                    &binding.source_message_id,
                    &message,
                )?,
                None => false,
            };
        // Batched followers retain their ingress turn assignment. The existing
        // atomic queue claim + TurnRecord attachment proves admission instead.
        let attached_batch_source = batch_source
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries
                 WHERE agent_id = ?1 AND message_id = ?2 AND status = 'interjected')",
                params![agent_id, message_id],
                |row| row.get::<_, bool>(0),
            )?;
        // Canonical anchor admission does not require an ingress turn stamp.
        let canonical_anchor_source = message.id == binding.source_message_id
            && message.turn_id == attempt.turn_id
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_entries
                 WHERE agent_id = ?1 AND message_id = ?2 AND status = 'dequeued')",
                params![agent_id, message_id],
                |row| row.get::<_, bool>(0),
            )?;
        if message.id != *message_id
            || message.agent_id != agent_id
            || !(message.turn_id.as_deref() == Some(turn_id)
                || canonical_anchor_source
                || attached_batch_source
                || replay_source.as_ref().is_some_and(|source| {
                    source.agent_id == agent_id
                        && message.turn_id.as_deref() == Some(source.turn_id.as_str())
                        && source.input_message_ids.contains(message_id)
                }))
            || message.kind != MessageKind::InternalFollowup
            || message.admission_context != Some(AdmissionContext::RuntimeOwned)
            || message.delivery_surface != Some(MessageDeliverySurface::RuntimeSystem)
            || !matches!(
                message.origin,
                MessageOrigin::System { .. } | MessageOrigin::Task { .. }
            )
        {
            continue;
        }
        if !delivery_by_message_id_tx(tx, &message.id)?
            .is_some_and(|delivery| delivery.authenticates_agent_message(&message))
        {
            continue;
        }
        if let Some(route) = message.routing_context {
            if route.message_id == message.id
                && route.recipient_agent_id == agent_id
                && route.sender_agent_id.is_some()
            {
                latest_input_routed = true;
                if !routes.iter().any(|existing: &AgentMessageRoutingContext| {
                    existing.message_id == route.message_id
                }) {
                    routes.push(route);
                }
            }
        }
    }
    Ok(Some(ReplyRouteRegistry {
        activation_id: attempt.attempt_id,
        turn_id: turn_id.to_string(),
        routes,
        latest_input_routed,
    }))
}

fn select_reply_route(
    registry: Option<ReplyRouteRegistry>,
    selector: Option<&str>,
    target_agent_id: &str,
    forward: bool,
) -> Result<Option<ResolvedAgentMessageReplyContext>> {
    let Some(registry) = registry else {
        return if selector.is_some() || forward {
            Err(invalid_reply_context(
                "no live admitted agent-message route in this execution",
            ))
        } else {
            Ok(None)
        };
    };
    let selected = if let Some(selector) = selector {
        Some(
            registry
                .routes
                .iter()
                .find(|route| route.message_id == selector)
                .ok_or_else(|| {
                    invalid_reply_context("unknown, stale, or foreign reply_context_id")
                })?,
        )
    } else if registry.routes.len() > 1
        && (forward
            || registry.routes.iter().any(|route| {
                route.sender_agent_id.as_deref() == Some(target_agent_id)
                    || route.reply_to_agent_id.as_deref() == Some(target_agent_id)
            }))
    {
        let ids = registry
            .routes
            .iter()
            .map(|route| route.message_id.as_str())
            .collect::<Vec<_>>();
        return Err(RuntimeError::policy(
            "agent_reply_context_ambiguous",
            format!(
                "multiple admitted routes; set reply_context_id to one of: {}",
                ids.join(", ")
            ),
        )
        .with_recovery_hint(
            "choose the runtime-provided message ID of the request being answered or forwarded",
        )
        .into());
    } else {
        registry
            .routes
            .first()
            .filter(|_| registry.routes.len() == 1 && registry.latest_input_routed)
    };
    let selected = selected.filter(|route| {
        selector.is_some()
            || forward
            || route.sender_agent_id.as_deref() == Some(target_agent_id)
            || route.reply_to_agent_id.as_deref() == Some(target_agent_id)
    });
    if forward && selected.is_none() {
        return Err(invalid_reply_context(
            "forward requires a unique or explicit admitted source",
        ));
    }
    Ok(selected.map(|route| ResolvedAgentMessageReplyContext {
        activation_id: registry.activation_id.clone(),
        turn_id: registry.turn_id.clone(),
        routing_context: route.clone(),
    }))
}

pub(crate) enum AgentMessageDeliveryAdmissionDecision {
    Queue {
        record: AgentMessageDeliveryRecord,
        replace_existing: bool,
    },
    Complete {
        record: AgentMessageDeliveryRecord,
        insert: bool,
        replay: bool,
    },
}

impl AgentMessageDeliveryAdmissionDecision {
    pub(crate) fn receipt(&self) -> AgentMessageDeliveryReceipt {
        match self {
            Self::Queue { record, .. } => record.receipt(false),
            Self::Complete { record, replay, .. } => record.receipt(*replay),
        }
    }
}

impl AgentMessageDeliveryRepository<'_> {
    pub(crate) fn resolve_reply_context(
        &self,
        agent_id: &str,
        expected_turn_id: Option<&str>,
        selector: Option<&str>,
        target_agent_id: &str,
        forward: bool,
    ) -> Result<Option<ResolvedAgentMessageReplyContext>> {
        self.db.transaction(|tx| {
            select_reply_route(
                load_reply_routes_tx(tx, agent_id, expected_turn_id)?,
                selector,
                target_agent_id,
                forward,
            )
        })
    }

    pub(crate) fn rowid(&self, delivery_id: &str) -> Result<Option<i64>> {
        self.db.with_read(|connection| {
            connection
                .query_row(
                    "SELECT rowid
                     FROM agent_message_deliveries
                     WHERE delivery_id = ?1",
                    [delivery_id],
                    |row| row.get(0),
                )
                .optional()
                .context("reading agent message delivery rowid")
        })
    }

    pub(crate) fn first_accepted_from_sender_after(
        &self,
        target_agent_id: &str,
        sender_agent_id: &str,
        after_rowid: i64,
    ) -> Result<Option<AgentMessageDeliveryRecord>> {
        self.db.with_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT payload_json
                 FROM agent_message_deliveries
                 WHERE rowid > ?1
                   AND target_agent_id = ?2
                   AND outcome = 'accepted'
                 ORDER BY rowid ASC",
            )?;
            let rows = statement.query_map(params![after_rowid, target_agent_id], |row| {
                row.get::<_, String>(0)
            })?;
            for payload in rows {
                let record = decode_delivery(&payload?)?;
                if record.caller.caller_agent_id.as_deref() == Some(sender_agent_id) {
                    return Ok(Some(record));
                }
            }
            Ok(None)
        })
    }

    pub fn latest(&self, delivery_id: &str) -> Result<Option<AgentMessageDeliveryRecord>> {
        self.db
            .with_read(|connection| delivery_by_id(connection, delivery_id))
    }

    pub fn latest_for_message(
        &self,
        message_id: &str,
    ) -> Result<Option<AgentMessageDeliveryRecord>> {
        self.db.with_read(|connection| {
            connection
                .query_row(
                    "SELECT payload_json
                     FROM agent_message_deliveries
                     WHERE message_id = ?1",
                    [message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|payload| decode_delivery(&payload))
                .transpose()
        })
    }

    pub fn receipt(&self, delivery_id: &str) -> Result<Option<AgentMessageDeliveryReceipt>> {
        Ok(self
            .latest(delivery_id)?
            .map(|record| record.receipt(false)))
    }

    pub(crate) fn admit_without_queue(
        &self,
        candidate: &AgentMessageDeliveryRecord,
    ) -> Result<AgentMessageDeliveryReceipt> {
        self.db.transaction(|tx| {
            let decision = prepare_delivery_admission_tx(tx, candidate)?;
            match &decision {
                AgentMessageDeliveryAdmissionDecision::Complete { .. } => {
                    persist_completed_admission_tx(tx, &decision)?;
                    Ok(decision.receipt())
                }
                AgentMessageDeliveryAdmissionDecision::Queue { .. } => {
                    anyhow::bail!("active delivery admission requires a target queue transaction")
                }
            }
        })
    }
}

pub(crate) fn delivery_by_id(
    connection: &rusqlite::Connection,
    delivery_id: &str,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    connection
        .query_row(
            "SELECT payload_json
             FROM agent_message_deliveries
             WHERE delivery_id = ?1",
            [delivery_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| decode_delivery(&payload))
        .transpose()
}

pub(crate) fn delivery_by_idempotency_scope_tx(
    tx: &Transaction<'_>,
    idempotency_scope: &str,
    idempotency_key_digest: &str,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    tx.query_row(
        "SELECT payload_json
         FROM agent_message_deliveries
         WHERE idempotency_scope = ?1 AND idempotency_key_digest = ?2",
        params![idempotency_scope, idempotency_key_digest],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| decode_delivery(&payload))
    .transpose()
}

pub(crate) fn delivery_by_message_id_tx(
    tx: &Transaction<'_>,
    message_id: &str,
) -> Result<Option<AgentMessageDeliveryRecord>> {
    tx.query_row(
        "SELECT payload_json
         FROM agent_message_deliveries
         WHERE message_id = ?1",
        [message_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| decode_delivery(&payload))
    .transpose()
}

pub(crate) fn prepare_delivery_admission_tx(
    tx: &Transaction<'_>,
    candidate: &AgentMessageDeliveryRecord,
) -> Result<AgentMessageDeliveryAdmissionDecision> {
    // Check before idempotency replay, in the same transaction as queue admission.
    if let Some(expected_incarnation) = candidate.expected_incarnation {
        let identity = tx
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                [&candidate.target_agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| serde_json::from_str::<AgentIdentityRecord>(&payload))
            .transpose()?;
        let actual_incarnation = identity.map(|identity| identity.incarnation);
        if actual_incarnation != Some(expected_incarnation) {
            return Err(AgentMessageDeliveryError::StaleIncarnation {
                expected_incarnation,
                actual_incarnation,
            }
            .into());
        }
    }
    if let Some(activation_id) = candidate.caller.routing_activation_id.as_deref() {
        let caller_id = candidate
            .caller
            .caller_agent_id
            .as_deref()
            .ok_or_else(|| invalid_reply_context("selected route has no runtime caller"))?;
        let turn_id = candidate
            .caller
            .current_turn_id
            .as_deref()
            .ok_or_else(|| invalid_reply_context("selected route has no runtime turn"))?;
        let registry = load_reply_routes_tx(tx, caller_id, Some(turn_id))?
            .ok_or_else(|| invalid_reply_context("selected route execution is no longer live"))?;
        if registry.activation_id != activation_id
            || !registry
                .routes
                .iter()
                .any(|route| candidate.caller.inherited_routing_context.as_ref() == Some(route))
        {
            return Err(invalid_reply_context(
                "selected route activation or source changed before delivery",
            ));
        }
    }
    let existing = delivery_by_idempotency_scope_tx(
        tx,
        &candidate.idempotency_scope,
        &candidate.idempotency_key_digest,
    )?;
    if let Some(existing) = existing.as_ref() {
        if existing.request_digest != candidate.request_digest {
            return Err(AgentMessageDeliveryError::IdempotencyConflict {
                idempotency_scope: candidate.idempotency_scope.clone(),
                idempotency_key_digest: candidate.idempotency_key_digest.clone(),
            }
            .into());
        }
        if existing.outcome == AgentMessageDeliveryOutcome::Accepted || !existing.retryable {
            return Ok(AgentMessageDeliveryAdmissionDecision::Complete {
                record: existing.clone(),
                insert: false,
                replay: true,
            });
        }
    }

    let identity = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
            [&candidate.target_agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| {
            serde_json::from_str::<AgentIdentityRecord>(&payload)
                .context("decoding delivery target identity")
        })
        .transpose()?;
    let state = tx
        .query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [&candidate.target_agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| {
            serde_json::from_str::<AgentState>(&payload)
                .context("decoding delivery target agent state")
        })
        .transpose()?;
    let relations = canonical_relations_from_connection(tx, &candidate.target_agent_id)?;
    let policy = relations
        .as_ref()
        .and_then(|projection| projection.message_policy.as_ref());
    let principal_id = match candidate.caller.principal_kind {
        crate::types::AgentMessagePrincipalKind::SupervisingParent
        | crate::types::AgentMessagePrincipalKind::PeerAgent => candidate
            .caller
            .caller_agent_id
            .as_deref()
            .unwrap_or(&candidate.caller.caller_principal),
        crate::types::AgentMessagePrincipalKind::Operator
        | crate::types::AgentMessagePrincipalKind::ExternalIngress
        | crate::types::AgentMessagePrincipalKind::RuntimeCapability => {
            &candidate.caller.caller_principal
        }
    };
    let matched_rule = policy.and_then(|policy| {
        policy.rules.iter().enumerate().find(|(_, rule)| {
            rule.principal_kind == candidate.caller.principal_kind
                && rule
                    .principal_id
                    .as_deref()
                    .is_none_or(|expected| expected == principal_id)
                && rule
                    .route
                    .as_deref()
                    .is_none_or(|expected| expected == candidate.caller.route)
        })
    });
    let derived_grant = (matched_rule.is_none()
        && identity
            .as_ref()
            .is_some_and(|identity| identity.status == AgentRegistryStatus::Active)
        && candidate.caller.principal_kind == AgentMessagePrincipalKind::PeerAgent
        && matches!(
            candidate.caller.route.as_str(),
            "agent_invocation" | "agent_message"
        )
        && relations.as_ref().is_some_and(|relations| {
            relations
                .durability
                .as_ref()
                .is_some_and(|record| record.durability == AgentCanonicalDurability::Persistent)
                && relations
                    .lifecycle_attachment
                    .as_ref()
                    .is_some_and(|record| {
                        record.attachment == AgentLifecycleAttachment::Independent
                    })
        }))
    .then(|| match candidate.caller.route.as_str() {
        "agent_message" => AgentMessageDerivedGrant::PersistentIndependentPeerMessage,
        _ => AgentMessageDerivedGrant::PersistentIndependentPeerInvocation,
    });
    let allowed = matched_rule.map_or_else(
        || {
            derived_grant.is_some()
                || policy.is_some_and(|policy| policy.default_effect == AgentPolicyEffect::Allow)
        },
        |(_, rule)| rule.effect == AgentPolicyEffect::Allow,
    );
    let evidence = AgentMessageAdmissionEvidence {
        identity_revision: identity.as_ref().map(|identity| identity.revision),
        identity_status: identity.as_ref().map(|identity| match identity.status {
            AgentRegistryStatus::Active => AgentIdentityLifecycle::Active,
            AgentRegistryStatus::Deleting => AgentIdentityLifecycle::Deleting,
            AgentRegistryStatus::Deleted => AgentIdentityLifecycle::Deleted,
        }),
        runtime_status: state
            .as_ref()
            .map(|state| enum_string(&state.status))
            .transpose()?,
        message_policy_revision: policy.map(|policy| policy.revision),
        principal_kind: candidate.caller.principal_kind,
        principal_id: Some(principal_id.to_string()),
        route: candidate.caller.route.clone(),
        matched_rule_index: matched_rule.map(|(index, _)| index),
        derived_grant,
    };
    let rejection = match identity.as_ref().map(|identity| identity.status) {
        None => Some((
            AgentMessageDeliveryRejectionCode::TargetNotFound,
            "target agent identity was not found",
        )),
        Some(AgentRegistryStatus::Deleting) => Some((
            AgentMessageDeliveryRejectionCode::AgentDeleting,
            "target agent is deletion-fenced",
        )),
        Some(AgentRegistryStatus::Deleted) => Some((
            AgentMessageDeliveryRejectionCode::AgentDeleted,
            "target agent is deleted",
        )),
        Some(AgentRegistryStatus::Active)
            if state
                .as_ref()
                .is_some_and(|state| state.status == AgentStatus::Stopped) =>
        {
            Some((
                AgentMessageDeliveryRejectionCode::AgentStopped,
                "target agent is stopped",
            ))
        }
        Some(AgentRegistryStatus::Active) if !allowed => Some((
            AgentMessageDeliveryRejectionCode::MessageNotAuthorized,
            "caller is not authorized by the target message policy",
        )),
        Some(AgentRegistryStatus::Active) => None,
    };
    let now = Utc::now();
    let replace_existing = existing.is_some();
    let mut record = existing.unwrap_or_else(|| candidate.clone());
    record.admission_evidence = evidence;
    record.updated_at = now;
    record.state_version = record
        .state_version
        .saturating_add(u64::from(replace_existing));
    if let Some((code, diagnostic)) = rejection {
        record.message_id = None;
        record.outcome = AgentMessageDeliveryOutcome::Rejected;
        record.state = AgentMessageDeliveryState::Rejected;
        record.rejection_code = Some(code);
        record.retryable = code.retryable();
        record.diagnostic = Some(bounded_diagnostic(diagnostic));
        record.accepted_at = None;
        record.terminal_at = Some(now);
        return Ok(AgentMessageDeliveryAdmissionDecision::Complete {
            record,
            insert: !replace_existing,
            replay: replace_existing,
        });
    }

    record.message_id.clone_from(&candidate.message_id);
    record.correlation_id.clone_from(&candidate.correlation_id);
    record.causation_id.clone_from(&candidate.causation_id);
    record.outcome = AgentMessageDeliveryOutcome::Accepted;
    record.state = AgentMessageDeliveryState::Queued;
    record.rejection_code = None;
    record.retryable = false;
    record.diagnostic = None;
    record.accepted_at = Some(now);
    record.terminal_at = None;
    Ok(AgentMessageDeliveryAdmissionDecision::Queue {
        record,
        replace_existing,
    })
}

pub(crate) fn insert_delivery_tx(
    tx: &Transaction<'_>,
    record: &AgentMessageDeliveryRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    tx.execute(
        "INSERT INTO agent_message_deliveries (
           delivery_id, target_agent_id, message_id, idempotency_scope,
           idempotency_key_digest, request_digest, outcome, state, state_version,
           rejection_code, retryable, accepted_at, terminal_at, created_at,
           updated_at, payload_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
         )",
        params![
            record.delivery_id,
            record.target_agent_id,
            record.message_id,
            record.idempotency_scope,
            record.idempotency_key_digest,
            record.request_digest,
            enum_string(&record.outcome)?,
            enum_string(&record.state)?,
            i64::try_from(record.state_version).context("delivery state version exceeds SQLite")?,
            record
                .rejection_code
                .as_ref()
                .map(enum_string)
                .transpose()?,
            i64::from(record.retryable),
            record.accepted_at.map(timestamp),
            record.terminal_at.map(timestamp),
            timestamp(record.created_at),
            timestamp(record.updated_at),
            payload_json,
        ],
    )?;
    Ok(())
}

pub(crate) fn persist_completed_admission_tx(
    tx: &Transaction<'_>,
    decision: &AgentMessageDeliveryAdmissionDecision,
) -> Result<()> {
    let AgentMessageDeliveryAdmissionDecision::Complete { record, insert, .. } = decision else {
        return Ok(());
    };
    if *insert {
        insert_delivery_tx(tx, record)
    } else {
        replace_delivery_tx(tx, record)
    }
}

pub(crate) fn persist_queued_admission_tx(
    tx: &Transaction<'_>,
    decision: &AgentMessageDeliveryAdmissionDecision,
) -> Result<()> {
    let AgentMessageDeliveryAdmissionDecision::Queue {
        record,
        replace_existing,
    } = decision
    else {
        return Ok(());
    };
    if *replace_existing {
        replace_delivery_tx(tx, record)?;
    } else {
        insert_delivery_tx(tx, record)?;
    }
    bind_message_wait_tx(tx, record)
}

/// Acceptance must not outlive the parent-owned generation/cursor binding.
/// The target may be purged before this parent's monitor is started or recovered.
fn bind_message_wait_tx(tx: &Transaction<'_>, delivery: &AgentMessageDeliveryRecord) -> Result<()> {
    use crate::types::{MessageOrigin, TaskKind, TaskRecoverySpec, TaskTargetKind, TaskTargetRef};
    let (Some(task_id), Some(owner)) = (
        delivery.caller.current_task_id.as_deref(),
        delivery.caller.caller_agent_id.as_deref(),
    ) else {
        return Ok(());
    };
    if !matches!(&delivery.caller.origin, MessageOrigin::Task { task_id: origin } if origin == task_id)
    {
        return Ok(());
    }
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM tasks WHERE task_id = ?1 AND owner_agent_id = ?2",
            params![task_id, owner],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(());
    };
    let mut task = super::repositories::decode_task_payload(&payload)?;
    if task.kind != TaskKind::AgentMessageWait {
        return Ok(());
    }
    anyhow::ensure!(
        super::repositories::is_active_task_status(&task.status),
        "message wait is no longer active"
    );
    let identity: String = tx.query_row(
        "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
        [&delivery.target_agent_id],
        |row| row.get(0),
    )?;
    let identity: AgentIdentityRecord = serde_json::from_str(&identity)?;
    let cursor: i64 = tx.query_row(
        "SELECT rowid FROM agent_message_deliveries WHERE delivery_id = ?1",
        [&delivery.delivery_id],
        |row| row.get(0),
    )?;
    let detail = task.detail.get_or_insert_with(|| serde_json::json!({}));
    let detail = detail
        .as_object_mut()
        .context("invalid message wait detail")?;
    detail.insert(
        "target_agent_id".into(),
        serde_json::json!(delivery.target_agent_id),
    );
    detail.insert(
        "target".into(),
        serde_json::to_value(TaskTargetRef {
            kind: TaskTargetKind::AgentMessageWait,
            agent_id: delivery.target_agent_id.clone(),
            incarnation: Some(identity.incarnation),
        })?,
    );
    detail.insert(
        "request_delivery_id".into(),
        serde_json::json!(delivery.delivery_id),
    );
    detail.insert(
        "message_wait_after_delivery_rowid".into(),
        serde_json::json!(cursor),
    );
    if let Some(TaskRecoverySpec::AgentMessageWait {
        delivery_id,
        after_delivery_rowid,
        ..
    }) = task.recovery.as_mut()
    {
        *delivery_id = Some(delivery.delivery_id.clone());
        *after_delivery_rowid = Some(cursor);
    }
    task.updated_at = std::cmp::max(task.updated_at, delivery.updated_at);
    super::repositories::upsert_task_tx(tx, &task)?;
    Ok(())
}

fn replace_delivery_tx(tx: &Transaction<'_>, record: &AgentMessageDeliveryRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)?;
    let changed = tx.execute(
        "UPDATE agent_message_deliveries
         SET message_id = ?1, outcome = ?2, state = ?3, state_version = ?4,
             rejection_code = ?5, retryable = ?6, accepted_at = ?7,
             terminal_at = ?8, updated_at = ?9, payload_json = ?10
         WHERE delivery_id = ?11",
        params![
            record.message_id,
            enum_string(&record.outcome)?,
            enum_string(&record.state)?,
            i64::try_from(record.state_version).context("delivery state version exceeds SQLite")?,
            record
                .rejection_code
                .as_ref()
                .map(enum_string)
                .transpose()?,
            i64::from(record.retryable),
            record.accepted_at.map(timestamp),
            record.terminal_at.map(timestamp),
            timestamp(record.updated_at),
            payload_json,
            record.delivery_id,
        ],
    )?;
    anyhow::ensure!(changed == 1, "delivery {} not found", record.delivery_id);
    Ok(())
}

pub(crate) fn compare_and_set_delivery_state_tx(
    tx: &Transaction<'_>,
    delivery_id: &str,
    expected_state: AgentMessageDeliveryState,
    next: &AgentMessageDeliveryRecord,
) -> Result<bool> {
    let payload_json = serde_json::to_string(next)?;
    let changed = tx.execute(
        "UPDATE agent_message_deliveries
         SET state = ?1, state_version = ?2, terminal_at = ?3,
             updated_at = ?4, payload_json = ?5
         WHERE delivery_id = ?6 AND state = ?7 AND state_version = ?8",
        params![
            enum_string(&next.state)?,
            i64::try_from(next.state_version).context("delivery state version exceeds SQLite")?,
            next.terminal_at.map(timestamp),
            timestamp(next.updated_at),
            payload_json,
            delivery_id,
            enum_string(&expected_state)?,
            i64::try_from(next.state_version.saturating_sub(1))
                .context("delivery state version exceeds SQLite")?,
        ],
    )?;
    Ok(changed == 1)
}

pub(crate) fn advance_delivery_state_for_message_tx(
    tx: &Transaction<'_>,
    message_id: &str,
    next_state: AgentMessageDeliveryState,
    diagnostic: Option<&str>,
) -> Result<bool> {
    let Some(mut record) = delivery_by_message_id_tx(tx, message_id)? else {
        return Ok(false);
    };
    let valid = matches!(
        (record.state, next_state),
        (
            AgentMessageDeliveryState::Queued,
            AgentMessageDeliveryState::Dispatched
                | AgentMessageDeliveryState::Consumed
                | AgentMessageDeliveryState::Failed
                | AgentMessageDeliveryState::CancelledByDeletion
        ) | (
            AgentMessageDeliveryState::Dispatched,
            AgentMessageDeliveryState::Consumed
                | AgentMessageDeliveryState::Failed
                | AgentMessageDeliveryState::CancelledByDeletion
        )
    );
    if !valid {
        return Ok(false);
    }

    let expected_state = record.state;
    let now = Utc::now();
    record.state = next_state;
    record.state_version = record.state_version.saturating_add(1);
    record.updated_at = now;
    record.terminal_at = next_state.is_terminal().then_some(now);
    if let Some(diagnostic) = diagnostic {
        record.diagnostic = Some(bounded_diagnostic(diagnostic));
    }
    compare_and_set_delivery_state_tx(tx, &record.delivery_id, expected_state, &record)
}

pub(crate) fn bind_delivery_execution_for_message_tx(
    tx: &Transaction<'_>,
    message_id: &str,
    attempt: &crate::domain::execution_protocol::ExecutionAttempt,
) -> Result<bool> {
    let Some(mut record) = delivery_by_message_id_tx(tx, message_id)? else {
        return Ok(false);
    };
    if !matches!(
        record.state,
        AgentMessageDeliveryState::Queued | AgentMessageDeliveryState::Dispatched
    ) {
        return Ok(false);
    }
    anyhow::ensure!(
        attempt.source_message_id.as_deref() == Some(message_id),
        "delivery execution attempt does not reference its message"
    );
    if let Some(existing) = record.activation_id.as_deref() {
        anyhow::ensure!(
            existing == attempt.attempt_id
                || attempt.recovery_of_attempt_id.as_deref() == Some(existing),
            "delivery execution attempt conflicts with its canonical activation"
        );
    }
    if let (Some(existing), Some(turn_id)) = (record.turn_id.as_deref(), attempt.turn_id.as_deref())
    {
        anyhow::ensure!(
            existing == turn_id || attempt.recovery_of_attempt_id.is_some(),
            "delivery execution turn conflicts with its canonical activation"
        );
    }

    let changed = record.activation_id.as_deref() != Some(attempt.attempt_id.as_str())
        || record.turn_id != attempt.turn_id;
    if !changed {
        return Ok(false);
    }
    record.activation_id = Some(attempt.attempt_id.clone());
    record.turn_id.clone_from(&attempt.turn_id);
    record.updated_at = Utc::now();
    replace_delivery_tx(tx, &record)?;
    Ok(true)
}

pub(crate) fn cancel_active_deliveries_for_target_tx(
    tx: &Transaction<'_>,
    target_agent_id: &str,
) -> Result<usize> {
    let queued = enum_string(&AgentMessageDeliveryState::Queued)?;
    let dispatched = enum_string(&AgentMessageDeliveryState::Dispatched)?;
    let records = {
        let mut statement = tx.prepare(
            "SELECT payload_json
             FROM agent_message_deliveries
             WHERE target_agent_id = ?1 AND state IN (?2, ?3)
             ORDER BY created_at ASC, delivery_id ASC",
        )?;
        let rows = statement.query_map(params![target_agent_id, queued, dispatched], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|payload| decode_delivery(&payload))
            .collect::<Result<Vec<_>>>()?
    };

    let mut cancelled = 0;
    for record in records {
        cancelled += usize::from(advance_delivery_state_for_message_tx(
            tx,
            record
                .message_id
                .as_deref()
                .context("accepted delivery is missing its message id")?,
            AgentMessageDeliveryState::CancelledByDeletion,
            Some("target agent entered the deletion fence"),
        )?);
    }
    Ok(cancelled)
}

fn decode_delivery(payload: &str) -> Result<AgentMessageDeliveryRecord> {
    serde_json::from_str(payload).context("decoding agent message delivery payload")
}

fn enum_string<T: serde::Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .context("delivery enum serialized to a non-string value")?
        .to_string())
}

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn bounded_diagnostic(value: &str) -> String {
    value.chars().take(DELIVERY_DIAGNOSTIC_LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::AgentMessageDeliveryService,
        runtime_db::{
            transitions::{QueueMutation, QueueOperation, QueueTransitionCommand},
            RuntimeDb,
        },
        types::{
            AdmissionContext, AgentCanonicalDurability, AgentDurabilityRecord, AgentKind,
            AgentLifecycleAttachment, AgentLifecycleAttachmentRecord, AgentMessageCallerContext,
            AgentMessageDeliveryError, AgentMessageDeliveryOutcome,
            AgentMessageDeliveryRejectionCode, AgentMessageDerivedGrant, AgentMessagePolicyRecord,
            AgentMessagePolicyRule, AgentMessagePrincipalKind, AgentMessageRoutingContext,
            AgentMessageSendRequest, AgentOwnership, AgentPolicyEffect, AgentProfilePreset,
            AgentState, AgentStatus, AgentVisibility, AuthorityClass, MessageBody,
            MessageDeliverySurface, MessageEnvelope, MessageOrigin, Priority, QueueEntryRecord,
            QueueEntryStatus,
        },
    };
    use tempfile::TempDir;

    fn runtime_db() -> Result<(TempDir, RuntimeDb)> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        Ok((dir, db))
    }

    fn seed_target(db: &RuntimeDb, status: AgentStatus) -> Result<()> {
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            "target-agent",
            AgentKind::Named,
            AgentVisibility::Private,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        let mut state = AgentState::new("target-agent");
        state.status = status;
        db.agent_states().upsert(&state)
    }

    fn caller() -> AgentMessageCallerContext {
        AgentMessageCallerContext {
            caller_principal: "runtime:agent-invocation".into(),
            caller_agent_id: Some("caller-agent".into()),
            principal_kind: AgentMessagePrincipalKind::RuntimeCapability,
            route: "agent_invocation".into(),
            origin: MessageOrigin::Task {
                task_id: "task-delivery".into(),
            },
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: Some("turn-delivery".into()),
            current_task_id: Some("task-delivery".into()),
            current_work_item_id: Some("work-delivery".into()),
            inherited_routing_context: None,
            routing_activation_id: None,
        }
    }

    fn peer_caller() -> AgentMessageCallerContext {
        AgentMessageCallerContext {
            caller_principal: "agent:caller-agent".into(),
            caller_agent_id: Some("caller-agent".into()),
            principal_kind: AgentMessagePrincipalKind::PeerAgent,
            route: "agent_invocation".into(),
            origin: MessageOrigin::Task {
                task_id: "task-peer-delivery".into(),
            },
            authority_class: AuthorityClass::ExternalEvidence,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: Some("turn-peer-delivery".into()),
            current_task_id: Some("task-peer-delivery".into()),
            current_work_item_id: Some("work-peer-delivery".into()),
            inherited_routing_context: None,
            routing_activation_id: None,
        }
    }

    fn request(key: &str, text: &str) -> AgentMessageSendRequest {
        AgentMessageSendRequest {
            target_agent_id: "target-agent".into(),
            content: MessageBody::Text { text: text.into() },
            client_idempotency_key: key.into(),
            correlation_id: Some("correlation-delivery".into()),
            causation_id: Some("message-parent".into()),
            requested_priority: Some(Priority::Normal),
            forward: false,
            reply_context_id: None,
        }
    }

    fn routed_caller() -> AgentMessageCallerContext {
        let mut caller = caller();
        caller.inherited_routing_context = Some(AgentMessageRoutingContext {
            message_id: "incoming-message".into(),
            sender_agent_id: Some("origin-agent".into()),
            recipient_agent_id: "caller-agent".into(),
            reply_to_agent_id: Some("origin-agent".into()),
            correlation_id: Some("correlation-incoming".into()),
            in_reply_to_message_id: None,
            original_sender_agent_id: None,
            original_reply_to_agent_id: None,
        });
        caller
    }

    fn prepare(key: &str, text: &str) -> Result<crate::runtime::PreparedAgentMessageDelivery> {
        AgentMessageDeliveryService::prepare(request(key, text), caller())
    }

    fn prepare_from_peer(
        key: &str,
        text: &str,
    ) -> Result<crate::runtime::PreparedAgentMessageDelivery> {
        AgentMessageDeliveryService::prepare(request(key, text), peer_caller())
    }

    fn accepted_reply_delivery(
        message: &MessageEnvelope,
        sender: &str,
        key: &str,
    ) -> Result<AgentMessageDeliveryRecord> {
        let mut source = peer_caller();
        source.caller_principal = format!("agent:{sender}");
        source.caller_agent_id = Some(sender.into());
        source.origin = message.origin.clone();
        source.authority_class = message.authority_class;
        let mut input = request(key, "reply source");
        input.target_agent_id = message.agent_id.clone();
        let mut delivery = AgentMessageDeliveryService::prepare(input, source)?.record;
        delivery.message_id = Some(message.id.clone());
        delivery.routing_context = message.routing_context.clone();
        delivery.outcome = AgentMessageDeliveryOutcome::Accepted;
        delivery.state = AgentMessageDeliveryState::Queued;
        delivery.accepted_at = Some(Utc::now());
        Ok(delivery)
    }

    fn seed_reply_binding(
        db: &RuntimeDb,
        turn_id: &str,
        activation_id: &str,
        source_id: &str,
    ) -> Result<()> {
        let mut state = db.agent_states().latest("caller-agent")?.unwrap();
        state.status = AgentStatus::AwakeRunning;
        state.current_turn_id = Some(turn_id.into());
        state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
            activation_id: Some(activation_id.into()),
            admission_provenance: Some(crate::types::ExecutionAdmissionProvenance::Canonical {
                scenario_class:
                    crate::domain::scheduler::SchedulerScenarioClass::WorkItemAutonomousContinuation,
                activation_id: activation_id.into(),
            }),
            source_message_id: source_id.into(),
            turn_id: turn_id.into(),
            owner: Some(TurnOwner::AgentLifecycle {
                agent_id: "caller-agent".into(),
            }),
            work_item_id: None,
            claimed_work_revision: None,
        });
        db.agent_states().upsert(&state)
    }

    fn seed_reply_turn(
        db: &RuntimeDb,
        senders: &[&str],
    ) -> Result<(TurnRecord, Vec<MessageEnvelope>)> {
        seed_reply_turn_with_trigger(db, senders, 0)
    }

    fn seed_reply_turn_with_trigger(
        db: &RuntimeDb,
        senders: &[&str],
        trigger_index: usize,
    ) -> Result<(TurnRecord, Vec<MessageEnvelope>)> {
        seed_reply_turn_with_config(db, senders, trigger_index, |_| {})
    }

    fn seed_reply_turn_with_config(
        db: &RuntimeDb,
        senders: &[&str],
        trigger_index: usize,
        configure: impl FnOnce(&mut TurnRecord),
    ) -> Result<(TurnRecord, Vec<MessageEnvelope>)> {
        use crate::domain::execution_protocol::{
            AdmittedFences, ExecutionOrigin, ExecutionPriority, ExecutionProtocolState,
            ExecutionProvenance, ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
        };
        db.agent_identities().upsert(&AgentIdentityRecord::new(
            "caller-agent",
            AgentKind::Named,
            AgentVisibility::Private,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ))?;
        db.agent_states().upsert(&AgentState::new("caller-agent"))?;
        let mut inputs = Vec::new();
        for (index, sender) in senders.iter().enumerate() {
            let mut message = MessageEnvelope::new(
                "caller-agent",
                MessageKind::InternalFollowup,
                MessageOrigin::Task {
                    task_id: format!("invoke-{index}"),
                },
                AuthorityClass::ExternalEvidence,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("request {index}"),
                },
            )
            .with_admission(
                MessageDeliverySurface::RuntimeSystem,
                AdmissionContext::RuntimeOwned,
            );
            message.turn_id = Some("routing-turn".into());
            message.routing_context = Some(AgentMessageRoutingContext {
                message_id: message.id.clone(),
                sender_agent_id: Some((*sender).into()),
                recipient_agent_id: message.agent_id.clone(),
                reply_to_agent_id: Some((*sender).into()),
                correlation_id: Some(format!("correlation-{index}")),
                in_reply_to_message_id: None,
                original_sender_agent_id: None,
                original_reply_to_agent_id: None,
            });
            let delivery = accepted_reply_delivery(&message, sender, &format!("source-{index}"))?;
            db.transaction(|tx| insert_delivery_tx(tx, &delivery))?;
            inputs.push(db.messages().append_with_index_changes(&message, &[])?);
        }
        let mut turn = TurnRecord::new("caller-agent", "routing-turn", 1);
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
            &inputs[trigger_index],
        ));
        turn.input_message_ids = inputs.iter().map(|message| message.id.clone()).collect();
        configure(&mut turn);
        db.turn_records().upsert(&turn)?;
        let fences = db
            .transitions()
            .load_execution_authority_fences("caller-agent")?;
        let source = &inputs[0];
        let revision = source.message_seq.unwrap();
        let attempt = ExecutionAttempt {
            input_projection: None,
            attempt_id: "routing-activation".into(),
            agent_id: "caller-agent".into(),
            source_message_id: Some(source.id.clone()),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::InternalFollowup {
                    message_id: source.id.clone(),
                },
                generation: revision,
            },
            binding: ExecutionBinding::AgentLifecycle {
                agent_id: "caller-agent".into(),
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::Task,
                trust: ExecutionTrust::ExternalEvidence,
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: revision,
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: fences.agent_control_revision,
                host_registry_revision: fences.host_registry_revision,
            },
            state: ExecutionAttemptState::Open,
            run_id: None,
            // Production admission precedes the live turn binding.
            turn_id: None,
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: Utc::now().to_rfc3339(),
            terminal_at: None,
        };
        let state = ExecutionProtocolState {
            agent_id: "caller-agent".into(),
            attempts: [(attempt.attempt_id.clone(), attempt)].into(),
            work_items: Default::default(),
            outcomes: Default::default(),
        };
        db.transaction(|tx| crate::runtime_db::transitions::persist_state_tx(tx, &state))?;
        seed_reply_binding(db, &turn.turn_id, "routing-activation", &inputs[0].id)?;
        Ok((turn, inputs))
    }

    fn prepare_reply(
        db: &RuntimeDb,
        target: &str,
        selector: Option<&str>,
        forward: bool,
        key: &str,
    ) -> Result<crate::runtime::PreparedAgentMessageDelivery> {
        prepare_reply_in_turn(db, "routing-turn", target, selector, forward, key)
    }

    fn prepare_reply_in_turn(
        db: &RuntimeDb,
        turn_id: &str,
        target: &str,
        selector: Option<&str>,
        forward: bool,
        key: &str,
    ) -> Result<crate::runtime::PreparedAgentMessageDelivery> {
        let context = db.agent_message_deliveries().resolve_reply_context(
            "caller-agent",
            Some(turn_id),
            selector,
            target,
            forward,
        )?;
        let mut caller = peer_caller();
        caller.route = "agent_message".into();
        caller.current_turn_id = Some(turn_id.into());
        caller.current_task_id = None;
        caller.current_work_item_id = None;
        caller.inherited_routing_context = context
            .as_ref()
            .map(|context| context.routing_context.clone());
        caller.routing_activation_id = context.map(|context| context.activation_id);
        let mut request = request(key, "answer");
        request.target_agent_id = target.into();
        request.reply_context_id = selector.map(str::to_string);
        request.forward = forward;
        request.correlation_id = None;
        request.causation_id = None;
        AgentMessageDeliveryService::prepare(request, caller)
    }

    #[test]
    fn reply_selector_disambiguates_senders_and_correlations_without_inheriting_direct_sends(
    ) -> Result<()> {
        for senders in [["sender-a", "sender-b"], ["sender-a", "sender-a"]] {
            let (_dir, db) = runtime_db()?;
            let (_, inputs) = seed_reply_turn(&db, &senders)?;
            for forward in [false, true] {
                let error = prepare_reply(&db, "sender-a", None, forward, "ambiguous")
                    .err()
                    .expect("multiple routes must reject implicit selection");
                assert_eq!(
                    crate::runtime_error::describe_runtime_error(&error).code,
                    "agent_reply_context_ambiguous"
                );
                for input in &inputs {
                    assert!(error.to_string().contains(&input.id));
                }
            }
            for (index, input) in inputs.iter().enumerate() {
                let prepared = prepare_reply(&db, senders[index], Some(&input.id), false, "reply")?;
                let route = prepared.message.routing_context.unwrap();
                assert_eq!(
                    route.in_reply_to_message_id.as_deref(),
                    Some(input.id.as_str())
                );
                assert_eq!(route.correlation_id, Some(format!("correlation-{index}")));
                assert_eq!(route.reply_to_agent_id.as_deref(), Some("caller-agent"));
                assert_eq!(
                    prepared.message.authority_class,
                    AuthorityClass::ExternalEvidence
                );
                assert_eq!(
                    prepared.message.causation_id.as_deref(),
                    Some(input.id.as_str())
                );
            }
            for selector in [None, Some(inputs[0].id.as_str())] {
                let prepared = prepare_reply(&db, "unrelated-agent", selector, false, "direct")?;
                let route = prepared.message.routing_context.unwrap();
                assert_eq!(route.in_reply_to_message_id, None);
                assert_eq!(route.correlation_id, None);
                assert_eq!(route.original_sender_agent_id, None);
            }
            let prepared =
                prepare_reply(&db, "relay-recipient", Some(&inputs[1].id), true, "relay")?;
            let route = prepared.message.routing_context.unwrap();
            assert_eq!(route.sender_agent_id.as_deref(), Some("caller-agent"));
            assert_eq!(route.reply_to_agent_id.as_deref(), Some(senders[1]));
            assert_eq!(route.original_sender_agent_id.as_deref(), Some(senders[1]));
            assert_eq!(
                route.original_reply_to_agent_id.as_deref(),
                Some(senders[1])
            );
        }
        Ok(())
    }

    #[test]
    fn reply_selector_single_route_compatibility_and_unrouted_inputs() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let (mut turn, mut inputs) = seed_reply_turn(&db, &["sender-a"])?;
        let id = inputs[0].id.clone();
        let implicit = prepare_reply(&db, "sender-a", None, false, "implicit")?;
        let explicit = prepare_reply(&db, "sender-a", Some(&id), false, "explicit")?;
        assert_eq!(
            implicit
                .message
                .routing_context
                .as_ref()
                .unwrap()
                .in_reply_to_message_id,
            explicit
                .message
                .routing_context
                .as_ref()
                .unwrap()
                .in_reply_to_message_id
        );
        assert!(prepare_reply(&db, "other", None, true, "unique-forward").is_ok());
        let mut newer = inputs[0].clone();
        newer.id = "unrouted-task-result".into();
        newer.kind = MessageKind::TaskResult;
        newer.message_seq = None;
        // A task-result payload cannot mint an agent-message route, even with route-shaped metadata.
        newer.routing_context.as_mut().unwrap().message_id = newer.id.clone();
        let newer = db.messages().append_with_index_changes(&newer, &[])?;
        turn.input_message_ids.push(newer.id.clone());
        db.turn_records().upsert(&turn)?;
        assert!(prepare_reply(&db, "sender-a", None, false, "no-implicit")?
            .message
            .routing_context
            .unwrap()
            .in_reply_to_message_id
            .is_none());
        assert!(prepare_reply(&db, "sender-a", Some(&id), false, "explicit-older").is_ok());
        assert!(prepare_reply(&db, "other", None, true, "no-forward").is_err());
        assert!(prepare_reply(&db, "sender-a", Some(&newer.id), false, "not-agent").is_err());
        inputs[0].routing_context.as_mut().unwrap().message_id = "spoofed".into();
        db.messages().upsert_many(&inputs)?;
        assert!(prepare_reply(&db, "sender-a", Some(&id), false, "invalid-route").is_err());
        Ok(())
    }

    #[test]
    fn reply_selector_requires_matching_accepted_agent_delivery() -> Result<()> {
        for defect in [
            "missing_delivery",
            "rejected",
            "not_accepted",
            "message_id",
            "target",
            "route",
            "sender",
            "principal",
            "missing_agent",
            "empty_agent",
            "operator_principal",
            "capability_principal",
            "external_principal",
            "authority",
            "origin",
            "surface",
            "admission",
            "operator_prompt",
            "task_result",
            "operator_origin",
        ] {
            let (_dir, db) = runtime_db()?;
            seed_target(&db, AgentStatus::AwakeIdle)?;
            let (_, mut inputs) = seed_reply_turn(&db, &["target-agent"])?;
            let input = &mut inputs[0];
            input.authority_class = AuthorityClass::OperatorInstruction;
            let mut delivery = db
                .agent_message_deliveries()
                .latest_for_message(&input.id)?
                .unwrap();
            delivery.caller.authority_class = input.authority_class;
            db.messages().upsert_many(&inputs)?;
            db.transaction(|tx| replace_delivery_tx(tx, &delivery))?;
            let id = inputs[0].id.clone();
            let prepared = prepare_reply(&db, "target-agent", Some(&id), false, "reply")?;
            assert!(delivery.authenticates_agent_message(&inputs[0]), "{defect}");

            match defect {
                "missing_delivery" => {}
                "rejected" => delivery.outcome = AgentMessageDeliveryOutcome::Rejected,
                "not_accepted" => delivery.accepted_at = None,
                "message_id" => delivery.message_id = Some("other-message".into()),
                "target" => delivery.target_agent_id = "other-agent".into(),
                "route" => {
                    delivery.routing_context.as_mut().unwrap().correlation_id =
                        Some("other-correlation".into());
                }
                "sender" => delivery.caller.caller_agent_id = Some("other-sender".into()),
                "principal" => delivery.caller.caller_principal = "agent:other-sender".into(),
                "missing_agent" => delivery.caller.caller_agent_id = None,
                "empty_agent" => delivery.caller.caller_agent_id = Some(String::new()),
                "operator_principal" => {
                    delivery.caller.principal_kind = AgentMessagePrincipalKind::Operator;
                }
                "capability_principal" => {
                    delivery.caller.principal_kind = AgentMessagePrincipalKind::RuntimeCapability;
                }
                "external_principal" => {
                    delivery.caller.principal_kind = AgentMessagePrincipalKind::ExternalIngress;
                }
                "authority" => delivery.caller.authority_class = AuthorityClass::ExternalEvidence,
                "origin" => {
                    delivery.caller.origin = MessageOrigin::System {
                        subsystem: "other-origin".into(),
                    };
                }
                "surface" => {
                    delivery.caller.delivery_surface = MessageDeliverySurface::HttpControlPrompt;
                }
                "admission" => {
                    delivery.caller.admission_context = AdmissionContext::ControlAuthenticated;
                }
                "operator_prompt" => inputs[0].kind = MessageKind::OperatorPrompt,
                "task_result" => inputs[0].kind = MessageKind::TaskResult,
                "operator_origin" => {
                    let origin = MessageOrigin::Operator {
                        actor_id: None,
                        actor_display_name: None,
                    };
                    inputs[0].origin = origin.clone();
                    delivery.caller.origin = origin;
                }
                _ => unreachable!(),
            }
            db.messages().upsert_many(&inputs)?;
            db.transaction(|tx| {
                if defect == "missing_delivery" {
                    tx.execute(
                        "DELETE FROM agent_message_deliveries WHERE delivery_id = ?1",
                        [&delivery.delivery_id],
                    )?;
                    Ok(())
                } else {
                    replace_delivery_tx(tx, &delivery)
                }
            })?;
            assert!(
                prepare_reply(&db, "target-agent", Some(&id), false, "invalid").is_err(),
                "{defect}"
            );
            let error = db
                .transitions()
                .commit_delivery_admission(&admission_command(&prepared), &prepared.record)
                .err()
                .with_context(|| format!("{defect} must fail transaction revalidation"))?;
            assert!(
                error.to_string().contains("selected route"),
                "{defect}: {error}"
            );
            assert!(db
                .agent_message_deliveries()
                .latest(&prepared.record.delivery_id)?
                .is_none());
        }
        Ok(())
    }

    #[test]
    fn reply_selector_rejects_unknown_foreign_and_unadmitted_sources() -> Result<()> {
        for defect in [
            "unknown",
            "not_admitted",
            "foreign_agent",
            "foreign_turn",
            "recipient",
            "surface",
            "admission",
            "origin",
            "expected_turn",
        ] {
            let (_dir, db) = runtime_db()?;
            let (mut turn, inputs) = seed_reply_turn(&db, &["sender-a"])?;
            let mut candidate = inputs[0].clone();
            candidate.id = format!("selector-proof-{defect}");
            candidate.message_seq = None;
            let route = candidate.routing_context.as_mut().unwrap();
            route.message_id = candidate.id.clone();
            route.sender_agent_id = Some("sender-b".into());
            route.reply_to_agent_id = Some("sender-b".into());
            let delivery = accepted_reply_delivery(&candidate, "sender-b", defect)?;
            db.transaction(|tx| insert_delivery_tx(tx, &delivery))?;
            let route = candidate.routing_context.as_mut().unwrap();
            match defect {
                "foreign_agent" => {
                    candidate.agent_id = "foreign-agent".into();
                    candidate.turn_id = Some("foreign-turn".into());
                    route.recipient_agent_id = "foreign-agent".into();
                }
                "foreign_turn" => candidate.turn_id = Some("foreign-turn".into()),
                "recipient" => route.recipient_agent_id = "other".into(),
                "surface" => candidate.delivery_surface = None,
                "admission" => candidate.admission_context = None,
                "origin" => {
                    candidate.origin = MessageOrigin::Operator {
                        actor_id: None,
                        actor_display_name: None,
                    }
                }
                "unknown" | "not_admitted" | "expected_turn" => {}
                _ => unreachable!(),
            }
            let candidate = db.messages().append_with_index_changes(&candidate, &[])?;
            if defect != "not_admitted" && defect != "foreign_agent" {
                turn.input_message_ids.push(candidate.id.clone());
                db.turn_records().upsert(&turn)?;
            }
            let result = db.agent_message_deliveries().resolve_reply_context(
                "caller-agent",
                Some(if defect == "expected_turn" {
                    "other-turn"
                } else {
                    "routing-turn"
                }),
                Some(if defect == "unknown" {
                    "unknown"
                } else {
                    &candidate.id
                }),
                "sender-b",
                false,
            );
            assert!(result.is_err(), "{defect}");
        }
        Ok(())
    }

    #[test]
    fn reply_selector_recovers_from_durable_inputs_and_revalidates_terminal_at_delivery(
    ) -> Result<()> {
        let (dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let (mut turn, inputs) = seed_reply_turn(&db, &["target-agent", "target-agent"])?;
        db.agent_canonical_relations()
            .upsert_message_policy(&AgentMessagePolicyRecord {
                agent_id: "target-agent".into(),
                revision: 1,
                default_effect: AgentPolicyEffect::Allow,
                rules: Vec::new(),
                created_at: Utc::now(),
            })?;
        let selected_id = inputs[0].id.clone();
        let prepared = prepare_reply(
            &db,
            "target-agent",
            Some(&selected_id),
            false,
            "restart-reply",
        )?;
        drop(db);
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        let recovered = prepare_reply(
            &db,
            "target-agent",
            Some(&selected_id),
            false,
            "restart-reply",
        )?;
        assert_eq!(
            prepared.record.request_digest,
            recovered.record.request_digest
        );
        let committed = db
            .transitions()
            .commit_delivery_admission(&admission_command(&recovered), &recovered.record)?;
        assert_eq!(
            committed.delivery_receipt.unwrap().outcome,
            AgentMessageDeliveryOutcome::Accepted
        );
        let pending = prepare_reply(
            &db,
            "target-agent",
            Some(&selected_id),
            false,
            "terminal-reply",
        )?;
        turn.terminal = Some(crate::types::TurnTerminalSummary {
            kind: crate::types::TurnTerminalKind::Completed,
            reason: None,
            no_brief_reason: None,
            completed_at: Utc::now(),
            duration_ms: 1,
        });
        db.turn_records().upsert(&turn)?;
        assert!(prepare_reply(
            &db,
            "target-agent",
            Some(&selected_id),
            false,
            "terminal-selector"
        )
        .is_err());
        assert!(db
            .transitions()
            .commit_delivery_admission(&admission_command(&pending), &pending.record)
            .is_err());
        assert!(db
            .agent_message_deliveries()
            .latest(&pending.record.delivery_id)?
            .is_none());
        // Even idempotent replays cannot reuse a now-terminal selection.
        assert!(db
            .transitions()
            .commit_delivery_admission(&admission_command(&recovered), &recovered.record)
            .is_err());
        Ok(())
    }

    fn admission_command(
        prepared: &crate::runtime::PreparedAgentMessageDelivery,
    ) -> QueueTransitionCommand {
        let now = Utc::now();
        QueueTransitionCommand {
            agent_id: prepared.message.agent_id.clone(),
            operation: QueueOperation::Admit,
            mutation: QueueMutation::Upsert(QueueEntryRecord {
                message_id: prepared.message.id.clone(),
                agent_id: prepared.message.agent_id.clone(),
                priority: prepared.message.priority.clone(),
                status: QueueEntryStatus::Queued,
                created_at: now,
                updated_at: now,
            }),
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: vec![prepared.message.clone()],
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: true,
            fault: None,
            brief_evidence: Vec::new(),
        }
    }

    fn queue_transition(
        db: &RuntimeDb,
        current: &QueueEntryRecord,
        operation: QueueOperation,
        status: QueueEntryStatus,
    ) -> Result<()> {
        let mut next = current.clone();
        next.status = status;
        next.updated_at = Utc::now();
        let mutation = match operation {
            QueueOperation::Claim | QueueOperation::Interject => QueueMutation::Consume(next),
            QueueOperation::Settle | QueueOperation::RepairDrop | QueueOperation::Discard => {
                QueueMutation::Upsert(next)
            }
            QueueOperation::Admit | QueueOperation::Requeue => {
                unreachable!("test helper only advances admitted deliveries")
            }
        };
        db.transitions().commit_queue(&QueueTransitionCommand {
            agent_id: current.agent_id.clone(),
            operation,
            mutation,
            scheduler_claim_work_item: None,
            agent_state: None,
            message_evidence: Vec::new(),
            transcript_entries: Vec::new(),
            turn_record: None,
            audit_events: Vec::new(),
            notify_scheduler: false,
            fault: None,
            brief_evidence: Vec::new(),
        })?;
        Ok(())
    }

    fn recovery_transition(
        db: &RuntimeDb,
        current: &QueueEntryRecord,
        status: QueueEntryStatus,
    ) -> Result<()> {
        let mut next = current.clone();
        next.status = status;
        next.updated_at = Utc::now();
        db.transitions().commit_scheduler_recovery(
            &QueueTransitionCommand {
                agent_id: current.agent_id.clone(),
                operation: QueueOperation::Settle,
                mutation: QueueMutation::Upsert(next),
                scheduler_claim_work_item: None,
                agent_state: None,
                message_evidence: Vec::new(),
                transcript_entries: Vec::new(),
                turn_record: None,
                audit_events: Vec::new(),
                notify_scheduler: false,
                fault: None,
                brief_evidence: Vec::new(),
            },
            &crate::runtime_db::transitions::ExecutionProtocolTransition::default(),
        )?;
        Ok(())
    }

    #[test]
    fn delivery_incarnation_fence_rechecks_identity_before_queue_and_replay() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let mut identity = db.agent_identities().latest("target-agent")?.unwrap();
        let mut prepared = prepare("incarnation-key", "hello")?;
        prepared.record.expected_incarnation = Some(identity.incarnation);
        db.transitions()
            .commit_delivery_admission(&admission_command(&prepared), &prepared.record)?;

        // Simulate replacement after preparation, including an accepted keyed retry.
        identity.incarnation += 1;
        identity.revision += 1;
        db.agent_identities().upsert(&identity)?;
        for key in ["incarnation-key", "new-incarnation-key"] {
            let mut candidate = prepare(key, "hello")?;
            candidate.record.expected_incarnation = prepared.record.expected_incarnation;
            let error = db
                .transitions()
                .commit_delivery_admission(&admission_command(&candidate), &candidate.record)
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<AgentMessageDeliveryError>(),
                Some(AgentMessageDeliveryError::StaleIncarnation { .. })
            ));
            assert!(db
                .agent_message_deliveries()
                .latest_for_message(&candidate.message.id)?
                .is_none());
        }
        Ok(())
    }

    #[test]
    fn delivery_incarnation_and_reply_route_fences_compose() -> Result<()> {
        for defect in ["none", "target_incarnation", "reply_source"] {
            let (_dir, db) = runtime_db()?;
            seed_target(&db, AgentStatus::AwakeIdle)?;
            let (_, mut inputs) = seed_reply_turn(&db, &["target-agent"])?;
            let mut identity = db.agent_identities().latest("target-agent")?.unwrap();
            let mut prepared =
                prepare_reply(&db, "target-agent", Some(&inputs[0].id), false, "fenced")?;
            prepared.record.expected_incarnation = Some(identity.incarnation);
            match defect {
                "target_incarnation" => {
                    identity.incarnation += 1;
                    identity.revision += 1;
                    db.agent_identities().upsert(&identity)?;
                }
                "reply_source" => {
                    inputs[0].routing_context.as_mut().unwrap().correlation_id =
                        Some("changed".into());
                    db.messages().upsert_many(&inputs)?;
                }
                _ => {}
            }
            let result = db
                .transitions()
                .commit_delivery_admission(&admission_command(&prepared), &prepared.record);
            if defect == "none" {
                assert_eq!(
                    result?.delivery_receipt.unwrap().outcome,
                    AgentMessageDeliveryOutcome::Accepted
                );
            } else {
                let error = result.unwrap_err();
                if defect == "target_incarnation" {
                    assert!(matches!(
                        error.downcast_ref::<AgentMessageDeliveryError>(),
                        Some(AgentMessageDeliveryError::StaleIncarnation { .. })
                    ));
                }
                assert!(db
                    .agent_message_deliveries()
                    .latest(&prepared.record.delivery_id)?
                    .is_none());
                assert!(db.queue_entries().latest(&prepared.message.id)?.is_none());
            }
        }
        Ok(())
    }

    #[test]
    fn reply_selector_revalidates_identity_owner_and_activation_fences() -> Result<()> {
        use crate::domain::execution_protocol::{interrupt_execution, InterruptExecution};
        for defect in ["stop", "identity", "owner", "activation", "source"] {
            let (_dir, db) = runtime_db()?;
            let (_, mut inputs) = seed_reply_turn(&db, &["target-agent"])?;
            let id = inputs[0].id.clone();
            let prepared = prepare_reply(&db, "target-agent", Some(&id), false, "stale")?;
            let direct = prepare_reply(&db, "unrelated-agent", Some(&id), false, "stale-direct")?;
            match defect {
                "stop" => {
                    let mut state = db.agent_states().latest("caller-agent")?.unwrap();
                    state.status = AgentStatus::Stopped;
                    db.agent_states().upsert(&state)?;
                }
                "identity" => {
                    let mut identity = db.agent_identities().latest("caller-agent")?.unwrap();
                    identity.revision += 1;
                    db.agent_identities().upsert(&identity)?;
                }
                "owner" => {
                    let mut state = db.agent_states().latest("caller-agent")?.unwrap();
                    state.current_execution_binding.as_mut().unwrap().owner =
                        Some(TurnOwner::WorkItem {
                            work_item_id: "other-work".into(),
                        });
                    db.agent_states().upsert(&state)?;
                }
                "source" => {
                    inputs[0].routing_context.as_mut().unwrap().correlation_id =
                        Some("changed".into());
                    db.messages().upsert_many(&inputs)?;
                }
                "activation" => {
                    let state = db
                        .transitions()
                        .load_execution_protocol_state_if_initialized("caller-agent")?
                        .unwrap();
                    let mut next = interrupt_execution(
                        &state,
                        &InterruptExecution {
                            attempt_id: "routing-activation".into(),
                            outcome_id: "interrupted-route".into(),
                            reason: "replace activation".into(),
                            interrupted_at: Utc::now().to_rfc3339(),
                        },
                    )
                    .map_err(anyhow::Error::msg)?
                    .state;
                    db.transaction(|tx| {
                        crate::runtime_db::transitions::persist_state_tx(tx, &next)
                    })?;
                    let mut replacement = state.attempts["routing-activation"].clone();
                    replacement.attempt_id = "replacement-activation".into();
                    replacement.recovery_of_attempt_id = Some("routing-activation".into());
                    next.attempts
                        .insert(replacement.attempt_id.clone(), replacement);
                    db.transaction(|tx| {
                        crate::runtime_db::transitions::persist_state_tx(tx, &next)
                    })?;
                    seed_reply_binding(&db, "routing-turn", "replacement-activation", &id)?;
                }
                _ => unreachable!(),
            }
            for candidate in [&prepared, &direct] {
                assert!(db.transitions().commit_delivery_admission(
                    &admission_command(candidate), &candidate.record,
                ).is_err(), "{defect}");
                assert!(db
                    .agent_message_deliveries()
                    .latest(&candidate.record.delivery_id)?
                    .is_none());
            }
        }
        Ok(())
    }

    #[test]
    fn reply_selector_interrupted_replay_requires_canonical_and_readmission_evidence() -> Result<()>
    {
        use crate::domain::execution_protocol::{interrupt_execution, InterruptExecution};
        for defect in [
            "none",
            "bound_prior",
            "unbound_prior_without_batch",
            "wrong_prior_turn",
            "wrong_batch_anchor",
            "wrong_source_trigger",
            "no_recovery",
            "no_replay",
            "not_readmitted",
            "wrong_source_turn",
            "source_turn_from_other_attempt",
        ] {
            let (_dir, db) = runtime_db()?;
            let (mut source_turn, inputs) = seed_reply_turn_with_trigger(
                &db,
                &["sender-a", "sender-b"],
                usize::from(defect == "wrong_source_trigger"),
            )?;
            let mut state = db
                .transitions()
                .load_execution_protocol_state_if_initialized("caller-agent")?
                .unwrap();
            assert!(state.attempts["routing-activation"].turn_id.is_none());
            if defect != "unbound_prior_without_batch" {
                let source_activation = if defect == "source_turn_from_other_attempt" {
                    "other-activation"
                } else {
                    "routing-activation"
                };
                source_turn.idle_input_batch = Some(crate::types::IdleInputBatchRecord {
                    anchor_message_id: if defect == "wrong_batch_anchor" {
                        inputs[1].id.clone()
                    } else {
                        inputs[0].id.clone()
                    },
                    activation_id: Some(source_activation.into()),
                    sender_agent_id: "sender-a".into(),
                    cutoff: Utc::now(),
                    max_items: 8,
                    max_bytes: 24 * 1024,
                    rendered_bytes: 2,
                    items: inputs
                        .iter()
                        .map(|message| crate::types::IdleInputBatchItem {
                            message_id: message.id.clone(),
                            source_ref: format!("message:{}", message.id),
                            rendered: "input".into(),
                        })
                        .collect(),
                });
                db.turn_records().upsert(&source_turn)?;
            }
            if matches!(defect, "bound_prior" | "wrong_prior_turn") {
                state
                    .attempts
                    .get_mut("routing-activation")
                    .unwrap()
                    .turn_id = Some(if defect == "wrong_prior_turn" {
                    "other-turn".into()
                } else {
                    source_turn.turn_id.clone()
                });
            }
            let mut recovered = interrupt_execution(
                &state,
                &InterruptExecution {
                    attempt_id: "routing-activation".into(),
                    outcome_id: "interrupted-route".into(),
                    reason: "restart".into(),
                    interrupted_at: Utc::now().to_rfc3339(),
                },
            )
            .map_err(anyhow::Error::msg)?
            .state;
            if !matches!(defect, "bound_prior" | "wrong_prior_turn") {
                assert!(recovered.attempts["routing-activation"].turn_id.is_none());
            }
            if defect == "source_turn_from_other_attempt" {
                let mut other = state.attempts["routing-activation"].clone();
                other.attempt_id = "other-activation".into();
                other.turn_id = Some(source_turn.turn_id.clone());
                recovered.attempts.insert(other.attempt_id.clone(), other);
                recovered = interrupt_execution(
                    &recovered,
                    &InterruptExecution {
                        attempt_id: "other-activation".into(),
                        outcome_id: "other-interruption".into(),
                        reason: "restart".into(),
                        interrupted_at: Utc::now().to_rfc3339(),
                    },
                )
                .map_err(anyhow::Error::msg)?
                .state;
            }
            db.transaction(|tx| crate::runtime_db::transitions::persist_state_tx(tx, &recovered))?;
            let mut attempt = state.attempts["routing-activation"].clone();
            attempt.attempt_id = "recovered-activation".into();
            assert_eq!(
                attempt.turn_id.is_none(),
                !matches!(defect, "bound_prior" | "wrong_prior_turn")
            );
            attempt.recovery_of_attempt_id =
                (defect != "no_recovery").then(|| "routing-activation".into());
            recovered
                .attempts
                .insert(attempt.attempt_id.clone(), attempt);
            db.transaction(|tx| crate::runtime_db::transitions::persist_state_tx(tx, &recovered))?;
            let mut replay = TurnRecord::new("caller-agent", "recovered-turn", 2);
            replay.input_message_ids = source_turn.input_message_ids.clone();
            if defect == "not_readmitted" {
                replay.input_message_ids.pop();
            }
            if defect != "no_replay" {
                replay.replay = Some(crate::types::TurnReplayProvenance {
                    source_message_id: inputs[0].id.clone(),
                    source_turn_id: if defect == "wrong_source_turn" {
                        "foreign-turn".into()
                    } else {
                        source_turn.turn_id
                    },
                    reason: "interrupted_queue_claim_reentry".into(),
                    prior_terminal: None,
                });
            }
            db.turn_records().upsert(&replay)?;
            seed_reply_binding(&db, "recovered-turn", "recovered-activation", &inputs[0].id)?;
            let result = db.agent_message_deliveries().resolve_reply_context(
                "caller-agent",
                Some("recovered-turn"),
                Some(&inputs[1].id),
                "sender-b",
                false,
            );
            if matches!(defect, "none" | "bound_prior") {
                let selected = result?.context("recovered route")?;
                assert_eq!(selected.activation_id, "recovered-activation");
                assert_eq!(
                    selected.routing_context.correlation_id.as_deref(),
                    Some("correlation-1")
                );
            } else {
                assert!(result.is_err(), "{defect}");
            }
        }
        Ok(())
    }

    #[test]
    fn reply_selector_retains_ingress_routes_across_two_interrupted_replays() -> Result<()> {
        use crate::domain::execution_protocol::{
            admit_execution, interrupt_execution, AdmitExecution, InterruptExecution,
        };
        use crate::types::{
            IdleInputBatchItem, IdleInputBatchRecord, LiveInputBatchRecord, TurnReplayProvenance,
        };

        for defect in [
            "none",
            "wrong_prior_turn",
            "wrong_batch_activation",
            "wrong_batch_anchor",
            "wrong_batch_first_item",
            "wrong_source_trigger",
            "wrong_source_ref",
            "missing_source_batch",
            "no_recovery",
            "no_replay",
            "wrong_source_turn",
            "not_readmitted",
            "missing_claim",
            "foreign_bound_turn",
            "foreign_bound_attempt",
            "live_none",
            "live_new",
            "live_wrong_activation",
            "live_wrong_ref",
            "live_closed_predecessor",
            "live_missing_predecessor",
        ] {
            let (_dir, db) = runtime_db()?;
            let (mut source_turn, mut inputs) = seed_reply_turn(&db, &["sender-a"])?;
            for index in 0..2 {
                let mut input = request(&format!("replay-input-{index}"), "batched input");
                input.target_agent_id = "caller-agent".into();
                input.correlation_id = Some(format!("replay-correlation-{index}"));
                let mut sender_context = peer_caller();
                sender_context.caller_principal = "agent:sender-a".into();
                sender_context.caller_agent_id = Some("sender-a".into());
                sender_context.origin = inputs[0].origin.clone();
                let mut prepared = AgentMessageDeliveryService::prepare(input, sender_context)?;
                prepared.message.turn_id = Some(format!("follower-ingress-{index}"));
                let admitted = db
                    .transitions()
                    .commit_delivery_admission(&admission_command(&prepared), &prepared.record)?;
                assert_eq!(
                    admitted.delivery_receipt.unwrap().outcome,
                    AgentMessageDeliveryOutcome::Accepted
                );
                inputs.push(
                    db.messages()
                        .by_id(Some("caller-agent"), &prepared.message.id)?
                        .unwrap(),
                );
            }
            source_turn.input_message_ids = inputs.iter().map(|input| input.id.clone()).collect();
            source_turn.idle_input_batch = Some(IdleInputBatchRecord {
                anchor_message_id: inputs[0].id.clone(),
                activation_id: Some("routing-activation".into()),
                sender_agent_id: "sender-a".into(),
                cutoff: Utc::now(),
                max_items: 8,
                max_bytes: 24 * 1024,
                rendered_bytes: 3,
                items: inputs
                    .iter()
                    .map(|input| IdleInputBatchItem {
                        message_id: input.id.clone(),
                        source_ref: format!("message:{}", input.id),
                        rendered: "input".into(),
                    })
                    .collect(),
            });
            if defect.starts_with("live_") {
                let idle = source_turn.idle_input_batch.take().unwrap();
                source_turn.live_input_batches.push(LiveInputBatchRecord {
                    safe_point: crate::types::LiveInputSafePoint::new(
                        idle.activation_id.as_ref().unwrap().clone(),
                        1,
                    ),
                    activation_id: idle.activation_id.unwrap(),
                    cutoff: idle.cutoff,
                    max_items: idle.max_items,
                    max_bytes: idle.max_bytes,
                    rendered_bytes: idle.rendered_bytes,
                    result_identities: Vec::new(),
                    ordinary_message_ids: idle
                        .items
                        .iter()
                        .map(|item| item.message_id.clone())
                        .collect(),
                    items: idle.items,
                });
            }
            for input in &inputs[1..] {
                let mut claimed = db.queue_entries().latest(&input.id)?.unwrap();
                claimed.status = QueueEntryStatus::Interjected;
                db.transitions().commit_queue(&QueueTransitionCommand {
                    agent_id: "caller-agent".into(),
                    operation: QueueOperation::Interject,
                    mutation: QueueMutation::Consume(claimed),
                    scheduler_claim_work_item: None,
                    agent_state: None,
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: Some(source_turn.clone()),
                    audit_events: Vec::new(),
                    notify_scheduler: false,
                    fault: None,
                    brief_evidence: Vec::new(),
                })?;
            }
            let mut execution = db
                .transitions()
                .load_execution_protocol_state_if_initialized("caller-agent")?
                .unwrap();
            let mut template = execution.attempts.remove("routing-activation").unwrap();
            // Scheduler admission copies the durable anchor's ingress stamp.
            template.turn_id = inputs[0].turn_id.clone();
            execution = admit_execution(
                &execution,
                &AdmitExecution {
                    attempt: template.clone(),
                },
            )
            .map_err(anyhow::Error::msg)?
            .state;
            for replay_index in 1..=2 {
                let prior_id = source_turn
                    .idle_input_batch
                    .as_ref()
                    .and_then(|batch| batch.activation_id.clone())
                    .or_else(|| {
                        source_turn
                            .live_input_batches
                            .first()
                            .map(|batch| batch.activation_id.clone())
                    })
                    .unwrap();
                execution = interrupt_execution(
                    &execution,
                    &InterruptExecution {
                        attempt_id: prior_id.clone(),
                        outcome_id: format!("interrupted-route-{replay_index}"),
                        reason: "restart".into(),
                        interrupted_at: Utc::now().to_rfc3339(),
                    },
                )
                .map_err(anyhow::Error::msg)?
                .state;
                // Restart persists interruption before scheduler readmission.
                db.transaction(|tx| {
                    crate::runtime_db::transitions::persist_state_tx(tx, &execution)
                })?;
                let mut attempt = template.clone();
                attempt.attempt_id = format!("recovered-activation-{replay_index}");
                attempt.recovery_of_attempt_id = Some(prior_id.clone());
                execution = admit_execution(
                    &execution,
                    &AdmitExecution {
                        attempt: attempt.clone(),
                    },
                )
                .map_err(anyhow::Error::msg)?
                .state;
                assert_eq!(execution.attempts[&prior_id].turn_id, inputs[0].turn_id);
                assert_eq!(attempt.turn_id, inputs[0].turn_id);
                if replay_index == 2 {
                    assert_ne!(
                        execution.attempts[&prior_id].turn_id.as_deref(),
                        Some(source_turn.turn_id.as_str())
                    );
                }
                let mut replay = TurnRecord::new(
                    "caller-agent",
                    format!("recovered-turn-{replay_index}"),
                    replay_index + 1,
                );
                replay.trigger = source_turn.trigger.clone();
                // A turn's trigger is immutable after its first persistence.
                if replay_index == 1 && defect == "wrong_source_trigger" {
                    replay.trigger =
                        Some(crate::types::TurnTriggerSummary::from_message(&inputs[1]));
                }
                replay.input_message_ids = source_turn.input_message_ids.clone();
                replay.idle_input_batch = source_turn.idle_input_batch.clone();
                if let Some(batch) = replay.idle_input_batch.as_mut() {
                    batch.activation_id = Some(attempt.attempt_id.clone());
                }
                replay.live_input_batches = source_turn.live_input_batches.clone();
                for batch in &mut replay.live_input_batches {
                    batch.activation_id = attempt.attempt_id.clone();
                }
                replay.replay = Some(TurnReplayProvenance {
                    source_message_id: inputs[0].id.clone(),
                    source_turn_id: source_turn.turn_id.clone(),
                    reason: "interrupted_queue_claim_reentry".into(),
                    prior_terminal: None,
                });
                if defect == "live_new" && replay_index == 1 {
                    let mut input = request("new-after-recovery", "new live input");
                    input.target_agent_id = "caller-agent".into();
                    input.correlation_id = Some("replay-correlation-2".into());
                    let mut sender = peer_caller();
                    sender.caller_principal = "agent:sender-a".into();
                    sender.caller_agent_id = Some("sender-a".into());
                    sender.origin = inputs[0].origin.clone();
                    let mut prepared = AgentMessageDeliveryService::prepare(input, sender)?;
                    prepared.message.turn_id = Some("new-after-recovery-ingress".into());
                    let admitted = db.transitions().commit_delivery_admission(
                        &admission_command(&prepared),
                        &prepared.record,
                    )?;
                    assert_eq!(
                        admitted.delivery_receipt.unwrap().outcome,
                        AgentMessageDeliveryOutcome::Accepted
                    );
                    let input = db
                        .messages()
                        .by_id(Some("caller-agent"), &prepared.message.id)?
                        .unwrap();
                    replay.input_message_ids.push(input.id.clone());
                    replay.live_input_batches.push(LiveInputBatchRecord {
                        activation_id: attempt.attempt_id.clone(),
                        safe_point: crate::types::LiveInputSafePoint::new(
                            attempt.attempt_id.clone(),
                            2,
                        ),
                        cutoff: Utc::now(),
                        max_items: 8,
                        max_bytes: 24 * 1024,
                        rendered_bytes: 5,
                        result_identities: Vec::new(),
                        ordinary_message_ids: vec![input.id.clone()],
                        items: vec![IdleInputBatchItem {
                            message_id: input.id.clone(),
                            source_ref: format!("message:{}", input.id),
                            rendered: "input".into(),
                        }],
                    });
                    let mut queue = db.queue_entries().latest(&input.id)?.unwrap();
                    queue.status = QueueEntryStatus::Interjected;
                    db.transitions().commit_queue(&QueueTransitionCommand {
                        agent_id: "caller-agent".into(),
                        operation: QueueOperation::Interject,
                        mutation: QueueMutation::Consume(queue),
                        scheduler_claim_work_item: None,
                        agent_state: None,
                        message_evidence: Vec::new(),
                        transcript_entries: Vec::new(),
                        turn_record: Some(replay.clone()),
                        audit_events: Vec::new(),
                        notify_scheduler: false,
                        fault: None,
                        brief_evidence: Vec::new(),
                    })?;
                    inputs.push(input);
                }
                if replay_index == 2 {
                    match defect {
                        "live_wrong_activation" => {
                            source_turn.live_input_batches[0].activation_id =
                                "foreign-activation".into();
                        }
                        "live_wrong_ref" => {
                            source_turn.live_input_batches[0].items[2].source_ref =
                                "message:foreign".into();
                        }
                        "live_closed_predecessor" => {
                            execution.attempts.get_mut(&prior_id).unwrap().state =
                                ExecutionAttemptState::Settled;
                        }
                        "wrong_prior_turn" => {
                            execution.attempts.get_mut(&prior_id).unwrap().turn_id =
                                Some("foreign-turn".into());
                        }
                        "wrong_batch_activation" => {
                            source_turn.idle_input_batch.as_mut().unwrap().activation_id =
                                Some("foreign-activation".into());
                        }
                        "wrong_batch_anchor" => {
                            source_turn
                                .idle_input_batch
                                .as_mut()
                                .unwrap()
                                .anchor_message_id = inputs[1].id.clone();
                        }
                        "wrong_batch_first_item" => {
                            source_turn
                                .idle_input_batch
                                .as_mut()
                                .unwrap()
                                .items
                                .swap(0, 1);
                        }
                        "wrong_source_ref" => {
                            source_turn.idle_input_batch.as_mut().unwrap().items[2].source_ref =
                                "message:foreign".into();
                        }
                        "missing_source_batch" => source_turn.idle_input_batch = None,
                        "no_recovery" => {
                            execution
                                .attempts
                                .get_mut(&attempt.attempt_id)
                                .unwrap()
                                .recovery_of_attempt_id = None;
                        }
                        "no_replay" => replay.replay = None,
                        "wrong_source_turn" => {
                            replay.replay.as_mut().unwrap().source_turn_id = "routing-turn".into();
                        }
                        "not_readmitted" => {
                            replay.input_message_ids.pop();
                        }
                        "missing_claim" => {
                            let mut queue = db.queue_entries().latest(&inputs[2].id)?.unwrap();
                            queue.status = QueueEntryStatus::Queued;
                            db.queue_entries().upsert(&queue)?;
                        }
                        "foreign_bound_turn" => {
                            db.turn_records().upsert(&TurnRecord::new(
                                "caller-agent",
                                inputs[2].turn_id.as_deref().unwrap(),
                                4,
                            ))?;
                        }
                        "foreign_bound_attempt" => {
                            let mut foreign = template.clone();
                            foreign.attempt_id = "foreign-activation".into();
                            foreign.turn_id = inputs[2].turn_id.clone();
                            let foreign_state = admit_execution(
                                &crate::domain::execution_protocol::ExecutionProtocolState::empty(
                                    "caller-agent",
                                ),
                                &AdmitExecution { attempt: foreign },
                            )
                            .map_err(anyhow::Error::msg)?
                            .state;
                            let foreign_state = interrupt_execution(
                                &foreign_state,
                                &InterruptExecution {
                                    attempt_id: "foreign-activation".into(),
                                    outcome_id: "foreign-interruption".into(),
                                    reason: "restart".into(),
                                    interrupted_at: Utc::now().to_rfc3339(),
                                },
                            )
                            .map_err(anyhow::Error::msg)?
                            .state;
                            execution.attempts.extend(foreign_state.attempts);
                            execution.outcomes.extend(foreign_state.outcomes);
                        }
                        _ => {}
                    }
                }
                db.transaction(|tx| {
                    crate::runtime_db::transitions::persist_state_tx(tx, &execution)?;
                    if replay_index == 2 && defect == "live_missing_predecessor" {
                        // State persistence upserts; explicitly remove both reciprocal records.
                        assert_eq!(
                            tx.execute(
                                "DELETE FROM execution_protocol_outcomes
                                 WHERE agent_id = ?1 AND attempt_id = ?2",
                                params!["caller-agent", prior_id],
                            )?,
                            1
                        );
                        assert_eq!(
                            tx.execute(
                                "DELETE FROM execution_protocol_attempts
                                 WHERE agent_id = ?1 AND attempt_id = ?2",
                                params!["caller-agent", prior_id],
                            )?,
                            1
                        );
                    }
                    Ok(())
                })?;
                db.turn_records().upsert(&source_turn)?;
                db.turn_records().upsert(&replay)?;
                seed_reply_binding(&db, &replay.turn_id, &attempt.attempt_id, &inputs[0].id)?;
                for (index, input) in inputs[1..].iter().enumerate() {
                    let route = prepare_reply_in_turn(
                        &db,
                        &replay.turn_id,
                        "sender-a",
                        Some(&input.id),
                        false,
                        "recovered-reply",
                    );
                    if replay_index == 2 && !matches!(defect, "none" | "live_none" | "live_new") {
                        if index == 1 {
                            let error = route.err().expect(defect);
                            assert_eq!(
                                crate::runtime_error::describe_runtime_error(&error).code,
                                "agent_reply_context_invalid",
                                "{defect}"
                            );
                        }
                        continue;
                    }
                    let prepared = route?;
                    assert_eq!(
                        prepared.record.caller.routing_activation_id,
                        Some(attempt.attempt_id.clone())
                    );
                    let route = prepared.message.routing_context.unwrap();
                    assert_eq!(
                        route.in_reply_to_message_id.as_deref(),
                        Some(input.id.as_str())
                    );
                    assert_eq!(
                        route.correlation_id,
                        Some(format!("replay-correlation-{index}"))
                    );
                    assert_eq!(route.reply_to_agent_id.as_deref(), Some("caller-agent"));
                    assert_eq!(
                        prepared.message.causation_id.as_deref(),
                        Some(input.id.as_str())
                    );
                }
                source_turn = replay;
            }
        }
        Ok(())
    }

    #[test]
    fn reply_selector_survives_benign_control_state_persistence() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        let (_, inputs) = seed_reply_turn(&db, &["sender-a"])?;
        let id = inputs[0].id.clone();
        let mut state = db.agent_states().latest("caller-agent")?.unwrap();
        state.pending += 1;
        db.agent_states().upsert(&state)?;
        assert!(prepare_reply(&db, "sender-a", Some(&id), false, "after-persist").is_ok());
        Ok(())
    }

    #[test]
    fn reply_selector_uses_atomic_batch_attachment_not_foreign_ingress_turn_ids() -> Result<()> {
        use crate::domain::execution_protocol::{interrupt_execution, InterruptExecution};
        use crate::types::{IdleInputBatchItem, IdleInputBatchRecord};
        for defect in [
            "none",
            "current_turn",
            "accepted_unbound_ingress",
            "missing_claim",
            "wrong_anchor",
            "wrong_ref",
            "foreign_turn",
            "foreign_bound_turn",
            "foreign_bound_attempt",
        ] {
            let (_dir, db) = runtime_db()?;
            let (mut turn, inputs) = seed_reply_turn(&db, &["sender-a"])?;
            let mut follower = inputs[0].clone();
            follower.id = format!("batched-source-{defect}");
            follower.turn_id = match defect {
                "current_turn" => Some(turn.turn_id.clone()),
                "foreign_turn"
                | "accepted_unbound_ingress"
                | "foreign_bound_turn"
                | "foreign_bound_attempt" => Some("foreign-turn".into()),
                _ => None,
            };
            follower.message_seq = None;
            let route = follower.routing_context.as_mut().unwrap();
            route.message_id = follower.id.clone();
            route.correlation_id = Some("batched-correlation".into());
            let follower = db.messages().append_with_index_changes(&follower, &[])?;
            if defect != "foreign_turn" {
                let delivery = accepted_reply_delivery(&follower, "sender-a", defect)?;
                db.transaction(|tx| insert_delivery_tx(tx, &delivery))?;
            }
            if defect == "foreign_bound_turn" {
                db.turn_records()
                    .upsert(&TurnRecord::new("caller-agent", "foreign-turn", 2))?;
            } else if defect == "foreign_bound_attempt" {
                let mut state = db
                    .transitions()
                    .load_execution_protocol_state_if_initialized("caller-agent")?
                    .unwrap();
                let mut foreign = state.attempts["routing-activation"].clone();
                foreign.attempt_id = "foreign-activation".into();
                foreign.turn_id = Some("foreign-turn".into());
                let mut foreign_state = state.clone();
                foreign_state.attempts = [(foreign.attempt_id.clone(), foreign)].into();
                let foreign_state = interrupt_execution(
                    &foreign_state,
                    &InterruptExecution {
                        attempt_id: "foreign-activation".into(),
                        outcome_id: "foreign-interruption".into(),
                        reason: "restart".into(),
                        interrupted_at: Utc::now().to_rfc3339(),
                    },
                )
                .map_err(anyhow::Error::msg)?
                .state;
                state.attempts.extend(foreign_state.attempts);
                state.outcomes.extend(foreign_state.outcomes);
                db.transaction(|tx| crate::runtime_db::transitions::persist_state_tx(tx, &state))?;
            }
            let queued = QueueEntryRecord {
                message_id: follower.id.clone(),
                agent_id: follower.agent_id.clone(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Queued,
                created_at: follower.created_at,
                updated_at: Utc::now(),
            };
            db.queue_entries().upsert(&queued)?;
            turn.input_message_ids.push(follower.id.clone());
            turn.idle_input_batch = Some(IdleInputBatchRecord {
                anchor_message_id: if defect == "wrong_anchor" {
                    "other-source".into()
                } else {
                    inputs[0].id.clone()
                },
                activation_id: Some("routing-activation".into()),
                sender_agent_id: "sender-a".into(),
                cutoff: Utc::now(),
                max_items: 8,
                max_bytes: 24 * 1024,
                rendered_bytes: 2,
                items: [&inputs[0], &follower]
                    .into_iter()
                    .map(|message| IdleInputBatchItem {
                        message_id: message.id.clone(),
                        source_ref: if defect == "wrong_ref" {
                            "message:other".into()
                        } else {
                            format!("message:{}", message.id)
                        },
                        rendered: "input".into(),
                    })
                    .collect(),
            });
            if defect == "missing_claim" {
                db.turn_records().upsert(&turn)?;
            } else {
                let mut consumed = queued;
                consumed.status = QueueEntryStatus::Interjected;
                db.transitions().commit_queue(&QueueTransitionCommand {
                    agent_id: "caller-agent".into(),
                    operation: QueueOperation::Interject,
                    mutation: QueueMutation::Consume(consumed),
                    scheduler_claim_work_item: None,
                    agent_state: None,
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: Some(turn),
                    audit_events: Vec::new(),
                    notify_scheduler: false,
                    fault: None,
                    brief_evidence: Vec::new(),
                })?;
            }
            let result = prepare_reply(&db, "sender-a", Some(&follower.id), false, "batched-reply");
            if matches!(defect, "none" | "current_turn" | "accepted_unbound_ingress") {
                let route = result?.message.routing_context.unwrap();
                assert_eq!(route.correlation_id.as_deref(), Some("batched-correlation"));
                assert_eq!(
                    route.in_reply_to_message_id.as_deref(),
                    Some(follower.id.as_str())
                );
            } else {
                assert!(result.is_err(), "{defect}");
            }
        }
        Ok(())
    }

    #[test]
    fn live_reply_routes_require_canonical_batch_and_accepted_ingress() -> Result<()> {
        use crate::types::{IdleInputBatchItem, LiveInputBatchRecord};

        for defect in [
            "none",
            "bound_current",
            "admitted_operator",
            "admitted_operator_parent",
            "missing_delivery",
            "rejected_delivery",
            "bound_foreign",
            "wrong_route",
            "wrong_activation",
            "wrong_ref",
            "missing_claim",
            "foreign_turn",
            "copied_replay",
            "pure_task_result",
            "authority_mismatch",
            "external_admission",
        ] {
            let (_dir, db) = runtime_db()?;
            let (mut turn, inputs) = seed_reply_turn_with_config(&db, &["sender-a"], 0, |turn| {
                if defect == "copied_replay" {
                    turn.replay = Some(crate::types::TurnReplayProvenance {
                        source_message_id: turn.input_message_ids[0].clone(),
                        source_turn_id: "unproven-source-turn".into(),
                        reason: "interrupted_queue_claim_reentry".into(),
                        prior_terminal: None,
                    });
                }
            })?;
            let mut follower = inputs[0].clone();
            follower.id = format!("live-source-{defect}");
            follower.turn_id = Some("live-ingress-turn".into());
            follower.message_seq = None;
            follower.routing_context.as_mut().unwrap().message_id = follower.id.clone();
            if defect == "pure_task_result" {
                follower.kind = MessageKind::TaskResult;
            } else if matches!(
                defect,
                "authority_mismatch" | "admitted_operator" | "admitted_operator_parent"
            ) {
                follower.authority_class = AuthorityClass::OperatorInstruction;
            } else if defect == "external_admission" {
                follower.admission_context = Some(AdmissionContext::ExternalTriggerCapability);
            }
            let follower = db.messages().append_with_index_changes(&follower, &[])?;
            if defect != "missing_delivery" {
                let mut delivery = accepted_reply_delivery(&follower, "sender-a", defect)?;
                if defect == "authority_mismatch" {
                    // Changing only the envelope cannot inherit operator authority.
                    delivery.caller.authority_class = AuthorityClass::ExternalEvidence;
                } else if defect == "admitted_operator_parent" {
                    delivery.caller.principal_kind = AgentMessagePrincipalKind::SupervisingParent;
                }
                if defect == "rejected_delivery" {
                    delivery.outcome = AgentMessageDeliveryOutcome::Rejected;
                    delivery.accepted_at = None;
                } else if defect == "bound_current" {
                    delivery.activation_id = Some("routing-activation".into());
                    delivery.turn_id = Some(turn.turn_id.clone());
                } else if defect == "bound_foreign" {
                    delivery.activation_id = Some("foreign-activation".into());
                    delivery.turn_id = Some("foreign-turn".into());
                } else if defect == "wrong_route" {
                    delivery.routing_context.as_mut().unwrap().correlation_id =
                        Some("other-correlation".into());
                }
                db.transaction(|tx| insert_delivery_tx(tx, &delivery))?;
            }
            if defect == "foreign_turn" {
                db.turn_records().upsert(&TurnRecord::new(
                    "caller-agent",
                    "live-ingress-turn",
                    2,
                ))?;
            }
            db.queue_entries().upsert(&QueueEntryRecord {
                message_id: follower.id.clone(),
                agent_id: follower.agent_id.clone(),
                priority: follower.priority,
                status: if defect == "missing_claim" {
                    QueueEntryStatus::Queued
                } else {
                    QueueEntryStatus::Interjected
                },
                created_at: follower.created_at,
                updated_at: Utc::now(),
            })?;
            turn.input_message_ids.push(follower.id.clone());
            turn.live_input_batches.push(LiveInputBatchRecord {
                activation_id: if defect == "wrong_activation" {
                    "foreign-activation".into()
                } else {
                    "routing-activation".into()
                },
                safe_point: crate::types::LiveInputSafePoint::new("routing-activation", 1),
                cutoff: Utc::now(),
                max_items: 8,
                max_bytes: 24 * 1024,
                rendered_bytes: 0,
                result_identities: Vec::new(),
                ordinary_message_ids: vec![follower.id.clone()],
                items: vec![IdleInputBatchItem {
                    message_id: follower.id.clone(),
                    source_ref: if defect == "wrong_ref" {
                        "message:other".into()
                    } else {
                        format!("message:{}", follower.id)
                    },
                    rendered: "live follower".into(),
                }],
            });
            db.turn_records().upsert(&turn)?;
            let result = prepare_reply(&db, "sender-a", Some(&follower.id), false, "live-reply");
            if matches!(
                defect,
                "none" | "bound_current" | "admitted_operator" | "admitted_operator_parent"
            ) {
                assert_eq!(
                    result?
                        .message
                        .routing_context
                        .unwrap()
                        .in_reply_to_message_id
                        .as_deref(),
                    Some(follower.id.as_str()),
                    "{defect}"
                );
            } else {
                assert!(result.is_err(), "{defect}");
            }
        }
        Ok(())
    }

    #[test]
    fn message_wait_binding_commits_atomically_with_delivery_and_survives_purge() -> Result<()> {
        use crate::types::{TaskKind, TaskRecord, TaskRecoverySpec, TaskStatus};
        let (dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let task = TaskRecord {
            id: "task-delivery".into(),
            agent_id: "caller-agent".into(),
            kind: TaskKind::AgentMessageWait,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: None,
            summary: Some("wait".into()),
            detail: Some(serde_json::json!({"target_agent_id":"target-agent"})),
            recovery: Some(TaskRecoverySpec::AgentMessageWait {
                summary: "wait".into(),
                message: "request".into(),
                target_agent_id: "target-agent".into(),
                authority_class: AuthorityClass::RuntimeInstruction,
                delivery_id: None,
                after_delivery_rowid: None,
            }),
        };
        db.tasks().upsert(&task)?;
        let prepared = prepare("atomic-wait", "request")?;
        let mut command = admission_command(&prepared);
        command.fault =
            Some(crate::runtime_db::transitions::TransitionFaultPoint::AfterCanonicalWrites);
        assert!(db
            .transitions()
            .commit_delivery_admission(&command, &prepared.record)
            .is_err());
        assert_eq!(db.tasks().latest(&task.id)?, Some(task.clone()));
        assert!(db
            .agent_message_deliveries()
            .latest(&prepared.record.delivery_id)?
            .is_none());
        command.fault = None;
        db.transitions()
            .commit_delivery_admission(&command, &prepared.record)?;
        let accepted = db.tasks().latest(&task.id)?.unwrap();
        let mut stale = task.clone();
        stale.updated_at = Utc::now();
        assert!(
            db.tasks().upsert(&stale).is_err(),
            "late status cannot erase an accepted binding"
        );
        assert_eq!(db.tasks().latest(&task.id)?, Some(accepted.clone()));
        assert_eq!(accepted.target().unwrap().incarnation, Some(1));
        let Some(TaskRecoverySpec::AgentMessageWait {
            delivery_id: Some(delivery),
            after_delivery_rowid: Some(cursor),
            ..
        }) = accepted.recovery.as_ref()
        else {
            panic!("acceptance lost parent-owned wait binding");
        };
        assert_eq!(delivery, &prepared.record.delivery_id);
        assert!(*cursor > 0);
        // A crash before monitor setup followed by target ledger purge cannot
        // erase the caller's generation or cause the request to be resent.
        db.transaction(|tx| {
            tx.execute(
                "DELETE FROM agent_message_deliveries WHERE target_agent_id='target-agent'",
                [],
            )?;
            Ok(())
        })?;
        drop(db);
        let reopened = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        assert_eq!(reopened.tasks().latest(&task.id)?, Some(accepted));
        Ok(())
    }

    #[test]
    fn accepted_delivery_is_idempotent_and_survives_restart() -> Result<()> {
        let (dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let prepared = prepare("stable-key", "hello")?;
        assert_eq!(prepared.message.turn_id, None);
        assert_eq!(prepared.message.task_id, None);
        assert_eq!(prepared.message.work_item_id, None);
        let first = db
            .transitions()
            .commit_delivery_admission(&admission_command(&prepared), &prepared.record)?;
        let first = first.delivery_receipt.context("missing delivery receipt")?;
        assert_eq!(first.outcome, AgentMessageDeliveryOutcome::Accepted);
        assert_eq!(first.state, AgentMessageDeliveryState::Queued);
        assert!(!first.idempotent_replay);

        let replay = prepare("stable-key", "hello")?;
        let replay = db
            .transitions()
            .commit_delivery_admission(&admission_command(&replay), &replay.record)?;
        let replay = replay.delivery_receipt.context("missing replay receipt")?;
        assert_eq!(replay.delivery_id, first.delivery_id);
        assert!(replay.idempotent_replay);
        assert_eq!(db.queue_entries().latest_all()?.len(), 1);

        let stored = db
            .agent_message_deliveries()
            .latest(&first.delivery_id)?
            .context("missing delivery ledger record")?;
        assert_eq!(
            stored.admission_evidence.principal_id.as_deref(),
            Some("runtime:agent-invocation")
        );
        assert_eq!(stored.admission_evidence.matched_rule_index, Some(1));
        assert_eq!(
            stored.caller.caller_agent_id.as_deref(),
            Some("caller-agent")
        );
        assert_eq!(
            stored.caller.current_turn_id.as_deref(),
            Some("turn-delivery")
        );
        assert_eq!(
            stored.caller.current_task_id.as_deref(),
            Some("task-delivery")
        );
        assert_eq!(
            stored.caller.current_work_item_id.as_deref(),
            Some("work-delivery")
        );

        let database_path = dir.path().join("state/runtime.sqlite");
        let lock_path = dir.path().join("state/runtime.lock");
        drop(db);
        let reopened = RuntimeDb::open_and_migrate(database_path, lock_path)?;
        let receipt = reopened
            .agent_message_deliveries()
            .receipt(&first.delivery_id)?
            .context("delivery receipt did not survive restart")?;
        assert_eq!(receipt.state, AgentMessageDeliveryState::Queued);
        assert_eq!(
            receipt.correlation_id.as_deref(),
            Some("correlation-delivery")
        );
        Ok(())
    }

    #[test]
    fn persistent_independent_peer_invocation_uses_auditable_derived_grant() -> Result<()> {
        let (dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let prepared = prepare_from_peer("peer-default", "delegate work")?;
        let accepted = db
            .transitions()
            .commit_delivery_admission(&admission_command(&prepared), &prepared.record)?;
        let delivery_id = accepted
            .delivery_receipt
            .context("missing accepted peer receipt")?
            .delivery_id;
        let stored = db
            .agent_message_deliveries()
            .latest(&delivery_id)?
            .context("missing peer delivery")?;
        assert_eq!(stored.outcome, AgentMessageDeliveryOutcome::Accepted);
        assert_eq!(stored.admission_evidence.matched_rule_index, None);
        assert_eq!(
            stored.admission_evidence.derived_grant,
            Some(AgentMessageDerivedGrant::PersistentIndependentPeerInvocation)
        );

        let database_path = dir.path().join("state/runtime.sqlite");
        let lock_path = dir.path().join("state/runtime.lock");
        drop(db);
        let reopened = RuntimeDb::open_and_migrate(database_path, lock_path)?;
        assert_eq!(
            reopened
                .agent_message_deliveries()
                .latest(&delivery_id)?
                .context("peer delivery did not survive restart")?
                .admission_evidence
                .derived_grant,
            Some(AgentMessageDerivedGrant::PersistentIndependentPeerInvocation)
        );
        Ok(())
    }

    #[test]
    fn routed_message_round_trips_through_message_repository_and_legacy_decode() -> Result<()> {
        let (dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let mut request = request("routing-roundtrip", "forwarded payload");
        request.forward = true;
        let prepared = AgentMessageDeliveryService::prepare(request, routed_caller())?;
        let expected = prepared
            .message
            .routing_context
            .clone()
            .context("prepared routing context missing")?;

        db.transitions()
            .commit_delivery_admission(&admission_command(&prepared), &prepared.record)?;
        let stored = db
            .messages()
            .recent(Some("target-agent"), 10)?
            .into_iter()
            .find(|message| message.id == prepared.message.id)
            .context("message repository did not persist routed message")?;
        assert_eq!(stored.routing_context, Some(expected.clone()));

        let database_path = dir.path().join("state/runtime.sqlite");
        let lock_path = dir.path().join("state/runtime.lock");
        drop(db);
        let reopened = RuntimeDb::open_and_migrate(database_path, lock_path)?;
        let replayed = reopened
            .messages()
            .recent(Some("target-agent"), 10)?
            .into_iter()
            .find(|message| message.id == prepared.message.id)
            .context("routed message did not survive restart")?;
        assert_eq!(replayed.routing_context, Some(expected));

        let mut legacy = serde_json::to_value(&replayed)?;
        legacy
            .as_object_mut()
            .expect("message payload must be an object")
            .remove("routing_context");
        let decoded: MessageEnvelope = serde_json::from_value(legacy)?;
        assert_eq!(decoded.routing_context, None);
        Ok(())
    }

    #[test]
    fn explicit_peer_deny_overrides_the_derived_grant() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let identity = db
            .agent_identities()
            .latest("target-agent")?
            .context("missing target identity")?;
        db.agent_canonical_relations()
            .upsert_message_policy(&AgentMessagePolicyRecord {
                agent_id: identity.agent_id,
                revision: 1,
                default_effect: AgentPolicyEffect::Deny,
                rules: vec![AgentMessagePolicyRule {
                    principal_kind: AgentMessagePrincipalKind::PeerAgent,
                    principal_id: Some("caller-agent".into()),
                    route: Some("agent_invocation".into()),
                    effect: AgentPolicyEffect::Deny,
                }],
                created_at: identity.created_at,
            })?;

        let prepared = prepare_from_peer("peer-deny", "delegate work")?;
        let rejected = db
            .agent_message_deliveries()
            .admit_without_queue(&prepared.record)?;
        assert_eq!(
            rejected.rejection_code,
            Some(AgentMessageDeliveryRejectionCode::MessageNotAuthorized)
        );
        let stored = db
            .agent_message_deliveries()
            .latest(&rejected.delivery_id)?
            .context("missing rejected peer delivery")?;
        assert_eq!(stored.admission_evidence.matched_rule_index, Some(0));
        assert_eq!(stored.admission_evidence.derived_grant, None);
        Ok(())
    }

    #[test]
    fn ephemeral_independent_target_has_no_peer_invocation_default() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let identity = db
            .agent_identities()
            .latest("target-agent")?
            .context("missing target identity")?;
        db.agent_canonical_relations()
            .upsert_durability(&AgentDurabilityRecord {
                agent_id: identity.agent_id.clone(),
                durability: AgentCanonicalDurability::Ephemeral,
                revision: 1,
                created_at: identity.created_at,
            })?;
        db.agent_canonical_relations().upsert_lifecycle_attachment(
            &AgentLifecycleAttachmentRecord {
                agent_id: identity.agent_id,
                attachment: AgentLifecycleAttachment::Independent,
                revision: 1,
                created_at: identity.created_at,
            },
        )?;

        let prepared = prepare_from_peer("peer-ephemeral", "delegate work")?;
        let rejected = db
            .agent_message_deliveries()
            .admit_without_queue(&prepared.record)?;
        assert_eq!(
            rejected.rejection_code,
            Some(AgentMessageDeliveryRejectionCode::MessageNotAuthorized)
        );
        let stored = db
            .agent_message_deliveries()
            .latest(&rejected.delivery_id)?
            .context("missing ephemeral rejection")?;
        assert_eq!(stored.admission_evidence.derived_grant, None);
        Ok(())
    }

    #[test]
    fn idempotency_conflict_is_typed_and_does_not_admit_twice() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let first = prepare("conflict-key", "first")?;
        db.transitions()
            .commit_delivery_admission(&admission_command(&first), &first.record)?;

        let conflicting = prepare("conflict-key", "different")?;
        let error = db
            .transitions()
            .commit_delivery_admission(&admission_command(&conflicting), &conflicting.record)
            .unwrap_err();
        assert!(error.downcast_ref::<AgentMessageDeliveryError>().is_some());
        assert_eq!(db.queue_entries().latest_all()?.len(), 1);
        Ok(())
    }

    #[test]
    fn retryable_rejection_can_succeed_with_the_same_key() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::Stopped)?;
        let rejected = prepare("retry-key", "retry me")?;
        let rejected = db
            .agent_message_deliveries()
            .admit_without_queue(&rejected.record)?;
        assert_eq!(rejected.outcome, AgentMessageDeliveryOutcome::Rejected);
        assert_eq!(
            rejected.rejection_code,
            Some(AgentMessageDeliveryRejectionCode::AgentStopped)
        );
        assert!(rejected.retryable);

        let mut state = db
            .agent_states()
            .latest("target-agent")?
            .context("missing target state")?;
        state.status = AgentStatus::AwakeIdle;
        db.agent_states().upsert(&state)?;
        let retry = prepare("retry-key", "retry me")?;
        let accepted = db
            .transitions()
            .commit_delivery_admission(&admission_command(&retry), &retry.record)?;
        let accepted = accepted
            .delivery_receipt
            .context("missing accepted retry receipt")?;
        assert_eq!(accepted.delivery_id, rejected.delivery_id);
        assert_eq!(accepted.outcome, AgentMessageDeliveryOutcome::Accepted);
        assert_eq!(accepted.state, AgentMessageDeliveryState::Queued);
        assert!(!accepted.idempotent_replay);
        Ok(())
    }

    #[test]
    fn queue_lifecycle_and_deletion_fence_are_linearized() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let prepared = prepare("delete-race", "race")?;
        let accepted = db
            .transitions()
            .commit_delivery_admission(&admission_command(&prepared), &prepared.record)?;
        let delivery_id = accepted
            .delivery_receipt
            .context("missing delivery receipt")?
            .delivery_id;
        let queued = db
            .queue_entries()
            .latest(&prepared.message.id)?
            .context("missing queue entry")?;
        queue_transition(
            &db,
            &queued,
            QueueOperation::Claim,
            QueueEntryStatus::Dequeued,
        )?;
        assert_eq!(
            db.agent_message_deliveries()
                .latest(&delivery_id)?
                .context("missing dispatched delivery")?
                .state,
            AgentMessageDeliveryState::Dispatched
        );

        let identity = db
            .agent_identities()
            .latest("target-agent")?
            .context("missing target identity")?;
        db.agent_deletions()
            .begin("target-agent", identity.revision, "test", false)?;
        let cancelled = db
            .agent_message_deliveries()
            .latest(&delivery_id)?
            .context("missing cancelled delivery")?;
        assert_eq!(
            cancelled.state,
            AgentMessageDeliveryState::CancelledByDeletion
        );
        assert!(cancelled.terminal_at.is_some());

        let deleting = prepare("after-fence", "late")?;
        let rejected = db
            .agent_message_deliveries()
            .admit_without_queue(&deleting.record)?;
        assert_eq!(
            rejected.rejection_code,
            Some(AgentMessageDeliveryRejectionCode::AgentDeleting)
        );
        assert!(!rejected.retryable);

        let dequeued = db
            .queue_entries()
            .latest(&prepared.message.id)?
            .context("missing dequeued entry")?;
        queue_transition(
            &db,
            &dequeued,
            QueueOperation::Settle,
            QueueEntryStatus::Processed,
        )?;
        assert_eq!(
            db.agent_message_deliveries()
                .latest(&delivery_id)?
                .context("missing terminal delivery")?
                .state,
            AgentMessageDeliveryState::CancelledByDeletion
        );
        Ok(())
    }

    #[test]
    fn scheduler_recovery_advances_delivery_projection_when_available() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let prepared = prepare("recovery-delivery", "recover")?;
        let accepted = db
            .transitions()
            .commit_delivery_admission(&admission_command(&prepared), &prepared.record)?;
        let delivery_id = accepted
            .delivery_receipt
            .context("missing delivery receipt")?
            .delivery_id;
        let queued = db
            .queue_entries()
            .latest(&prepared.message.id)?
            .context("missing queue entry")?;
        queue_transition(
            &db,
            &queued,
            QueueOperation::Claim,
            QueueEntryStatus::Dequeued,
        )?;
        let dequeued = db
            .queue_entries()
            .latest(&prepared.message.id)?
            .context("missing dequeued entry")?;

        recovery_transition(&db, &dequeued, QueueEntryStatus::Processed)?;

        assert_eq!(
            db.agent_message_deliveries()
                .latest(&delivery_id)?
                .context("missing recovered delivery")?
                .state,
            AgentMessageDeliveryState::Consumed
        );
        Ok(())
    }

    #[test]
    fn failed_delivery_diagnostic_is_bounded() -> Result<()> {
        let (_dir, db) = runtime_db()?;
        seed_target(&db, AgentStatus::AwakeIdle)?;
        let prepared = prepare("diagnostic-key", "fail")?;
        let accepted = db
            .transitions()
            .commit_delivery_admission(&admission_command(&prepared), &prepared.record)?;
        let delivery_id = accepted
            .delivery_receipt
            .context("missing delivery receipt")?
            .delivery_id;
        let diagnostic = "x".repeat(DELIVERY_DIAGNOSTIC_LIMIT + 100);
        db.transaction(|tx| {
            anyhow::ensure!(advance_delivery_state_for_message_tx(
                tx,
                &prepared.message.id,
                AgentMessageDeliveryState::Failed,
                Some(&diagnostic),
            )?);
            Ok(())
        })?;
        let failed = db
            .agent_message_deliveries()
            .latest(&delivery_id)?
            .context("missing failed delivery")?;
        assert_eq!(failed.state, AgentMessageDeliveryState::Failed);
        assert_eq!(
            failed
                .diagnostic
                .as_deref()
                .context("missing failure diagnostic")?
                .chars()
                .count(),
            DELIVERY_DIAGNOSTIC_LIMIT
        );
        Ok(())
    }
}
