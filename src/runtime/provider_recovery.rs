//! Durable source resolution shared by recovery admission and context assembly.
use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};

use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, ExecutionProtocolState,
        ExecutionSourceIdentity,
    },
    storage::AppStorage,
    types::{MessageEnvelope, TurnOwner, TurnRecord, TurnTerminalKind},
};

#[derive(Debug, Clone)]
pub(crate) struct ProviderRecoverySource {
    pub predecessor: ExecutionAttempt,
    pub source_turn: TurnRecord,
    pub root_message: MessageEnvelope,
}

pub(crate) fn resolve(
    storage: &AppStorage,
    message: &MessageEnvelope,
    state: &ExecutionProtocolState,
) -> Result<ProviderRecoverySource> {
    let mut cursor = message.clone();
    let mut seen = BTreeSet::new();
    let mut first = None;
    let mut expected_binding = None;
    loop {
        if seen.len() >= 64 || !seen.insert(cursor.id.clone()) {
            bail!("provider recovery lineage is cyclic or exceeds its bound");
        }
        let selection = super::turn::TurnModelSelection::from_message(&cursor)
            .map_err(|_| anyhow!("provider recovery directive malformed"))?;
        let directive = selection
            .recovery
            .as_ref()
            .ok_or_else(|| anyhow!("provider recovery directive missing"))?;
        if state.agent_id != message.agent_id
            || cursor.agent_id != message.agent_id
            || !matches!(
                directive.source_terminal_kind,
                TurnTerminalKind::DeferredToFallback
                    | TurnTerminalKind::ProviderFailedNeedsRecovery
            )
            || cursor.source_refs.get("source_turn_id") != Some(&directive.source_turn_id)
            || cursor.source_refs.get("source_message_id") != Some(&directive.source_message_id)
            || cursor.causation_id.as_deref() != Some(directive.source_message_id.as_str())
        {
            bail!("provider recovery source references mismatch");
        }
        let source_message = storage
            .read_message_by_id(&directive.source_message_id)?
            .ok_or_else(|| anyhow!("provider recovery source message missing"))?;
        let source_turn = storage
            .read_turn_by_id(&directive.source_turn_id)?
            .ok_or_else(|| anyhow!("provider recovery source turn missing"))?;
        if source_message.agent_id != message.agent_id
            || source_message.turn_id.as_deref() != Some(source_turn.turn_id.as_str())
            || source_turn.agent_id != message.agent_id
            || source_turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                != Some(source_message.id.as_str())
            || source_turn.terminal.as_ref().map(|terminal| terminal.kind)
                != Some(directive.source_terminal_kind)
        {
            bail!("provider recovery source turn mismatch");
        }
        let mut attempts = state.attempts.values().filter(|attempt| {
            attempt.agent_id == message.agent_id
                && attempt.source_message_id.as_deref() == Some(source_message.id.as_str())
                && attempt.turn_id.as_deref() == Some(source_turn.turn_id.as_str())
                && matches!(
                    attempt.state,
                    ExecutionAttemptState::Settled | ExecutionAttemptState::Interrupted
                )
                && attempt.terminal_outcome_id.as_ref().is_some_and(|id| {
                    state
                        .outcomes
                        .get(id)
                        .is_some_and(|outcome| outcome.attempt_id == attempt.attempt_id)
                })
        });
        let predecessor = attempts
            .next()
            .ok_or_else(|| anyhow!("provider recovery predecessor missing"))?;
        if attempts.next().is_some() {
            bail!("provider recovery predecessor ambiguous");
        }
        let owner = match &predecessor.binding {
            ExecutionBinding::WorkItem { work_item_id } => TurnOwner::WorkItem {
                work_item_id: work_item_id.clone(),
            },
            ExecutionBinding::Conversation { interaction_id } => TurnOwner::Conversation {
                interaction_id: interaction_id.clone(),
            },
            ExecutionBinding::AgentLifecycle { agent_id } => TurnOwner::AgentLifecycle {
                agent_id: agent_id.clone(),
            },
            ExecutionBinding::Command => TurnOwner::Command,
        };
        if source_turn.owner.as_ref() != Some(&owner)
            || cursor.work_item_id.as_deref() != owner.work_item_id()
            || expected_binding
                .as_ref()
                .is_some_and(|binding| binding != &predecessor.binding)
        {
            bail!("provider recovery binding mismatch");
        }
        expected_binding = Some(predecessor.binding.clone());
        if first.is_none() {
            first = Some((predecessor.clone(), source_turn));
        }
        if matches!(
            predecessor.source.identity,
            ExecutionSourceIdentity::RuntimeRecovery { .. }
        ) {
            let prior_id = predecessor
                .recovery_of_attempt_id
                .as_ref()
                .ok_or_else(|| anyhow!("provider recovery predecessor lineage missing"))?;
            let prior = state
                .attempts
                .get(prior_id)
                .ok_or_else(|| anyhow!("provider recovery prior attempt missing"))?;
            let prior_selection = super::turn::TurnModelSelection::from_message(&source_message)
                .map_err(|_| anyhow!("provider recovery prior directive malformed"))?;
            let prior_directive = prior_selection
                .recovery
                .as_ref()
                .ok_or_else(|| anyhow!("provider recovery prior directive missing"))?;
            if prior.source_message_id.as_deref()
                != Some(prior_directive.source_message_id.as_str())
                || prior.turn_id.as_deref() != Some(prior_directive.source_turn_id.as_str())
            {
                bail!("provider recovery predecessor lineage mismatch");
            }
            cursor = source_message;
        } else {
            if super::turn::TurnModelSelection::message_has_provider_recovery_provenance(
                &source_message,
            ) {
                bail!("provider recovery root has inconsistent execution identity");
            }
            let (predecessor, source_turn) = first.expect("first source is resolved");
            return Ok(ProviderRecoverySource {
                predecessor,
                source_turn,
                root_message: source_message,
            });
        }
    }
}

pub(crate) fn resolve_for_context(
    storage: &AppStorage,
    message: &MessageEnvelope,
) -> Result<Option<ProviderRecoverySource>> {
    if !super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message) {
        return Ok(None);
    }
    let db = storage
        .runtime_db()?
        .ok_or_else(|| anyhow!("provider recovery database missing"))?;
    let state = db
        .transitions()
        .load_execution_protocol_state_if_initialized(&message.agent_id)?
        .ok_or_else(|| anyhow!("provider recovery execution state missing"))?;
    resolve(storage, message, &state).map(Some)
}

// References are consumed only after durable resolution by context assembly.
pub(crate) fn continuity_turn_id(message: &MessageEnvelope) -> Option<&str> {
    super::turn::TurnModelSelection::message_has_provider_recovery_provenance(message)
        .then(|| {
            message
                .source_refs
                .get("source_turn_id")
                .map(String::as_str)
        })
        .flatten()
}
