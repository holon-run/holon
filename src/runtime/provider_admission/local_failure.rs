//! Local dispatch rejection is an execution stop, never provider recovery.

use super::*;
use crate::domain::execution_protocol::{
    CommandResult, ConversationOutcome, ExecutionBinding, ExecutionOutcome, ExecutionOutcomeRecord,
    ExecutionProtocolCommand, SettleExecution, WorkItemExecutionState, WorkItemOutcome,
};
use crate::runtime_db::transitions::ExecutionProtocolTransition;
use crate::types::{QueueEntryRecord, TurnOwner, TurnRecord};

pub(in crate::runtime) fn local_dispatch_failure_settlement(
    storage: &crate::storage::AppStorage,
    runtime_db: &RuntimeDb,
    record: &QueueEntryRecord,
    terminal_turn: Option<&TurnRecord>,
    attempt: &ExecutionAttempt,
) -> Result<Option<ExecutionProtocolTransition>> {
    let owner = match &attempt.binding {
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
    let Some(message) = storage.read_message_by_id(&record.message_id)? else {
        return Ok(None);
    };
    let Some(turn) = crate::runtime::canonical_matching_terminal_turn(
        runtime_db,
        record,
        &message,
        &owner,
        terminal_turn,
    )?
    else {
        return Ok(None);
    };
    let Some(_) = turn.runtime_failure.as_ref().filter(|failure| {
        failure.domain == crate::runtime_error::RuntimeErrorDomain::Conflict
            && failure.code == "provider_dispatch_rejected"
            && !failure.retryable
            && turn
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.kind == crate::types::TurnTerminalKind::Aborted)
    }) else {
        return Ok(None);
    };
    let reason = format!("provider_dispatch_rejected:{}", turn.turn_id);
    let outcome = match &attempt.binding {
        ExecutionBinding::WorkItem { work_item_id } => {
            let protocol = runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized(&attempt.agent_id)?
                .context("local dispatch stop requires canonical execution state")?;
            ensure!(
                protocol.work_items.get(work_item_id).is_some_and(|work| {
                    matches!(
                        &work.state,
                        WorkItemExecutionState::InFlight { generation, attempt_id }
                            if attempt_id == &attempt.attempt_id
                                && Some(*generation) == attempt.admitted_fences.work_item_generation
                    )
                }),
                "local dispatch stop cannot settle a replacement owner"
            );
            ExecutionOutcome::WorkItem(WorkItemOutcome::NeedsRepair { repair_id: reason })
        }
        ExecutionBinding::Conversation { .. } | ExecutionBinding::AgentLifecycle { .. } => {
            ExecutionOutcome::Conversation(ConversationOutcome::Paused { reason })
        }
        ExecutionBinding::Command => {
            ExecutionOutcome::Command(CommandResult::Quarantined { reason })
        }
    };
    Ok(Some(ExecutionProtocolTransition {
        bootstrap: None,
        commands: vec![ExecutionProtocolCommand::Settle(SettleExecution {
            outcome: ExecutionOutcomeRecord {
                outcome_id: format!("outcome:message:{}", record.message_id),
                attempt_id: attempt.attempt_id.clone(),
                outcome,
                created_at: record.updated_at.to_rfc3339(),
            },
        })],
    }))
}
