//! A replay terminal may differ from its Message's immutable assignment only
//! through the already-qualified canonical Turn and complete Attempt closure.
use super::*;
use crate::domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding};
use crate::runtime::resident_input_evidence::ExecutionInputCoverage;
use crate::types::TurnTerminalKind;

pub(super) fn validate(
    records: &BTreeMap<Key, Arc<QualifiedInputEvidence>>,
    execution: &ExecutionInputCoverage,
    source: &str,
    terminal_turn: &str,
    assigned_turn: &str,
) -> Result<()> {
    let record = |family: Family, id: &str| -> Result<&QualifiedInputEvidence> {
        let key = Key::new(family, id);
        records.get(&key).map(Arc::as_ref).ok_or_else(|| {
            InputEvidenceSnapshotError::NeedsPreparation(format!(
                "provider replay {}:{} is not qualified",
                family.as_str(),
                id
            ))
            .into()
        })
    };
    let turn = |id| -> Result<&TurnRecord> {
        match &record(Family::Turn, id)?.record {
            PreparedInputEvidence::Turn(turn) => Ok(turn),
            _ => anyhow::bail!("provider replay Turn family differs"),
        }
    };
    let attempt = |id| -> Result<&ExecutionAttempt> {
        match &record(Family::Attempt, id)?.record {
            PreparedInputEvidence::Attempt(attempt) => Ok(attempt),
            _ => anyhow::bail!("provider replay Attempt family differs"),
        }
    };
    let message = match &record(Family::Message, source)?.record {
        PreparedInputEvidence::Message(message) => message,
        _ => anyhow::bail!("provider replay Message family differs"),
    };
    let current = turn(terminal_turn)?;
    let original = turn(assigned_turn)?;
    let replay = current
        .replay
        .as_ref()
        .context("provider terminal differs from assignment without canonical replay")?;
    ensure!(
        current.turn_id != original.turn_id
            && current.agent_id == message.agent_id
            && original.agent_id == message.agent_id
            && current.effective_owner() == original.effective_owner()
            && replay.source_turn_id == original.turn_id
            && replay.source_message_id == message.id
            && replay.reason == "interrupted_queue_claim_reentry"
            && replay
                .prior_terminal
                .as_ref()
                .is_none_or(|summary| summary.kind == TurnTerminalKind::Interrupted)
            && original.terminal.as_ref().map(|summary| summary.kind)
                == Some(TurnTerminalKind::Interrupted)
            && [current, original].iter().all(|turn| {
                turn.trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref())
                    == Some(message.id.as_str())
                    && turn.input_message_ids.contains(&message.id)
            }),
        "provider replay lacks its original interrupted Turn relation"
    );

    let mut matches = 0;
    for observed in execution
        .source(source)?
        .attempts
        .values()
        .filter(|observed| {
            !observed.pruned
                && observed.turn_id_hint == message.turn_id
                && observed.terminal_outcome_id.is_some()
                && matches!(
                    observed.state,
                    ExecutionAttemptState::Settled | ExecutionAttemptState::Interrupted
                )
        })
    {
        let current_attempt = attempt(&observed.attempt_id)?;
        ensure!(
            observed.matches(current_attempt)
                && current_attempt.agent_id == message.agent_id
                && current_attempt.source_message_id.as_deref() == Some(source),
            "provider replay candidate differs from its complete source index"
        );
        let Some(prior_id) = current_attempt.recovery_of_attempt_id.as_deref() else {
            continue;
        };
        let prior = attempt(prior_id)?;
        let owner = match &current_attempt.binding {
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
        if current.effective_owner() != owner
            || prior.attempt_id == current_attempt.attempt_id
            || prior.state != ExecutionAttemptState::Interrupted
            || prior.agent_id != current_attempt.agent_id
            || prior.source_message_id != current_attempt.source_message_id
            || prior.turn_id != current_attempt.turn_id
            || prior.source != current_attempt.source
            || prior.binding != current_attempt.binding
            || prior.provenance != current_attempt.provenance
            || !record(Family::Attempt, &current_attempt.attempt_id)?
                .attempt_turn_keys
                .as_ref()
                .is_some_and(|keys| keys.contains(&Key::new(Family::Turn, terminal_turn)))
        {
            continue;
        }
        if let Some(ancestor_id) = prior.recovery_of_attempt_id.as_deref() {
            if attempt(ancestor_id)?.source_message_id == prior.source_message_id {
                continue;
            }
        }
        let outcome = current_attempt
            .terminal_outcome_id
            .as_deref()
            .context("provider replay candidate lost its terminal outcome")?;
        ensure!(
            matches!(&record(Family::Outcome, outcome)?.record,
                PreparedInputEvidence::Outcome(outcome)
                    if outcome.attempt_id == current_attempt.attempt_id),
            "provider replay candidate has no exact terminal outcome"
        );
        matches += 1;
    }
    ensure!(
        matches == 1,
        "provider replay needs exactly one original interrupted Attempt predecessor"
    );
    Ok(())
}
