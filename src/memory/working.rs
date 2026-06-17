use anyhow::Result;

use crate::{
    storage::AppStorage,
    types::{AgentState, ClosureDecision, TodoItemState, WorkingMemorySnapshot},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingMemoryRefresh {
    pub previous_snapshot: WorkingMemorySnapshot,
    pub current_snapshot: WorkingMemorySnapshot,
    pub working_memory_updated: bool,
}

pub fn refresh_working_memory(
    storage: &AppStorage,
    agent: &mut AgentState,
    current_closure: &ClosureDecision,
) -> Result<WorkingMemoryRefresh> {
    let previous_snapshot =
        normalize_working_memory_snapshot(agent.working_memory.current_working_memory.clone());
    let next_snapshot = normalize_working_memory_snapshot(derive_working_memory_snapshot(
        storage,
        current_closure,
    )?);
    let scrubbed_legacy_fields = previous_snapshot != agent.working_memory.current_working_memory;
    if scrubbed_legacy_fields || previous_snapshot != next_snapshot {
        agent.working_memory.current_working_memory = next_snapshot.clone();
    }
    let cleared_stale_summary =
        !working_memory_snapshot_is_empty(&next_snapshot) && agent.context_summary.take().is_some();

    let working_memory_updated = cleared_stale_summary
        || scrubbed_legacy_fields
        || agent.working_memory.current_working_memory != previous_snapshot;

    Ok(WorkingMemoryRefresh {
        previous_snapshot,
        current_snapshot: next_snapshot,
        working_memory_updated,
    })
}

fn normalize_working_memory_snapshot(mut snapshot: WorkingMemorySnapshot) -> WorkingMemorySnapshot {
    snapshot.scope_hints.clear();
    snapshot.recent_decisions.clear();
    snapshot
}

fn working_memory_snapshot_is_empty(snapshot: &WorkingMemorySnapshot) -> bool {
    snapshot == &WorkingMemorySnapshot::default()
}

pub fn derive_working_memory_snapshot(
    storage: &AppStorage,
    current_closure: &ClosureDecision,
) -> Result<WorkingMemorySnapshot> {
    let projection = storage.work_queue_prompt_projection()?;
    let current_work_item = projection.current.as_ref();
    let active_waiting = storage
        .active_wait_conditions()?
        .into_iter()
        .filter(|record| record.work_item_id.is_some())
        .collect::<Vec<_>>();
    let current_work_item_id = current_work_item.map(|item| item.id.as_str());
    let waiting_on = active_waiting
        .iter()
        .filter(|record| record.work_item_id.as_deref() == current_work_item_id)
        .map(|record| {
            if let Some(subject_ref) = record.subject_ref.as_deref() {
                format!("{} on {}", record.waiting_for, subject_ref)
            } else {
                record.waiting_for.clone()
            }
        })
        .chain(current_closure.waiting_reason.map(|reason| {
            format!(
                "runtime: {}",
                serde_json::to_string(&reason)
                    .unwrap_or_else(|_| "\"waiting\"".to_string())
                    .trim_matches('"')
            )
        }))
        .take(4)
        .collect::<Vec<_>>();
    let pending_followups = current_work_item
        .map(|item| {
            item.todo_list
                .iter()
                .filter(|todo| todo.state != TodoItemState::Completed)
                .map(|todo| todo.text.clone())
                .take(6)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let working_set_files = current_work_item
        .map(|item| {
            item.work_refs
                .iter()
                .filter(|work_ref| work_ref.kind == crate::types::WorkItemRefKind::File)
                .filter(|work_ref| work_ref.status == crate::types::WorkItemRefStatus::Active)
                .map(|work_ref| work_ref.ref_id.clone())
                .take(8)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(WorkingMemorySnapshot {
        current_work_item_id: current_work_item.map(|item| item.id.clone()),
        objective: current_work_item.map(|item| item.objective.clone()),
        work_summary: current_work_item.map(|item| item.objective.clone()),
        plan: current_work_item
            .and_then(|item| item.plan_artifact.as_ref())
            .map(|artifact| artifact.preview.clone())
            .filter(|preview| !preview.trim().is_empty()),
        todo_list: current_work_item
            .map(|item| item.todo_list.clone())
            .unwrap_or_default(),
        working_set_files,
        pending_followups,
        waiting_on,
        ..WorkingMemorySnapshot::default()
    })
}
