//! One immutable winner per message wait. A final-brief outbox and its content
//! commit with the responding execution's normal settlement; the normal task
//! monitor publishes it through atomic terminal-task/result/queue settlement.

use super::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

use crate::{
    domain::execution_protocol::{
        ConversationOutcome, ExecutionOutcome, ExecutionOutcomeRecord, ExecutionProtocolState,
        ExecutionSourceIdentity, WorkItemOutcome,
    },
    runtime_db::{evidence::append_message_tx, RuntimeIndexChange},
    types::{
        AuthorityClass, BriefKind, BriefRecord, MessageBody, Priority, TaskKind, TaskRecord,
        TaskRecoverySpec, TaskStatus, TranscriptEntry, TranscriptEntryKind, TurnTerminalKind,
    },
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum MessageWaitReply {
    Explicit {
        delivery_id: String,
        message_id: String,
    },
    FinalBrief {
        message_id: String,
        activation_id: String,
        turn_id: String,
        brief_id: String,
    },
    ReplyTimeout {
        deadline_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MessageWaitOutcome {
    pub task_id: String,
    pub owner_agent_id: String,
    pub target_agent_id: String,
    pub request_delivery_id: String,
    pub reply: MessageWaitReply,
    pub committed_at: DateTime<Utc>,
}

fn task_tx(tx: &Transaction<'_>, task_id: &str) -> Result<Option<TaskRecord>> {
    tx.query_row(
        "SELECT payload_json FROM tasks WHERE task_id = ?1",
        [task_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|payload| super::super::repositories::decode_task_payload(&payload))
    .transpose()
}

fn wait_contract(task: &TaskRecord) -> Option<(&str, &str, i64, Option<DateTime<Utc>>)> {
    if task.kind != TaskKind::AgentMessageWait {
        return None;
    }
    match task.recovery.as_ref()? {
        TaskRecoverySpec::AgentMessageWait {
            target_agent_id,
            delivery_id: Some(delivery_id),
            after_delivery_rowid: Some(boundary),
            reply_deadline_at,
            ..
        } => Some((target_agent_id, delivery_id, *boundary, *reply_deadline_at)),
        _ => None,
    }
}

fn outcome_tx(tx: &Transaction<'_>, task: &TaskRecord) -> Result<Option<MessageWaitOutcome>> {
    let Some((target, request, _, _)) = wait_contract(task) else {
        return Ok(None);
    };
    let outcome = tx
        .query_row(
            "SELECT payload_json FROM agent_message_wait_outcomes WHERE task_id = ?1",
            [&task.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<MessageWaitOutcome>(&payload))
        .transpose()?;
    if let Some(outcome) = &outcome {
        anyhow::ensure!(
            outcome.task_id == task.id
                && outcome.owner_agent_id == task.agent_id
                && outcome.target_agent_id == target
                && outcome.request_delivery_id == request,
            "message wait outcome does not match its immutable request"
        );
    }
    Ok(outcome)
}

fn explicit_reply_tx(tx: &Transaction<'_>, task: &TaskRecord) -> Result<Option<MessageWaitReply>> {
    let Some((target, _, boundary, deadline)) = wait_contract(task) else {
        return Ok(None);
    };
    let mut statement = tx.prepare(
        "SELECT payload_json FROM agent_message_deliveries
         WHERE rowid > ?1 AND target_agent_id = ?2 AND outcome = 'accepted'
         ORDER BY rowid ASC",
    )?;
    let rows = statement.query_map(params![boundary, task.agent_id], |row| {
        row.get::<_, String>(0)
    })?;
    for payload in rows {
        let delivery = decode_delivery(&payload?)?;
        if delivery.caller.caller_agent_id.as_deref() != Some(target)
            || deadline.is_some_and(|deadline| {
                delivery
                    .accepted_at
                    .is_none_or(|accepted| accepted > deadline)
            })
        {
            continue;
        }
        return Ok(Some(MessageWaitReply::Explicit {
            delivery_id: delivery.delivery_id,
            message_id: delivery
                .message_id
                .context("accepted reply is missing its message")?,
        }));
    }
    Ok(None)
}

fn insert_outcome_tx(
    tx: &Transaction<'_>,
    task: &TaskRecord,
    reply: MessageWaitReply,
    now: DateTime<Utc>,
) -> Result<MessageWaitOutcome> {
    let (target, request, _, _) = wait_contract(task).context("message wait receipt is missing")?;
    let outcome = MessageWaitOutcome {
        task_id: task.id.clone(),
        owner_agent_id: task.agent_id.clone(),
        target_agent_id: target.into(),
        request_delivery_id: request.into(),
        reply,
        committed_at: now,
    };
    tx.execute(
        "INSERT INTO agent_message_wait_outcomes (task_id, owner_agent_id, payload_json)
         VALUES (?1, ?2, ?3)",
        params![task.id, task.agent_id, serde_json::to_string(&outcome)?],
    )?;
    Ok(outcome)
}

/// Called under the writer transaction, so accepted replies, guaranteed queued
/// brief replies, timeout and source settlement have one ordering.
pub(crate) fn resolve_message_wait_tx(
    tx: &Transaction<'_>,
    task_id: &str,
) -> Result<Option<MessageWaitOutcome>> {
    let Some(task) = task_tx(tx, task_id)? else {
        return Ok(None);
    };
    if !super::super::repositories::is_active_task_status(&task.status) {
        return Ok(None);
    }
    if let Some(outcome) = outcome_tx(tx, &task)? {
        return Ok(Some(outcome));
    }
    let Some((_, _, _, deadline)) = wait_contract(&task) else {
        return Ok(None);
    };
    let now = Utc::now();
    if let Some(reply) = explicit_reply_tx(tx, &task)? {
        return insert_outcome_tx(tx, &task, reply, now).map(Some);
    }
    if let Some(deadline_at) = deadline.filter(|deadline| now >= *deadline) {
        return insert_outcome_tx(
            tx,
            &task,
            MessageWaitReply::ReplyTimeout { deadline_at },
            now,
        )
        .map(Some);
    }
    Ok(None)
}

impl AgentMessageDeliveryRepository<'_> {
    pub(crate) fn resolve_message_wait(&self, task_id: &str) -> Result<Option<MessageWaitOutcome>> {
        self.db
            .transaction(|tx| resolve_message_wait_tx(tx, task_id))
    }
}

pub(crate) struct PreparedFinalBriefReply {
    task_id: String,
    target_incarnation: u64,
    activation_id: String,
    turn_id: String,
    brief: BriefRecord,
    request_message_id: String,
    source_message_id: String,
    consumed_activation_id: String,
    consumed_turn_id: String,
}

/// Recovery envelopes are infrastructure inputs, not additional business
/// requests. Only the canonical provider resolver may connect them to a request.
fn provider_reply_source_tx(
    tx: &Transaction<'_>,
    turn: &TurnRecord,
) -> Result<Option<(ReplyRouteRegistry, String, String, String)>> {
    if !turn.trigger.as_ref().is_some_and(|trigger| {
        trigger.kind == MessageKind::InternalFollowup
            && trigger.authority_class == AuthorityClass::RuntimeInstruction
            && matches!(&trigger.origin,
                MessageOrigin::System { subsystem } if subsystem == "model_lineage_recovery")
    }) {
        return Ok(None);
    }
    let mut state = ExecutionProtocolState::empty(&turn.agent_id);
    let mut statement =
        tx.prepare("SELECT payload_json FROM execution_protocol_attempts WHERE agent_id = ?1")?;
    for row in statement.query_map([&turn.agent_id], |row| row.get::<_, String>(0))? {
        let attempt: ExecutionAttempt = serde_json::from_str(&row?)?;
        state.attempts.insert(attempt.attempt_id.clone(), attempt);
    }
    let mut statement =
        tx.prepare("SELECT payload_json FROM execution_protocol_outcomes WHERE agent_id = ?1")?;
    for row in statement.query_map([&turn.agent_id], |row| row.get::<_, String>(0))? {
        let outcome: ExecutionOutcomeRecord = serde_json::from_str(&row?)?;
        state.outcomes.insert(outcome.outcome_id.clone(), outcome);
    }
    let mut open = state.attempts.values().filter(|attempt| {
        attempt.state == ExecutionAttemptState::Open && attempt.agent_id == turn.agent_id
    });
    let Some(attempt) = open.next() else {
        return Ok(None);
    };
    if open.next().is_some() || attempt.terminal_outcome_id.is_some() {
        return Ok(None);
    }
    let Some(source_id) = attempt.source_message_id.as_deref() else {
        return Ok(None);
    };
    if !matches!(&attempt.source.identity,
        ExecutionSourceIdentity::RuntimeRecovery { recovery_id } if recovery_id == source_id)
        || turn
            .trigger
            .as_ref()
            .and_then(|trigger| trigger.message_id.as_deref())
            != Some(source_id)
    {
        return Ok(None);
    }
    let execution: ReplyRouteExecutionState = serde_json::from_str(&tx.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
        [&turn.agent_id],
        |row| row.get::<_, String>(0),
    )?)?;
    let Some(binding) = execution.current_execution_binding else {
        return Ok(None);
    };
    if execution.current_turn_id.as_deref() != Some(turn.turn_id.as_str())
        || binding.turn_id != turn.turn_id
        || binding.source_message_id != source_id
        || binding.activation_id.as_deref() != Some(attempt.attempt_id.as_str())
        || binding.owner.as_ref() != Some(&turn.effective_owner())
        || !matches!(&binding.admission_provenance,
            Some(crate::types::ExecutionAdmissionProvenance::Canonical { activation_id, .. })
                if activation_id == &attempt.attempt_id)
        || attempt.admitted_fences.host_registry_revision
            != crate::runtime_db::transitions::authority_fences_tx(tx, &turn.agent_id)?
                .host_registry_revision
    {
        return Ok(None);
    }
    let read_message = |id: &str| -> Result<Option<MessageEnvelope>> {
        tx.query_row(
            "SELECT payload_json FROM messages WHERE message_id = ?1",
            [id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|text| serde_json::from_str(&text).map_err(Into::into))
        .transpose()
    };
    let Some(message) = read_message(source_id)? else {
        return Ok(None);
    };
    let source =
        crate::runtime::provider_recovery::resolve_with(&message, &state, read_message, |id| {
            tx.query_row(
                "SELECT payload_json FROM turn_records WHERE turn_id = ?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|text| serde_json::from_str(&text).map_err(Into::into))
            .transpose()
        });
    let Ok(source) = source else { return Ok(None) };
    if !source.is_initial_attempt(&message, attempt)
        || source
            .source_turns
            .iter()
            .any(|source_turn| source_turn.effective_owner() != turn.effective_owner())
    {
        return Ok(None);
    }
    let mut allowed =
        std::collections::BTreeSet::from([source.root_message.id.as_str(), source_id]);
    for predecessor in &source.predecessors {
        if let Some(id) = predecessor.source_message_id.as_deref() {
            allowed.insert(id);
        }
    }
    if !turn
        .input_message_ids
        .iter()
        .any(|id| id == &source.root_message.id)
        || !turn.input_message_ids.iter().any(|id| id == source_id)
        || std::iter::once(turn)
            .chain(source.source_turns.iter())
            .any(|source_turn| {
                source_turn
                    .input_message_ids
                    .iter()
                    .any(|id| !allowed.contains(id.as_str()))
            })
        || source
            .source_turns
            .first()
            .is_none_or(|root| root.input_message_ids != [source.root_message.id.clone()])
    {
        return Ok(None);
    }
    let Some(request) = delivery_by_message_id_tx(tx, &source.root_message.id)? else {
        return Ok(None);
    };
    let Some(root_turn) = source.source_turns.first() else {
        return Ok(None);
    };
    if !request.authenticates_agent_message(&source.root_message)
        || request.state != AgentMessageDeliveryState::Consumed
        || request.turn_id.as_deref() != Some(root_turn.turn_id.as_str())
        || !source.predecessors.iter().any(|prior| {
            Some(&prior.attempt_id) == request.activation_id.as_ref()
                && prior.source_message_id.as_deref() == Some(source.root_message.id.as_str())
        })
    {
        return Ok(None);
    }
    let Some(route) = source.root_message.routing_context else {
        return Ok(None);
    };
    if route.message_id != source.root_message.id || route.recipient_agent_id != turn.agent_id {
        return Ok(None);
    }
    Ok(Some((
        ReplyRouteRegistry {
            activation_id: attempt.attempt_id.clone(),
            turn_id: turn.turn_id.clone(),
            routes: vec![route],
            latest_input_routed: true,
        },
        source_id.into(),
        request
            .activation_id
            .expect("validated consumed activation"),
        root_turn.turn_id.clone(),
    )))
}

pub(crate) fn prepare_final_brief_reply_tx(
    tx: &Transaction<'_>,
    turn: &TurnRecord,
    new_briefs: &[BriefRecord],
    new_rounds: &[TranscriptEntry],
) -> Result<Option<PreparedFinalBriefReply>> {
    if turn
        .terminal
        .as_ref()
        .is_none_or(|terminal| terminal.kind != TurnTerminalKind::Completed)
        || !turn.waiting_condition_ids.is_empty()
        || turn.runtime_failure.is_some()
    {
        return Ok(None);
    }
    let (registry, source_message_id, consumed_activation_id, consumed_turn_id) =
        if let Some(recovery) = provider_reply_source_tx(tx, turn)? {
            recovery
        } else {
            if turn.input_message_ids.len() != 1 {
                return Ok(None);
            }
            let Some(registry) = load_reply_routes_for_settlement_tx(tx, &turn.agent_id, turn)?
            else {
                return Ok(None);
            };
            let consumed = (
                turn.input_message_ids[0].clone(),
                registry.activation_id.clone(),
                registry.turn_id.clone(),
            );
            (registry, consumed.0, consumed.1, consumed.2)
        };
    if registry.routes.len() != 1 || !registry.latest_input_routed {
        return Ok(None);
    }
    let route = &registry.routes[0];
    if !turn.input_message_ids.contains(&route.message_id) {
        return Ok(None);
    }
    let Some(request) = delivery_by_message_id_tx(tx, &route.message_id)? else {
        return Ok(None);
    };
    let Some(task_id) = request.caller.current_task_id.as_deref() else {
        return Ok(None);
    };
    if !matches!(&request.caller.origin, MessageOrigin::Task { task_id: origin } if origin == task_id)
        || request.correlation_id.as_deref() != Some(task_id)
        || request.target_agent_id != turn.agent_id
    {
        return Ok(None);
    }
    let Some(task) = task_tx(tx, task_id)? else {
        return Ok(None);
    };
    let Some((target, delivery_id, boundary, _)) = wait_contract(&task) else {
        return Ok(None);
    };
    let requester_incarnation = match task.recovery.as_ref() {
        Some(TaskRecoverySpec::AgentMessageWait {
            requester_incarnation,
            ..
        }) => *requester_incarnation,
        _ => None,
    };
    let Some(requester_incarnation) = requester_incarnation else {
        // Historical tasks lack the caller-incarnation evidence needed by this
        // new channel; retain their explicit-message-only contract.
        return Ok(None);
    };
    let target_incarnation = task.target().and_then(|target| target.incarnation);
    let Some(target_incarnation) = target_incarnation else {
        return Ok(None);
    };
    if target != turn.agent_id
        || delivery_id != request.delivery_id
        || request
            .expected_incarnation
            .is_some_and(|value| value != target_incarnation)
        || request.caller.caller_agent_id.as_deref() != Some(task.agent_id.as_str())
        || route.sender_agent_id.as_deref() != Some(task.agent_id.as_str())
        || !super::super::repositories::is_active_task_status(&task.status)
        || outcome_tx(tx, &task)?.is_some()
    {
        return Ok(None);
    }
    let rowid: i64 = tx.query_row(
        "SELECT rowid FROM agent_message_deliveries WHERE delivery_id = ?1",
        [&request.delivery_id],
        |row| row.get(0),
    )?;
    if rowid != boundary {
        return Ok(None);
    }
    for (agent_id, incarnation) in [
        (&task.agent_id, requester_incarnation),
        (&turn.agent_id, target_incarnation),
    ] {
        let identity = tx
            .query_row(
                "SELECT payload_json FROM agent_identities WHERE agent_id = ?1",
                [agent_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| serde_json::from_str::<AgentIdentityRecord>(&payload))
            .transpose()?;
        if identity.is_none_or(|identity| {
            identity.incarnation != incarnation || identity.status != AgentRegistryStatus::Active
        }) {
            return Ok(None);
        }
    }
    let mut briefs = Vec::new();
    for brief_id in &turn.produced_brief_ids {
        let brief = if let Some(brief) = new_briefs.iter().find(|brief| &brief.id == brief_id) {
            Some(brief.clone())
        } else {
            tx.query_row(
                "SELECT payload_json FROM briefs WHERE evidence_id = ?1",
                [brief_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| serde_json::from_str::<BriefRecord>(&payload))
            .transpose()?
        };
        if let Some(brief) = brief.filter(|brief| brief.kind == BriefKind::Result) {
            briefs.push(brief);
        }
    }
    let [brief] = briefs.as_slice() else {
        return Ok(None);
    };
    if brief.agent_id != turn.agent_id
        || brief.turn_id.as_deref() != Some(turn.turn_id.as_str())
        || brief.work_item_id != turn.current_work_item_id
        || brief.text.trim().is_empty()
        || brief.turn_index != Some(turn.turn_index)
    {
        return Ok(None);
    }
    let Some(round_id) = brief.finalizes_assistant_round_id.as_deref() else {
        return Ok(None);
    };
    let round = if let Some(round) = new_rounds.iter().find(|round| round.id == round_id) {
        Some(round.clone())
    } else {
        tx.query_row(
            "SELECT payload_json FROM transcript_entries WHERE evidence_id = ?1",
            [round_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<TranscriptEntry>(&payload))
        .transpose()?
    };
    if round.is_none_or(|round| {
        round.agent_id != turn.agent_id
            || round.kind != TranscriptEntryKind::AssistantRound
            || round.data["turn_id"].as_str() != Some(turn.turn_id.as_str())
            || round.data["round_purpose"].as_str() != Some("agent_response")
            || round.data["visibility"].as_str() != Some("operator_visible")
    }) {
        return Ok(None);
    }
    Ok(Some(PreparedFinalBriefReply {
        task_id: task.id,
        target_incarnation,
        activation_id: registry.activation_id,
        turn_id: registry.turn_id,
        brief: brief.clone(),
        request_message_id: route.message_id.clone(),
        source_message_id,
        consumed_activation_id,
        consumed_turn_id,
    }))
}

pub(crate) fn commit_final_brief_reply_tx(
    tx: &Transaction<'_>,
    prepared: Option<PreparedFinalBriefReply>,
) -> Result<Vec<RuntimeIndexChange>> {
    let Some(prepared) = prepared else {
        return Ok(Vec::new());
    };
    let Some(task) = task_tx(tx, &prepared.task_id)? else {
        return Ok(Vec::new());
    };
    if !super::super::repositories::is_active_task_status(&task.status)
        || outcome_tx(tx, &task)?.is_some()
    {
        return Ok(Vec::new());
    }
    let attempt = tx
        .query_row(
            "SELECT payload_json FROM execution_protocol_attempts WHERE attempt_id = ?1 AND agent_id = ?2",
            params![prepared.activation_id, prepared.brief.agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<ExecutionAttempt>(&payload))
        .transpose()?;
    let Some(attempt) = attempt else {
        return Ok(Vec::new());
    };
    let Some(outcome_id) = attempt.terminal_outcome_id.as_deref() else {
        return Ok(Vec::new());
    };
    let outcome = tx
        .query_row(
            "SELECT payload_json FROM execution_protocol_outcomes WHERE outcome_id = ?1 AND agent_id = ?2",
            params![outcome_id, prepared.brief.agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|payload| serde_json::from_str::<ExecutionOutcomeRecord>(&payload))
        .transpose()?;
    let Some(outcome) = outcome else {
        return Ok(Vec::new());
    };
    let request = delivery_by_message_id_tx(tx, &prepared.request_message_id)?;
    if attempt.state != ExecutionAttemptState::Settled
        || attempt.agent_id != prepared.brief.agent_id
        || attempt.source_message_id.as_deref() != Some(prepared.source_message_id.as_str())
        || request.is_none_or(|request| {
            request.state != AgentMessageDeliveryState::Consumed
                || request.activation_id.as_deref()
                    != Some(prepared.consumed_activation_id.as_str())
                || request.turn_id.as_deref() != Some(prepared.consumed_turn_id.as_str())
        })
        || outcome.attempt_id != attempt.attempt_id
        || !matches!(
            outcome.outcome,
            ExecutionOutcome::Conversation(ConversationOutcome::Replied)
                | ExecutionOutcome::WorkItem(WorkItemOutcome::Complete { .. })
        )
    {
        return Ok(Vec::new());
    }
    // Recheck under this same transaction, before queuing any derived content.
    if resolve_message_wait_tx(tx, &task.id)?.is_some() {
        return Ok(Vec::new());
    }
    let now = Utc::now();
    if let Some(deadline_at) = wait_contract(&task)
        .expect("validated wait")
        .3
        .filter(|deadline| now >= *deadline)
    {
        insert_outcome_tx(
            tx,
            &task,
            MessageWaitReply::ReplyTimeout { deadline_at },
            now,
        )?;
        return Ok(Vec::new());
    }
    let mut message = MessageEnvelope::new(
        &task.agent_id,
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "agent_final_brief_reply".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: prepared.brief.text.clone(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    message.id = format!("message:agent-final-brief-reply:{}", task.id);
    message.created_at = now;
    message.work_item_id = task.work_item_id.clone();
    message.correlation_id = Some(task.id.clone());
    message.causation_id = Some(prepared.request_message_id.clone());
    message.source_refs.extend([
        ("request_message_id".into(), prepared.request_message_id),
        (
            "request_delivery_id".into(),
            wait_contract(&task).expect("validated wait").1.into(),
        ),
        ("source_agent_id".into(), prepared.brief.agent_id.clone()),
        (
            "source_incarnation".into(),
            prepared.target_incarnation.to_string(),
        ),
        (
            "source_activation_id".into(),
            prepared.activation_id.clone(),
        ),
        ("source_turn_id".into(), prepared.turn_id.clone()),
        ("source_brief_id".into(), prepared.brief.id.clone()),
    ]);
    message.metadata = Some(serde_json::json!({
        "runtime_derived_agent_reply": {
            "waiting_task_id": task.id, "content_source": "final_brief",
            "content_authority": "agent_output", "source_refs": message.source_refs,
        }
    }));
    let (message, _) = append_message_tx(tx, &message)?;
    insert_outcome_tx(
        tx,
        &task,
        MessageWaitReply::FinalBrief {
            message_id: message.id.clone(),
            activation_id: prepared.activation_id,
            turn_id: prepared.turn_id,
            brief_id: prepared.brief.id,
        },
        now,
    )?;
    Ok(vec![RuntimeIndexChange::for_message(&message)])
}

pub(crate) fn validate_message_wait_terminal_tx(
    tx: &Transaction<'_>,
    task: &TaskRecord,
) -> Result<()> {
    if task.kind != TaskKind::AgentMessageWait
        || !matches!(task.status, TaskStatus::Completed | TaskStatus::Failed)
    {
        return Ok(());
    }
    let Some(outcome) = outcome_tx(tx, task)? else {
        return Ok(());
    };
    let detail = task
        .detail
        .as_ref()
        .context("message wait result detail is missing")?;
    let matches = match &outcome.reply {
        MessageWaitReply::Explicit {
            message_id,
            delivery_id,
        } => {
            task.status == TaskStatus::Completed
                && detail["message_id"].as_str() == Some(message_id)
                && detail["response_delivery_id"].as_str() == Some(delivery_id)
        }
        MessageWaitReply::FinalBrief { message_id, .. } => {
            task.status == TaskStatus::Completed
                && detail["message_id"].as_str() == Some(message_id)
                && detail["reply_content_source"].as_str() == Some("runtime_derived_final_brief")
        }
        MessageWaitReply::ReplyTimeout { .. } => {
            task.status == TaskStatus::Failed
                && detail["failure_reason"].as_str() == Some("reply_timeout")
        }
    };
    anyhow::ensure!(
        matches,
        "message wait terminal conflicts with its durable reply winner"
    );
    Ok(())
}
