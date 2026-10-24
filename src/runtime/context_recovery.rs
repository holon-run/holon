//! Local context stops use canonical WorkItem settlement, never provider recovery.

use super::*;
use crate::domain::execution_protocol::{
    ContextBudgetStop, ExecutionAttempt, ExecutionBinding, ExecutionOutcome,
    ExecutionOutcomeRecord, ExecutionProtocolCommand, RecheckContextBudgetStop, SettleExecution,
    WorkItemExecutionState, WorkItemOutcome,
};
use crate::runtime_db::transitions::ExecutionProtocolTransition;
use crate::runtime_error::RuntimeErrorDomain;

pub(super) fn context_budget_failure_settlement(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    record: &QueueEntryRecord,
    terminal_turn: Option<&TurnRecord>,
    attempt: &ExecutionAttempt,
) -> Result<Option<ExecutionProtocolTransition>> {
    let ExecutionBinding::WorkItem { work_item_id } = &attempt.binding else {
        return Ok(None);
    };
    let Some(message) = storage.read_message_by_id(&record.message_id)? else {
        return Ok(None);
    };
    let owner = crate::types::TurnOwner::WorkItem {
        work_item_id: work_item_id.clone(),
    };
    let Some(turn) =
        canonical_matching_terminal_turn(runtime_db, record, &message, &owner, terminal_turn)?
    else {
        return Ok(None);
    };
    let Some(failure) = turn.runtime_failure.as_ref().filter(|failure| {
        failure.domain == RuntimeErrorDomain::Validation
            && failure.code == "context_pinned_minimum_over_budget"
            && !failure.retryable
            && turn
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.kind == crate::types::TurnTerminalKind::Aborted)
    }) else {
        return Ok(None);
    };
    let Some(configured_estimated_tokens) = failure
        .safe_context
        .get("configured_budget_estimated_tokens")
        .and_then(|budget| budget.parse::<usize>().ok())
    else {
        return Ok(None);
    };
    let Some(source_revision) = attempt.admitted_fences.work_item_source_revision else {
        return Ok(None);
    };
    let stop = ContextBudgetStop {
        // A concurrent source update must remain a changed condition after settlement.
        source_revision,
        source_turn_id: turn.turn_id,
        configured_estimated_tokens,
    };
    Ok(Some(ExecutionProtocolTransition {
        bootstrap: None,
        commands: vec![ExecutionProtocolCommand::Settle(SettleExecution {
            outcome: ExecutionOutcomeRecord {
                outcome_id: format!("outcome:message:{}", record.message_id),
                attempt_id: attempt.attempt_id.clone(),
                outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Failed {
                    policy: stop.policy()?,
                }),
                created_at: record.updated_at.to_rfc3339(),
            },
        })],
    }))
}

impl RuntimeHandle {
    pub(super) async fn reconcile_context_budget_stops(&self) -> Result<()> {
        let agent_id = self.agent_id().await?;
        let Some(state) = self
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized(&agent_id)?
        else {
            return Ok(());
        };
        let configured_budget = self
            .current_context_config()
            .await
            .prompt_budget_estimated_tokens;
        for (id, expected) in state.work_items {
            let WorkItemExecutionState::Paused { reason, .. } = &expected.state else {
                continue;
            };
            let Some(stop) = ContextBudgetStop::from_policy(reason) else {
                continue;
            };
            if stop.source_revision == expected.source_revision
                && stop.configured_estimated_tokens == configured_budget
            {
                continue;
            }
            // A context recheck does not resolve an independent business wait or blocker.
            let _admission = self.execution_admission_lease()?;
            let Some(work) = self.inner.storage.latest_work_item(&id)? else {
                continue;
            };
            if work.agent_id != agent_id
                || work.state != crate::types::WorkItemState::Open
                || work.revision != expected.source_revision
                || work.plan_status != crate::types::WorkItemPlanStatus::Ready
                || work.blocked_by.is_some()
                || self
                    .inner
                    .storage
                    .raw_unresolved_wait_conditions_for_agent(&agent_id)?
                    .iter()
                    .any(|wait| wait.work_item_id.as_deref() == Some(id.as_str()))
            {
                continue;
            }
            let generation = expected
                .generation()
                .checked_add(1)
                .ok_or_else(|| anyhow!("WorkItem scheduling generation overflow"))?;
            let _outcome = self
                .inner
                .runtime_db
                .transitions()
                .commit_execution_protocol_recovery_plan(
                    &agent_id,
                    &[ExecutionProtocolCommand::RecheckContextBudgetStop(
                        Box::new(RecheckContextBudgetStop {
                            command_id: format!("context_budget_recheck:{id}:{generation}"),
                            work_item_id: id.clone(),
                            expected,
                            configured_estimated_tokens: configured_budget,
                        }),
                    )],
                    &[AuditEvent::legacy(
                        "context_budget_stop_recheck",
                        serde_json::json!({
                            "work_item_id": id,
                            "source_turn_id": stop.source_turn_id,
                            "configured_budget_estimated_tokens": configured_budget,
                            "source_revision": work.revision,
                        }),
                    )],
                )?;
        }
        Ok(())
    }
}
