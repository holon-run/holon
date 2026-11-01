//! Admission checks shared by parent cleanup and idle runtime retirement.
//!
//! These queries run in the deletion admission transaction. Producers check
//! the durable fence in the same transaction as their new work/reference.

use anyhow::{bail, Result};
use rusqlite::{OptionalExtension, Transaction};

/// Existing operator/one-shot deletion retains its cancellation semantics.
/// Parent cleanup closes new admission without cancelling accepted work.
pub(crate) fn ensure_work_admission_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<()> {
    let fenced: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_deletion_jobs j
         JOIN agent_identities i ON i.agent_id = j.agent_id
         WHERE j.agent_id = ?1 AND i.status != 'active'
           AND json_extract(j.payload_json, '$.mode') = 'parent_cleanup')",
        [agent_id],
        |row| row.get(0),
    )?;
    if fenced {
        bail!("agent_cleanup_fenced: agent {agent_id} is being deleted");
    }
    Ok(())
}

/// A root cannot gain new users or change ownership while its owner is fenced.
pub(crate) fn ensure_root_admission_tx(tx: &Transaction<'_>, root_id: &str) -> Result<()> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM execution_root_entries WHERE execution_root_id = ?1",
            [root_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(payload) = payload {
        let root: crate::types::ExecutionRootEntry = serde_json::from_str(&payload)?;
        if let Some(worktree) = root.worktree {
            for agent_id in worktree
                .authorized_agent_ids
                .iter()
                .chain(worktree.registered_by_agent_id.iter())
            {
                ensure_work_admission_tx(tx, agent_id)?;
            }
        }
    }
    Ok(())
}

/// Durable state that proves live execution or scheduling. These block both
/// deletion admission and reminder delivery because the agent is not idle.
const EXECUTION_BLOCKER_CHECKS: &[(&str, &str)] = &[
    ("active_task", "SELECT EXISTS(SELECT 1 FROM tasks WHERE owner_agent_id = ?1 AND status IN ('queued','running','cancelling'))"),
    ("inbound_invocation", "SELECT EXISTS(SELECT 1 FROM tasks WHERE child_agent_id = ?1 AND status IN ('queued','running','cancelling'))"),
    ("queued_or_claimed_input", "SELECT EXISTS(SELECT 1 FROM queue_entries WHERE agent_id = ?1 AND status IN ('queued','dequeued','interrupted'))"),
    ("accepted_delivery", "SELECT EXISTS(SELECT 1 FROM agent_message_deliveries WHERE target_agent_id = ?1 AND state IN ('queued','dispatched'))"),
    ("wait", "SELECT EXISTS(SELECT 1 FROM wait_conditions WHERE agent_id = ?1 AND status IN ('active','triggered'))"),
    ("pending_result_settlement", "SELECT EXISTS(SELECT 1 FROM task_result_settlements WHERE agent_id = ?1 AND state != 'settled')"),
    ("open_execution", "SELECT EXISTS(SELECT 1 FROM execution_protocol_attempts WHERE agent_id = ?1 AND lifecycle_state = 'open')"),
    ("workspace_occupancy", "SELECT EXISTS(SELECT 1 FROM workspace_occupancies WHERE holder_agent_id = ?1 AND released_at IS NULL)"),
];

/// Durable retention state that outlives the current turn. The parent owns the
/// decision to keep or release it, so it is a diagnostic hint rather than a
/// blocker for deletion admission or reminder delivery.
const RETENTION_BLOCKER_CHECKS: &[(&str, &str)] = &[
    ("open_work_item", "SELECT EXISTS(SELECT 1 FROM work_items WHERE agent_id = ?1 AND state != 'completed')"),
    ("timer", "SELECT EXISTS(SELECT 1 FROM timers WHERE agent_id = ?1 AND status = 'active')"),
    ("timer_wake", "SELECT EXISTS(SELECT 1 FROM timers t JOIN timer_wakes w USING(timer_id) WHERE t.agent_id = ?1 AND w.status = 'pending')"),
];

/// Fail closed on unknown state. Historical rows alone are not live work.
/// `last_runtime_failure` is a diagnostic and is intentionally not an execution
/// obligation: a settled idle agent remains deletable and idle even if the last
/// turn failed and no later turn succeeded.
fn runtime_state_blocker_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Option<String>> {
    let state: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(state) = state else {
        return Ok(Some("unknown_runtime_state".into()));
    };
    let state: crate::types::AgentState = serde_json::from_str(&state)?;
    if !matches!(
        state.status,
        crate::types::AgentStatus::AwakeIdle
            | crate::types::AgentStatus::Asleep
            | crate::types::AgentStatus::Stopped
    ) || state.current_run_id.is_some()
        || state.pending > 0
        || state.sleeping_until.is_some()
        || state.pending_wake_hint.is_some()
    {
        return Ok(Some("runtime_execution_or_wake".into()));
    }
    Ok(None)
}

// Each query uses an owner/target index. Keep these predicates aligned with
// producer fencing; a caller must never make work idle by cancelling it.
fn first_blocker_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
    checks: &[(&str, &str)],
) -> Result<Option<String>> {
    for (reason, sql) in checks {
        if tx.query_row(sql, [agent_id], |row| row.get::<_, bool>(0))? {
            return Ok(Some((*reason).into()));
        }
    }
    Ok(None)
}

/// Full idle safety facts. Used by idle runtime retirement, which must keep
/// considering retention state before exiting a runtime.
pub(crate) fn idle_blocker_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Option<String>> {
    if let Some(blocker) = runtime_state_blocker_tx(tx, agent_id)? {
        return Ok(Some(blocker));
    }
    if let Some(blocker) = first_blocker_tx(tx, agent_id, EXECUTION_BLOCKER_CHECKS)? {
        return Ok(Some(blocker));
    }
    first_blocker_tx(tx, agent_id, RETENTION_BLOCKER_CHECKS)
}

/// Execution-only blockers: live runtime state plus active, queued, or waiting
/// work. Retention state (open work items, timers, timer wakes) is excluded so
/// callers can treat it as a hint instead of a hard stop.
pub(crate) fn execution_blocker_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<Option<String>> {
    if let Some(blocker) = runtime_state_blocker_tx(tx, agent_id)? {
        return Ok(Some(blocker));
    }
    first_blocker_tx(tx, agent_id, EXECUTION_BLOCKER_CHECKS)
}

/// Deletion admission for a supervised child. Execution blockers, descendant
/// responsibility, and protected artifacts block cleanup. Retention state
/// (open work items, timers) and retained command-task output do not: the parent
/// decides whether they are still needed, and deletion later terminalizes or
/// cancels them through the normal phases.
pub(crate) fn parent_cleanup_blocker_tx(
    tx: &Transaction<'_>,
    agent_id: &str,
) -> Result<Option<String>> {
    if let Some(blocker) = execution_blocker_tx(tx, agent_id)? {
        return Ok(Some(blocker));
    }
    for (reason, sql) in [
        ("descendant_responsibility", "SELECT EXISTS(SELECT 1 FROM agent_supervisions WHERE supervisor_agent_id = ?1 AND state != 'closed')"),
        ("protected_artifact", "SELECT EXISTS(SELECT 1 FROM artifact_metadata WHERE agent_id = ?1)"),
    ] {
        if tx.query_row(sql, [agent_id], |row| row.get::<_, bool>(0))? { return Ok(Some(reason.into())); }
    }
    Ok(None)
}

pub(crate) fn retirable_child_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<bool> {
    use crate::types::*;
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id = ?1 AND status = 'active'",
            [agent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let identity: AgentIdentityRecord = serde_json::from_str(&payload)?;
    if identity.kind != AgentKind::Child
        || identity.visibility != AgentVisibility::Private
        || identity.ownership() != AgentOwnership::ParentSupervised
    {
        return Ok(false);
    }
    let relations = super::agent_relations::canonical_relations_from_connection(tx, agent_id)?;
    Ok(relations.is_some_and(|r| {
        r.resolution == AgentCanonicalResolution::Resolved
            && r.durability
                .is_some_and(|d| d.durability == AgentCanonicalDurability::Ephemeral)
            && r.lifecycle_attachment
                .is_some_and(|a| a.attachment == AgentLifecycleAttachment::SupervisionAttached)
            && r.supervision.is_some_and(|s| {
                matches!(
                    s.state,
                    AgentSupervisionState::Active | AgentSupervisionState::CleanupRequired
                )
            })
    }))
}

pub(crate) fn idle_evidence_tx(tx: &Transaction<'_>, agent_id: &str) -> Result<String> {
    let (incarnation, state): (u64, String) = tx.query_row("SELECT COALESCE(json_extract(i.payload_json, '$.incarnation'), 1), COALESCE(s.payload_json, 'null') FROM agent_identities i LEFT JOIN agent_states s USING(agent_id) WHERE i.agent_id = ?1", [agent_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let state: Option<crate::types::AgentState> = serde_json::from_str(&state)?;
    let Some(state) = state else {
        return Ok(serde_json::to_string(&(
            incarnation,
            "unknown_runtime_state",
        ))?);
    };
    let latest_task: Option<String> = tx.query_row(
        "SELECT MAX(updated_at) FROM tasks WHERE owner_agent_id = ?1 OR child_agent_id = ?1",
        [agent_id],
        |row| row.get(0),
    )?;
    Ok(serde_json::to_string(&(
        incarnation,
        state.turn_index,
        state.total_message_count,
        state.last_turn_terminal,
        latest_task,
    ))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_db::RuntimeDb;
    use crate::types::{
        AgentState, AgentStatus, PendingWakeHint, QueueEntryRecord, QueueEntryStatus,
        RuntimeFailurePhase, RuntimeFailureSummary, TaskKind, TaskRecord, TaskStatus,
        TurnTerminalKind, TurnTerminalRecord, WorkItemRecord, WorkItemState,
    };
    use chrono::Utc;

    fn open_db() -> (tempfile::TempDir, RuntimeDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("runtime.sqlite"),
            dir.path().join("runtime.lock"),
        )
        .unwrap();
        (dir, db)
    }

    fn failure(summary: &str) -> RuntimeFailureSummary {
        RuntimeFailureSummary {
            occurred_at: Utc::now(),
            summary: summary.to_string(),
            phase: RuntimeFailurePhase::RuntimeTurn,
            detail_hint: Some("historical diagnostic".into()),
            failure_artifact: None,
        }
    }

    fn terminal_record(kind: TurnTerminalKind) -> TurnTerminalRecord {
        TurnTerminalRecord {
            turn_id: "turn-1".into(),
            turn_index: 1,
            kind,
            reason: Some("fixture".into()),
            last_assistant_message: None,
            no_brief_reason: None,
            checkpoint: None,
            completed_at: Utc::now(),
            duration_ms: 1,
        }
    }

    fn settled(agent_id: &str, status: AgentStatus, kind: TurnTerminalKind) -> AgentState {
        let mut state = AgentState::new(agent_id);
        state.status = status;
        state.last_turn_terminal = Some(terminal_record(kind));
        state.last_runtime_failure = Some(failure("provider_dispatch_rejected"));
        state
    }

    fn blockers(
        db: &RuntimeDb,
        agent_id: &str,
    ) -> (Option<String>, Option<String>, Option<String>) {
        db.transaction(|tx| {
            Ok((
                execution_blocker_tx(tx, agent_id)?,
                idle_blocker_tx(tx, agent_id)?,
                parent_cleanup_blocker_tx(tx, agent_id)?,
            ))
        })
        .unwrap()
    }

    #[test]
    fn historical_failure_is_not_an_execution_obligation() {
        let (_dir, db) = open_db();
        assert_eq!(
            blockers(&db, "missing").0.as_deref(),
            Some("unknown_runtime_state")
        );

        for (agent_id, status, kind) in [
            (
                "failed-then-ok",
                AgentStatus::AwakeIdle,
                TurnTerminalKind::Completed,
            ),
            (
                "last-failed-idle",
                AgentStatus::AwakeIdle,
                TurnTerminalKind::Aborted,
            ),
            (
                "last-failed-asleep",
                AgentStatus::Asleep,
                TurnTerminalKind::ProviderFailedNeedsRecovery,
            ),
            (
                "last-failed-stopped",
                AgentStatus::Stopped,
                TurnTerminalKind::Interrupted,
            ),
        ] {
            let state = settled(agent_id, status, kind);
            let diagnostic = state.last_runtime_failure.clone();
            db.agent_states().upsert_for_fixture(&state).unwrap();
            assert_eq!(blockers(&db, agent_id), (None, None, None), "{agent_id}");
            assert_eq!(
                db.agent_states()
                    .latest(agent_id)
                    .unwrap()
                    .unwrap()
                    .last_runtime_failure,
                diagnostic,
                "diagnostic must survive admission"
            );
        }

        let mut running = settled(
            "running",
            AgentStatus::AwakeRunning,
            TurnTerminalKind::Aborted,
        );
        db.agent_states().upsert_for_fixture(&running).unwrap();
        assert_eq!(
            blockers(&db, "running").0.as_deref(),
            Some("runtime_execution_or_wake")
        );

        running.id = "run-id".into();
        running.status = AgentStatus::AwakeIdle;
        running.current_run_id = Some("run-1".into());
        db.agent_states().upsert_for_fixture(&running).unwrap();
        assert_eq!(
            blockers(&db, "run-id").0.as_deref(),
            Some("runtime_execution_or_wake")
        );

        let mut pending = settled("pending", AgentStatus::Asleep, TurnTerminalKind::Aborted);
        pending.pending = 1;
        db.agent_states().upsert_for_fixture(&pending).unwrap();
        assert_eq!(
            blockers(&db, "pending").0.as_deref(),
            Some("runtime_execution_or_wake")
        );

        let mut sleeping = settled("sleeping", AgentStatus::Stopped, TurnTerminalKind::Aborted);
        sleeping.sleeping_until = Some(Utc::now());
        db.agent_states().upsert_for_fixture(&sleeping).unwrap();
        assert_eq!(
            blockers(&db, "sleeping").0.as_deref(),
            Some("runtime_execution_or_wake")
        );

        let mut hinted = settled("hinted", AgentStatus::AwakeIdle, TurnTerminalKind::Aborted);
        hinted.pending_wake_hint = Some(PendingWakeHint {
            reason: "external".into(),
            description: None,
            source: None,
            scope: None,
            external_trigger_id: None,
            resource: None,
            body: None,
            content_type: None,
            correlation_id: None,
            causation_id: None,
            created_at: Utc::now(),
        });
        db.agent_states().upsert_for_fixture(&hinted).unwrap();
        assert_eq!(
            blockers(&db, "hinted").0.as_deref(),
            Some("runtime_execution_or_wake")
        );

        let retained = settled(
            "retained",
            AgentStatus::AwakeIdle,
            TurnTerminalKind::Aborted,
        );
        db.agent_states().upsert_for_fixture(&retained).unwrap();
        db.work_items()
            .insert_new(&WorkItemRecord::new(
                "retained",
                "still open",
                WorkItemState::Open,
            ))
            .unwrap();
        let (execution, idle, cleanup) = blockers(&db, "retained");
        assert_eq!(execution, None);
        assert_eq!(idle.as_deref(), Some("open_work_item"));
        assert_eq!(cleanup, None);

        let live = settled(
            "live-task",
            AgentStatus::AwakeIdle,
            TurnTerminalKind::Aborted,
        );
        db.agent_states().upsert_for_fixture(&live).unwrap();
        let now = Utc::now();
        let task = TaskRecord {
            id: "live-task-1".into(),
            agent_id: "live-task".into(),
            kind: TaskKind::ChildAgentTask,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        };
        db.tasks().upsert(&task).unwrap();
        assert_eq!(blockers(&db, "live-task").0.as_deref(), Some("active_task"));
        assert_eq!(
            db.tasks().latest(&task.id).unwrap().unwrap().status,
            TaskStatus::Running
        );

        let queued_agent = settled("queued", AgentStatus::Asleep, TurnTerminalKind::Aborted);
        db.agent_states().upsert_for_fixture(&queued_agent).unwrap();
        let queued = QueueEntryRecord {
            message_id: "queued-1".into(),
            agent_id: "queued".into(),
            priority: crate::types::Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: now,
            updated_at: now,
        };
        db.queue_entries().upsert(&queued).unwrap();
        assert_eq!(
            blockers(&db, "queued").0.as_deref(),
            Some("queued_or_claimed_input")
        );
        assert_eq!(
            db.queue_entries()
                .latest(&queued.message_id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Queued
        );
    }
}
