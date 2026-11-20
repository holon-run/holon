//! Historical replay classification never grants turn ownership or hydration.
use std::collections::BTreeSet;

use anyhow::Result;

use crate::{
    domain::execution_protocol::{
        ExecutionAttemptState, ExecutionBinding, ExecutionSourceIdentity,
    },
    types::{MessageEnvelope, TurnOwner, TurnRecord, TurnTerminalKind, TurnTriggerSummary},
};

const REPLAY_LINEAGE_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageReferenceClass {
    OwnedByTurn,
    VerifiedReplayReference,
    ForeignOrUnverifiableReference,
}

fn attempt_stamp_matches(
    attempt_turn_id: Option<&str>,
    turn: &TurnRecord,
    anchor: &MessageEnvelope,
) -> bool {
    attempt_turn_id.is_none_or(|id| id == turn.turn_id || anchor.turn_id.as_deref() == Some(id))
}

fn source_matches_anchor(source: &ExecutionSourceIdentity, anchor_id: &str) -> bool {
    match source {
        ExecutionSourceIdentity::QueueMessage { message_id }
        | ExecutionSourceIdentity::InternalFollowup { message_id } => message_id == anchor_id,
        ExecutionSourceIdentity::TaskResult {
            result_message_id, ..
        }
        | ExecutionSourceIdentity::ChildResult {
            result_message_id, ..
        } => result_message_id == anchor_id,
        ExecutionSourceIdentity::TriggeredWait {
            trigger_message_id, ..
        } => trigger_message_id == anchor_id,
        _ => false,
    }
}

pub(super) fn classify_message_reference(
    storage: &impl super::ContextInputs,
    turn: &TurnRecord,
    message: &MessageEnvelope,
) -> Result<MessageReferenceClass> {
    if storage.inspect_assigned_turn(&message.id, |assigned| {
        super::turn_record_owns_object(turn, assigned)
    })? {
        return Ok(MessageReferenceClass::OwnedByTurn);
    }
    if verified_replay_reference(storage, turn, message)? {
        return Ok(MessageReferenceClass::VerifiedReplayReference);
    }
    Ok(MessageReferenceClass::ForeignOrUnverifiableReference)
}

fn verified_replay_reference(
    storage: &impl super::ContextInputs,
    turn: &TurnRecord,
    message: &MessageEnvelope,
) -> Result<bool> {
    let Some(replay) = turn.replay.as_ref() else {
        return Ok(false);
    };
    let Some(anchor) = storage.history_message_by_id(&replay.source_message_id)? else {
        return Ok(false);
    };
    let owner = turn.effective_owner();
    let trigger = TurnTriggerSummary::from_message(&anchor);
    if anchor.id != replay.source_message_id
        || message.agent_id != turn.agent_id
        || anchor.agent_id != turn.agent_id
        || message
            .turn_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
        || anchor
            .turn_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
        || turn.trigger.as_ref() != Some(&trigger)
        || !snapshot_contains(turn, &anchor.id, &message.id)
    {
        return Ok(false);
    }
    let Some(activation_id) = turn
        .idle_input_batch
        .as_ref()
        .and_then(|batch| batch.activation_id.as_deref())
    else {
        return Ok(false);
    };
    let Some(mut attempt) = storage.execution_attempt_by_id(&turn.agent_id, activation_id)? else {
        return Ok(false);
    };
    if attempt.agent_id != turn.agent_id
        || attempt.attempt_id != activation_id
        || attempt.source_message_id.as_deref() != Some(anchor.id.as_str())
        || !source_matches_anchor(&attempt.source.identity, &anchor.id)
        || !attempt_stamp_matches(attempt.turn_id.as_deref(), turn, &anchor)
        || !binding_matches_owner(&attempt.binding, &owner)
        || attempt.state == ExecutionAttemptState::ProtocolViolation
    {
        return Ok(false);
    }
    let mut previous_turn: Option<super::inputs::ContextHistoryTurn> = None;
    let mut seen_turns = BTreeSet::from([turn.turn_id.clone()]);
    let mut seen_attempts = BTreeSet::from([attempt.attempt_id.clone()]);
    let mut anchor_ingress_found = false;
    let mut message_ingress_found = false;
    // Match the runtime's bounded, cycle-checked replay ancestry, not numeric ids
    // or attempt.turn_id: attempts may retain the original ingress turn stamp.
    for _ in 0..REPLAY_LINEAGE_LIMIT {
        let younger = previous_turn.as_deref().unwrap_or(turn);
        let Some(replay) = younger.replay.as_ref() else {
            return Ok(anchor_ingress_found
                && message_ingress_found
                && attempt.recovery_of_attempt_id.is_none());
        };
        if replay.reason != "interrupted_queue_claim_reentry"
            || replay.source_message_id != anchor.id
            || replay.source_turn_id.trim().is_empty()
            || !seen_turns.insert(replay.source_turn_id.clone())
        {
            return Ok(false);
        }
        let Some(source) = storage.history_turn_by_id(&replay.source_turn_id)? else {
            return Ok(false);
        };
        let Some(prior_id) = attempt.recovery_of_attempt_id.as_deref() else {
            return Ok(false);
        };
        if !seen_attempts.insert(prior_id.to_owned()) {
            return Ok(false);
        }
        let Some(prior) = storage.execution_attempt_by_id(&turn.agent_id, prior_id)? else {
            return Ok(false);
        };
        if prior.agent_id != turn.agent_id
            || prior.attempt_id != prior_id
            || source.agent_id != turn.agent_id
            || source.turn_id != replay.source_turn_id
            || source.effective_owner() != owner
            || source.trigger.as_ref() != Some(&trigger)
            || source
                .terminal
                .as_ref()
                .is_none_or(|terminal| terminal.kind != TurnTerminalKind::Interrupted)
            || !snapshot_contains(&source, &anchor.id, &message.id)
            || !same_snapshot(&younger, &source)
            || source
                .idle_input_batch
                .as_ref()
                .and_then(|batch| batch.activation_id.as_deref())
                != Some(prior.attempt_id.as_str())
            || prior.state != ExecutionAttemptState::Interrupted
            || prior.source_message_id != attempt.source_message_id
            || prior.source.identity != attempt.source.identity
            || prior.binding != attempt.binding
            || !attempt_stamp_matches(prior.turn_id.as_deref(), &source, &anchor)
        {
            return Ok(false);
        }
        anchor_ingress_found |= anchor.turn_id.as_deref() == Some(source.turn_id.as_str());
        message_ingress_found |= message.turn_id.as_deref() == Some(source.turn_id.as_str());
        previous_turn = Some(source);
        attempt = prior;
    }
    let younger = previous_turn.as_deref().unwrap_or(turn);
    Ok(younger.replay.is_none()
        && anchor_ingress_found
        && message_ingress_found
        && attempt.recovery_of_attempt_id.is_none())
}

fn snapshot_contains(turn: &TurnRecord, anchor_id: &str, message_id: &str) -> bool {
    turn.idle_input_batch.as_ref().is_some_and(|batch| {
        batch.anchor_message_id == anchor_id
            && batch.items.first().is_some_and(|item| {
                item.message_id == anchor_id && item.source_ref == format!("message:{anchor_id}")
            })
            && turn.input_message_ids.iter().any(|id| id == anchor_id)
            && turn.input_message_ids.iter().any(|id| id == message_id)
            && batch.items.iter().any(|item| {
                item.message_id == message_id && item.source_ref == format!("message:{message_id}")
            })
    })
}

fn same_snapshot(younger: &TurnRecord, source: &TurnRecord) -> bool {
    match (&younger.idle_input_batch, &source.idle_input_batch) {
        (Some(younger), Some(source)) => {
            younger.anchor_message_id == source.anchor_message_id
                && younger.sender_agent_id == source.sender_agent_id
                && younger.cutoff == source.cutoff
                && younger.max_items == source.max_items
                && younger.max_bytes == source.max_bytes
                && younger.rendered_bytes == source.rendered_bytes
                && younger.items == source.items
        }
        _ => false,
    }
}

fn binding_matches_owner(binding: &ExecutionBinding, owner: &TurnOwner) -> bool {
    match (binding, owner) {
        (
            ExecutionBinding::WorkItem { work_item_id },
            TurnOwner::WorkItem {
                work_item_id: owner,
            },
        ) => work_item_id == owner,
        (
            ExecutionBinding::Conversation { interaction_id },
            TurnOwner::Conversation {
                interaction_id: owner,
            },
        ) => interaction_id == owner,
        (
            ExecutionBinding::AgentLifecycle { agent_id },
            TurnOwner::AgentLifecycle { agent_id: owner },
        ) => agent_id == owner,
        (ExecutionBinding::Command, TurnOwner::Command) => true,
        _ => false,
    }
}
