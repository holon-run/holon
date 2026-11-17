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
    let predicate = turn
        .runtime_failure
        .as_ref()
        .and_then(|failure| failure.safe_context.get("predicate"))
        .cloned()
        .unwrap_or_else(|| "unknown".into());
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
            ExecutionOutcome::WorkItem(dispatch_rejection_work_item_outcome(
                work_item_id,
                &turn.turn_id,
                &predicate,
                &protocol,
            ))
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

/// Bounded automatic re-entry for admission conflicts rejected before any
/// provider IO. The guard fired with zero model cost and zero side effects,
/// so retrying from a fresh snapshot is safe; genuine invariant violations
/// (`AncestryCycle`, `OwnerMissing`, provenance mismatch) never clear and
/// pause instead, mirroring how the same failure pauses a conversation.
pub(in crate::runtime) fn dispatch_rejection_work_item_outcome(
    work_item_id: &str,
    turn_id: &str,
    predicate: &str,
    protocol: &crate::domain::execution_protocol::ExecutionProtocolState,
) -> WorkItemOutcome {
    const INVARIANT_VIOLATIONS: [&str; 3] = [
        "AncestryCycle",
        "OwnerMissing",
        "SnapshotProvenanceMismatch",
    ];
    if INVARIANT_VIOLATIONS
        .iter()
        .any(|violation| predicate.contains(violation))
    {
        return WorkItemOutcome::Pause {
            reason: format!("provider_dispatch_rejected_invariant:{predicate}:{turn_id}"),
        };
    }
    let prior_reentries = protocol
        .outcomes
        .values()
        .filter(|outcome| {
            matches!(
                &outcome.outcome,
                ExecutionOutcome::WorkItem(WorkItemOutcome::Reentry { recovery_ref })
                    if recovery_ref.starts_with("provider_dispatch_rejected")
            )
        })
        .filter(|outcome| {
            protocol
                .attempts
                .get(&outcome.attempt_id)
                .is_some_and(|record| {
                    matches!(
                        &record.binding,
                        ExecutionBinding::WorkItem { work_item_id: id } if id == work_item_id
                    )
                })
        })
        .count();
    if prior_reentries >= PROVIDER_DISPATCH_REENTRY_LIMIT {
        return WorkItemOutcome::Pause {
            reason: format!(
                "provider_dispatch_rejected_retry_exhausted:{work_item_id}:{prior_reentries}"
            ),
        };
    }
    WorkItemOutcome::Reentry {
        recovery_ref: format!("provider_dispatch_rejected:{turn_id}"),
    }
}

/// How many pre-IO admission re-entries a WorkItem may take before the
/// conflict is surfaced as Paused for explicit blocker clearance. Backoff
/// between attempts comes from the queued-available no-progress policy.
pub(in crate::runtime) const PROVIDER_DISPATCH_REENTRY_LIMIT: usize = 3;

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
mod reentry_tests {
    use super::*;
    use crate::domain::execution_protocol::{
        AdmittedFences, ExecutionAttempt, ExecutionAttemptState, ExecutionOrigin,
        ExecutionOutcomeRecord, ExecutionPriority, ExecutionProtocolState, ExecutionProvenance,
        ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
    };

    fn settled_work_item_attempt(work_item_id: &str, index: usize) -> ExecutionAttempt {
        ExecutionAttempt {
            input_projection: None,
            attempt_id: format!("attempt-{index}"),
            agent_id: "agent-a".into(),
            source_message_id: Some(format!("message-{index}")),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::WorkItemContinuation {
                    work_item_id: work_item_id.into(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::WorkItem {
                work_item_id: work_item_id.into(),
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::System,
                trust: ExecutionTrust::RuntimeInstruction,
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: 1,
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Settled,
            run_id: None,
            turn_id: Some(format!("turn-{index}")),
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: "2026-11-17T00:00:00Z".into(),
            terminal_at: None,
        }
    }

    fn protocol_with_reentries(work_item_id: &str, reentries: usize) -> ExecutionProtocolState {
        let mut protocol = ExecutionProtocolState::empty("agent-a");
        for index in 0..reentries {
            let attempt = settled_work_item_attempt(work_item_id, index);
            protocol
                .attempts
                .insert(attempt.attempt_id.clone(), attempt.clone());
            let outcome = ExecutionOutcomeRecord {
                outcome_id: format!("outcome-{index}"),
                attempt_id: attempt.attempt_id.clone(),
                outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Reentry {
                    recovery_ref: format!("provider_dispatch_rejected:turn-{index}"),
                }),
                created_at: "2026-11-17T00:00:00Z".into(),
            };
            protocol
                .outcomes
                .insert(outcome.outcome_id.clone(), outcome);
        }
        protocol
    }

    #[test]
    fn dispatch_rejection_reenters_runnable_below_the_retry_limit() {
        for reentries in 0..PROVIDER_DISPATCH_REENTRY_LIMIT {
            let protocol = protocol_with_reentries("work-a", reentries);
            assert_eq!(
                dispatch_rejection_work_item_outcome(
                    "work-a",
                    "turn-new",
                    "work_item_revision_drift",
                    &protocol,
                ),
                WorkItemOutcome::Reentry {
                    recovery_ref: "provider_dispatch_rejected:turn-new".into(),
                }
            );
        }
    }

    #[test]
    fn dispatch_rejection_pauses_once_the_retry_limit_is_exhausted() {
        let protocol = protocol_with_reentries("work-a", PROVIDER_DISPATCH_REENTRY_LIMIT);
        assert_eq!(
            dispatch_rejection_work_item_outcome(
                "work-a",
                "turn-new",
                "work_item_revision_drift",
                &protocol,
            ),
            WorkItemOutcome::Pause {
                reason: format!(
                    "provider_dispatch_rejected_retry_exhausted:work-a:{}",
                    PROVIDER_DISPATCH_REENTRY_LIMIT
                ),
            }
        );
    }

    #[test]
    fn dispatch_rejection_pauses_immediately_for_invariant_violations() {
        for predicate in [
            "AncestryCycle",
            "OwnerMissing",
            "SnapshotProvenanceMismatch",
        ] {
            let protocol = protocol_with_reentries("work-a", 0);
            assert_eq!(
                dispatch_rejection_work_item_outcome("work-a", "turn-new", predicate, &protocol),
                WorkItemOutcome::Pause {
                    reason: format!("provider_dispatch_rejected_invariant:{predicate}:turn-new"),
                },
                "invariant violations must never clear by retrying"
            );
        }
    }

    #[test]
    fn reentry_budget_counts_only_this_work_items_rejections() {
        let protocol = protocol_with_reentries("work-b", PROVIDER_DISPATCH_REENTRY_LIMIT);
        assert_eq!(
            dispatch_rejection_work_item_outcome(
                "work-a",
                "turn-new",
                "work_item_revision_drift",
                &protocol,
            ),
            WorkItemOutcome::Reentry {
                recovery_ref: "provider_dispatch_rejected:turn-new".into(),
            },
            "another WorkItem's budget must not consume this one's"
        );
    }
}
