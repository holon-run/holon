//! A restarted producer has no private seal for its old accepted Running input.
//! This read-only clock proof borrows selected qualified facts; metadata cannot
//! supply a timestamp or new Task/Message authority.
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};

use crate::runtime::{
    resident_input_evidence::{
        CapturedInputEvidence, InputEvidenceFamily, InputEvidenceKey, InputEvidenceReadError,
        PreparedInputEvidence, QualifiedInputEvidence,
    },
    resident_task::{
        CanonicalTaskFact, CapturedTasks, QualifiedTaskFact, TaskFamily, TaskKey, TaskReadError,
    },
};
use crate::runtime_db::task_snapshot::TaskSnapshotError;
use crate::types::{
    AdmissionContext, MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin,
    TaskKind, TaskRecord, TaskStatus,
};

/// Only the original private producer registry can mint this transient
/// association. No caller metadata or bare timestamp constructs it.
pub(super) struct OriginalRunningSeal(DateTime<Utc>);
impl OriginalRunningSeal {
    pub(super) fn observe(
        registry: &crate::runtime::control_cancellation::ControlCancellationRegistry,
        message: &MessageEnvelope,
    ) -> Result<Option<Self>> {
        Ok(registry
            .original_child_running_status_clock(message)?
            .map(Self))
    }
}

/// Not serializable or constructible from a caller Task DTO. Both borrowed
/// facts stay in their original selected capture until this synchronous use.
pub(super) struct AcceptedChildRunningClock<'a> {
    _message: &'a QualifiedInputEvidence,
    _task: &'a QualifiedTaskFact,
    created_at: DateTime<Utc>,
    clear_provenance_parent: bool,
    original_clock: bool,
}
impl AcceptedChildRunningClock<'_> {
    pub(super) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub(super) fn preserves_original_clock(&self) -> bool {
        self.original_clock
    }
    pub(super) fn preserve_rejoin_shape(&self, parsed: &mut TaskRecord) {
        if self.clear_provenance_parent {
            parsed
                .detail
                .as_mut()
                .and_then(serde_json::Value::as_object_mut)
                .expect("qualified original creator detail object")
                .remove("parent_turn_id");
        }
    }
}

pub(super) fn qualify<'a>(
    inputs: &'a CapturedInputEvidence,
    tasks: &'a CapturedTasks,
    message: &MessageEnvelope,
    parsed: &TaskRecord,
    seal: Option<&OriginalRunningSeal>,
) -> Result<Option<AcceptedChildRunningClock<'a>>> {
    if message.kind != MessageKind::TaskStatus
        || parsed.status != TaskStatus::Running
        || !(parsed.kind.is_child_agent()
            || matches!(
                parsed.kind,
                TaskKind::ActorInvocation | TaskKind::AgentMessageWait
            ))
    {
        return Ok(None);
    }
    ensure!(
        tasks.coverage.active_complete,
        TaskReadError::IncompleteActive
    );
    // An already terminal task is handled by the original phase monotonicity.
    // Do not expand the terminal window merely to preserve an ignored clock.
    if !tasks.coverage.active_task_ids.contains(&parsed.id) {
        return Ok(None);
    }
    ensure!(
        inputs.scope == tasks.scope,
        "Running clock Input/Task scopes differ"
    );
    let key = TaskKey::new(TaskFamily::Task, &parsed.id);
    let qualified = tasks
        .lookup(&key)?
        .ok_or_else(|| TaskReadError::NeedsPreparation(key.clone()))?;
    qualified.validate(&tasks.scope)?;
    let CanonicalTaskFact::Task(task) = &qualified.fact else {
        anyhow::bail!("Running clock selected a non-Task fact")
    };
    let source_key = InputEvidenceKey::new(InputEvidenceFamily::Message, &message.id);
    let source = inputs
        .lookup(&source_key)?
        .ok_or_else(|| InputEvidenceReadError::NeedsPreparation(source_key.clone()))?;
    ensure!(
        source.locator.scope == inputs.scope
            && source.locator.key == source_key
            && matches!(&source.record, PreparedInputEvidence::Message(actual) if actual == message),
        "Running clock differs from its actual accepted Input Message"
    );
    // The header supplies only expected equality. Missing legacy headers need
    // an original frozen association; the time always comes from actual Task.
    let expected_field = message
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("task_created_at"));
    let association = if let Some(expected) = expected_field {
        expected
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
            == Some(task.created_at)
    } else {
        seal.is_some_and(|seal| seal.0 == task.created_at)
            || tasks.fixed_running_generation_matches(qualified, source)?
    };
    ensure!(
        association,
        TaskSnapshotError::CanonicalChanged(key.clone())
    );
    fn field<'a>(
        detail: Option<&'a serde_json::Value>,
        name: &str,
    ) -> Option<&'a serde_json::Value> {
        detail.and_then(|value| value.get(name))
    }
    let original_detail = message
        .metadata
        .as_ref()
        .and_then(|value| value.get("task_detail"));
    let has_rejoin = ["rejoin_obligation_id", "rejoin_generation"]
        .into_iter()
        .any(|name| {
            field(original_detail, name).is_some() || field(task.detail.as_ref(), name).is_some()
        });
    let unbound = crate::runtime::tasks::unbound_task_rejoin_generation(task);
    // An accepted creator may allocate markers before any parent exists. Only
    // this exact qualified source can identify the parser's added provenance.
    let same_rejoin = !has_rejoin
        || ([
            "rejoin_obligation_id",
            "rejoin_generation",
            "parent_turn_id",
        ]
        .into_iter()
        .all(|name| field(original_detail, name) == field(task.detail.as_ref(), name))
            && if let Some(generation) = unbound {
                field(parsed.detail.as_ref(), "rejoin_obligation_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(task.id.as_str())
                    && field(parsed.detail.as_ref(), "rejoin_generation")
                        .and_then(serde_json::Value::as_u64)
                        == Some(generation)
                    && field(parsed.detail.as_ref(), "parent_turn_id").is_none()
                    && field(parsed.detail.as_ref(), "result_execution_turn_id")
                        .and_then(serde_json::Value::as_str)
                        == message.turn_id.as_deref()
            } else {
                task.rejoin_fence()
                    .ok()
                    .is_some_and(|fence| parsed.rejoin_fence().ok().as_ref() == Some(&fence))
            });
    ensure!(
        message.agent_id == tasks.scope.agent_id
            && task.agent_id == message.agent_id
            && parsed.agent_id == task.agent_id
            && task.id == parsed.id
            && task.kind == parsed.kind
            && message.task_id.as_deref() == Some(task.id.as_str())
            && matches!(&message.origin, MessageOrigin::Task { task_id } if task_id == &task.id)
            && message.delivery_surface == Some(MessageDeliverySurface::TaskRejoin)
            && message.admission_context == Some(AdmissionContext::RuntimeOwned)
            && message.work_item_id == task.work_item_id
            && parsed.work_item_id == task.work_item_id
            && parsed.recovery == task.recovery
            && same_rejoin,
        TaskSnapshotError::CanonicalChanged(key)
    );
    Ok(Some(AcceptedChildRunningClock {
        _message: source.as_ref(),
        _task: qualified.as_ref(),
        created_at: task.created_at,
        clear_provenance_parent: unbound.is_some(),
        original_clock: true,
    }))
}

#[cfg(test)]
#[path = "task_status_clock_tests.rs"]
mod tests;
