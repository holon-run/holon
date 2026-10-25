//! Durable source resolution shared by recovery admission and context assembly.
use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};

use crate::{
    domain::execution_protocol::{
        ExecutionAttempt, ExecutionAttemptState, ExecutionBinding, ExecutionProtocolState,
        ExecutionSourceIdentity,
    },
    storage::AppStorage,
    types::{
        MessageEnvelope, TurnOwner, TurnRecord, TurnTerminalKind, WaitConditionRecord,
        WaitConditionStatus,
    },
};

#[derive(Debug, Clone)]
pub(crate) struct ProviderRecoverySource {
    pub predecessor: ExecutionAttempt,
    pub predecessors: Vec<ExecutionAttempt>,
    pub source_turns: Vec<TurnRecord>,
    pub root_message: MessageEnvelope,
}

impl ProviderRecoverySource {
    pub fn exact_wait_id(&self) -> Option<&str> {
        self.predecessors
            .iter()
            .find_map(|attempt| match &attempt.source.identity {
                ExecutionSourceIdentity::TriggeredWait {
                    wait_id,
                    trigger_message_id,
                    ..
                } if trigger_message_id == &self.root_message.id => Some(wait_id.as_str()),
                _ => None,
            })
    }

    pub fn source_turn(&self) -> &TurnRecord {
        self.source_turns
            .last()
            .expect("validated provider recovery lineage is nonempty")
    }

    pub fn is_initial_attempt(
        &self,
        message: &MessageEnvelope,
        attempt: &ExecutionAttempt,
    ) -> bool {
        attempt.agent_id == message.agent_id
            && attempt.source_message_id.as_deref() == Some(message.id.as_str())
            && attempt.binding == self.predecessor.binding
            && attempt.recovery_of_attempt_id.as_deref()
                == Some(self.predecessor.attempt_id.as_str())
    }

    pub fn matches_attempt(
        &self,
        message: &MessageEnvelope,
        turn: &TurnRecord,
        attempt: &ExecutionAttempt,
        state: &ExecutionProtocolState,
        mut read_turn: impl FnMut(&str) -> Result<Option<TurnRecord>>,
    ) -> Result<bool> {
        if state.agent_id != message.agent_id
            || turn.agent_id != message.agent_id
            || turn.effective_owner() != self.source_turn().effective_owner()
            || turn
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                != Some(message.id.as_str())
        {
            return Ok(false);
        }
        if self.is_initial_attempt(message, attempt) {
            return Ok(true);
        }
        // Only the bounded, same-message interrupted replay may bridge the
        // current attempt to this recovery directive's exact predecessor.
        Ok(
            initial_source_attempt(message, turn, attempt, state, &mut read_turn)?
                .is_some_and(|initial| self.is_initial_attempt(message, initial)),
        )
    }

    pub fn interrupted_initial_attempt<'a>(
        &self,
        message: &MessageEnvelope,
        state: &'a ExecutionProtocolState,
    ) -> Option<&'a ExecutionAttempt> {
        let mut candidates = state.attempts.values().filter(|attempt| {
            attempt.state == ExecutionAttemptState::Interrupted
                && self.is_initial_attempt(message, attempt)
                && !state.attempts.values().any(|successor| {
                    successor.recovery_of_attempt_id.as_deref() == Some(attempt.attempt_id.as_str())
                })
        });
        let attempt = candidates.next()?;
        candidates.next().is_none().then_some(attempt)
    }
}

pub(crate) fn resolve(
    storage: &AppStorage,
    message: &MessageEnvelope,
    state: &ExecutionProtocolState,
) -> Result<ProviderRecoverySource> {
    resolve_with(
        message,
        state,
        |id| storage.read_message_by_id(id),
        |id| storage.read_turn_by_id(id),
    )
}

pub(crate) fn resolved_exact_wait(
    storage: &AppStorage,
    root_message: &MessageEnvelope,
    work_item_id: &str,
    wait_id: &str,
) -> Result<Option<WaitConditionRecord>> {
    Ok(storage
        .latest_wait_conditions_for_agent(&root_message.agent_id)?
        .into_iter()
        .find(|wait| {
            wait.id == wait_id
                && wait.work_item_id.as_deref() == Some(work_item_id)
                && wait.status == WaitConditionStatus::Resolved
                && wait.trigger_message_id() == Some(root_message.id.as_str())
                && super::scheduler::message_matches_wait_condition(root_message, wait)
        }))
}

/// The transition uses the same resolver against its transaction snapshot.
pub(crate) fn resolve_with(
    message: &MessageEnvelope,
    state: &ExecutionProtocolState,
    mut read_message: impl FnMut(&str) -> Result<Option<MessageEnvelope>>,
    mut read_turn: impl FnMut(&str) -> Result<Option<TurnRecord>>,
) -> Result<ProviderRecoverySource> {
    let mut cursor = message.clone();
    let mut seen = BTreeSet::new();
    let mut first = None;
    let mut source_turns = Vec::new();
    let mut predecessors = Vec::new();
    let mut expected_binding = None;
    let mut expected_predecessor = None;
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
        let source_message = read_message(&directive.source_message_id)?
            .ok_or_else(|| anyhow!("provider recovery source message missing"))?;
        let source_turn = read_turn(&directive.source_turn_id)?
            .ok_or_else(|| anyhow!("provider recovery source turn missing"))?;
        if source_message.agent_id != message.agent_id
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
        let attempts = state.attempts.values().filter(|attempt| {
            attempt.agent_id == message.agent_id
                && attempt.source_message_id.as_deref() == Some(source_message.id.as_str())
                && attempt.turn_id == source_message.turn_id
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
        let mut resolved = Vec::new();
        for attempt in attempts {
            if let Some(initial) = initial_source_attempt(
                &source_message,
                &source_turn,
                attempt,
                state,
                &mut read_turn,
            )? {
                resolved.push((attempt, initial));
            }
        }
        let (predecessor, initial) = resolved
            .first()
            .copied()
            .ok_or_else(|| anyhow!("provider recovery predecessor missing"))?;
        if resolved.len() != 1 {
            bail!("provider recovery predecessor ambiguous");
        }
        if expected_predecessor
            .as_ref()
            .is_some_and(|id| id != &predecessor.attempt_id)
        {
            bail!("provider recovery predecessor lineage mismatch");
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
            first = Some(predecessor.clone());
        }
        predecessors.push(predecessor.clone());
        if initial.attempt_id != predecessor.attempt_id {
            predecessors.push(initial.clone());
        }
        source_turns.push(source_turn);
        if matches!(
            predecessor.source.identity,
            ExecutionSourceIdentity::RuntimeRecovery { .. }
        ) {
            let prior_id = initial
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
            {
                bail!("provider recovery predecessor lineage mismatch");
            }
            expected_predecessor = Some(prior.attempt_id.clone());
            cursor = source_message;
        } else {
            if super::turn::TurnModelSelection::message_has_provider_recovery_provenance(
                &source_message,
            ) {
                bail!("provider recovery root has inconsistent execution identity");
            }
            let predecessor = first.expect("first source is resolved");
            source_turns.reverse();
            predecessors.reverse();
            return Ok(ProviderRecoverySource {
                predecessor,
                predecessors,
                source_turns,
                root_message: source_message,
            });
        }
    }
}

fn initial_source_attempt<'a>(
    message: &MessageEnvelope,
    turn: &TurnRecord,
    attempt: &'a ExecutionAttempt,
    state: &'a ExecutionProtocolState,
    read_turn: &mut impl FnMut(&str) -> Result<Option<TurnRecord>>,
) -> Result<Option<&'a ExecutionAttempt>> {
    // Task-result ingress may legitimately have no turn stamp. Its canonical
    // initial attempt and persisted trigger still identify the failed turn.
    if message.turn_id.is_none() && turn.replay.is_none() {
        return Ok(attempt.recovery_of_attempt_id.is_none().then_some(attempt));
    }
    let Some(ingress_turn) = message.turn_id.as_deref().or_else(|| {
        turn.replay
            .as_ref()
            .map(|replay| replay.source_turn_id.as_str())
    }) else {
        return Ok(None);
    };
    if turn.turn_id == ingress_turn {
        return Ok(Some(attempt));
    }
    let Some(replay) = turn.replay.as_ref() else {
        return Ok(None);
    };
    let Some(prior) = attempt
        .recovery_of_attempt_id
        .as_ref()
        .and_then(|id| state.attempts.get(id))
    else {
        return Ok(None);
    };
    if replay.source_message_id != message.id
        || replay.source_turn_id != ingress_turn
        || replay.reason != "interrupted_queue_claim_reentry"
        || prior.state != ExecutionAttemptState::Interrupted
        || prior.agent_id != attempt.agent_id
        || prior.source_message_id != attempt.source_message_id
        || prior.turn_id != attempt.turn_id
        || prior.source != attempt.source
        || prior.binding != attempt.binding
        || prior.provenance != attempt.provenance
        // Provider hops do not spend the one same-message replay allowance.
        || prior.recovery_of_attempt_id.as_ref().and_then(|id| state.attempts.get(id))
            .is_some_and(|ancestor| ancestor.source_message_id == prior.source_message_id)
    {
        return Ok(None);
    }
    if let Some(interrupted) = read_turn(ingress_turn)? {
        if interrupted.agent_id != turn.agent_id
            || interrupted.effective_owner() != turn.effective_owner()
            || interrupted
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_deref())
                != Some(message.id.as_str())
            || interrupted.terminal.as_ref().map(|terminal| terminal.kind)
                != Some(TurnTerminalKind::Interrupted)
        {
            return Ok(None);
        }
    } else if replay.prior_terminal.is_some() {
        return Ok(None);
    }
    Ok(Some(prior))
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
