//! Read-only assessment, not authority to reopen a damaged execution.

use super::*;
use crate::domain::execution_protocol::{
    ExecutionOutcome, ExecutionSourceIdentity, WorkItemExecutionState, WorkItemOutcome,
};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WorkItemRepairAssessment {
    pub work_item_id: String,
    pub durable_revision: Option<u64>,
    pub source_revision: u64,
    pub generation: u64,
    pub repair_id: String,
    pub original_attempt_id: Option<String>,
    pub message_id: Option<String>,
    pub wait_id: Option<String>,
    pub queue_status: Option<QueueEntryStatus>,
    pub wait_status: Option<crate::types::WaitConditionStatus>,
    pub admitted_revision: Option<u64>,
    pub original_source_restorable: bool,
    pub refusal_reasons: Vec<String>,
    pub next_action: &'static str,
}

pub(crate) fn work_item_repair_assessments(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
) -> Result<Vec<WorkItemRepairAssessment>> {
    let Some(protocol) = runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized(agent_id)?
    else {
        return Ok(Vec::new());
    };
    let mut assessments = Vec::new();
    for (work_item_id, execution) in &protocol.work_items {
        let WorkItemExecutionState::NeedsRepair {
            generation,
            repair_id,
        } = &execution.state
        else {
            continue;
        };
        let work = runtime_db.work_items().latest(work_item_id)?;
        let attempts = protocol
            .outcomes
            .values()
            .filter(|outcome| {
                matches!(
                        &outcome.outcome,
                        ExecutionOutcome::WorkItem(WorkItemOutcome::NeedsRepair { repair_id: id })
                            if id == repair_id
                    )
            })
            .filter_map(|outcome| protocol.attempts.get(&outcome.attempt_id))
            .filter(|attempt| matches!(
                &attempt.binding,
                crate::domain::execution_protocol::ExecutionBinding::WorkItem { work_item_id: id }
                    if id == work_item_id
            ))
            .collect::<Vec<_>>();
        let attempt = match attempts.as_slice() {
            [attempt] => Some(*attempt),
            _ => None,
        };
        let message_id = attempt.and_then(|attempt| attempt.source_message_id.clone());
        let wait_id = attempt.and_then(|attempt| match &attempt.source.identity {
            ExecutionSourceIdentity::TriggeredWait { wait_id, .. } => Some(wait_id.clone()),
            _ => None,
        });
        let queue_status = message_id
            .as_deref()
            .map(|id| runtime_db.queue_entries().latest(id))
            .transpose()?
            .flatten()
            .map(|entry| entry.status);
        let wait_status = if let Some(wait_id) = wait_id.as_deref() {
            storage
                .latest_wait_conditions_for_agent(agent_id)?
                .into_iter()
                .find(|wait| wait.id == wait_id && wait.work_item_id.as_ref() == Some(work_item_id))
                .map(|wait| wait.status)
        } else {
            None
        };
        let mut refusal_reasons = Vec::new();
        if !work
            .as_ref()
            .is_some_and(|work| work.agent_id == agent_id && work.state == WorkItemState::Open)
        {
            refusal_reasons.push("owner_missing_or_not_open".into());
        }
        if attempt.is_none() {
            refusal_reasons.push("original_attempt_missing_or_ambiguous".into());
        }
        if !matches!(
            queue_status,
            Some(QueueEntryStatus::Queued | QueueEntryStatus::Interrupted)
        ) {
            refusal_reasons.push("original_source_not_retained".into());
        }
        if !matches!(
            wait_status,
            Some(crate::types::WaitConditionStatus::Triggered)
        ) {
            refusal_reasons.push("original_exact_wait_not_triggered".into());
        }
        // A last-send dispatch=false marker is not proof about earlier sends,
        // tools, or consumption. Current ledgers do not attest a repairable
        // revision-only gap; never infer that proof from missing rows.
        refusal_reasons.push("revision_only_gap_and_zero_side_effects_not_proven".into());
        assessments.push(WorkItemRepairAssessment {
            work_item_id: work_item_id.clone(),
            durable_revision: work.as_ref().map(|work| work.revision),
            source_revision: execution.source_revision,
            generation: *generation,
            repair_id: repair_id.clone(),
            original_attempt_id: attempt.map(|attempt| attempt.attempt_id.clone()),
            message_id,
            wait_id,
            queue_status,
            wait_status,
            admitted_revision: attempt
                .and_then(|attempt| attempt.admitted_fences.work_item_source_revision),
            original_source_restorable: false,
            refusal_reasons,
            next_action: "trusted_new_continuation_required_do_not_replay_original",
        });
    }
    Ok(assessments)
}
