//! Monitor Task facts are selected from the child publication, never raw history.
use super::*;
use super::{
    input_evidence_budget::{BudgetViewGuard, InputMemoryCategory, OwnedBytesEstimate},
    resident_task::{CapturedTasks, TaskParentTurnHeader, TaskReadError},
    state_mutation::PreparedSchedulerRead,
    task_queries::{TaskQueryLease, TaskQuerySpec},
};
use crate::types::WaitConditionKind;
use std::collections::BTreeMap;

pub(crate) struct ChildMonitorRead {
    read: PreparedSchedulerRead,
}
impl ChildMonitorRead {
    pub(crate) fn state(&self) -> &AgentState {
        &self.read.state
    }
    pub(crate) fn closure(&self) -> crate::types::ClosureDecision {
        RuntimeHandle::closure_decision_from_projection(
            &self.read.state,
            &self.read.projection,
            None,
        )
    }
    pub(crate) fn has_lifecycle_blockers(&self) -> Result<bool> {
        if self.read.projection.active_tasks.iter().any(|task| {
            task.kind != TaskKind::ChildAgentTask
                && matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                )
        }) {
            return Ok(true);
        }
        anyhow::ensure!(
            self.read.work.live_complete,
            super::resident_work::WorkReadError::IncompleteLive
        );
        // Preserve the original raw Active Task-Wait predicate, including waits
        // whose Work owner is no longer Open; the context queue filter differs.
        for id in &self.read.work.unresolved_wait_ids {
            let wait = self
                .read
                .work
                .wait_condition(id)?
                .context("monitor Wait body unavailable")?;
            anyhow::ensure!(
                matches!(
                    wait.status,
                    WaitConditionStatus::Active | WaitConditionStatus::Triggered
                ),
                "monitor unresolved Wait is terminal"
            );
            if wait.status == WaitConditionStatus::Active && wait.kind == WaitConditionKind::Task {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The caller owns this live role across polling; it retains no Runtime/root.
pub(crate) struct TaskParentTurnSession {
    lease: TaskQueryLease,
    _slot_budget: Arc<BudgetViewGuard>,
}
impl TaskParentTurnSession {
    pub(crate) fn turn_id(&self) -> &str {
        self.lease
            .selection()
            .parent_turn_ids
            .first()
            .expect("one original parent Turn role")
    }
}
pub(crate) struct TaskParentTurnRead {
    captured: CapturedTasks,
    head_index: usize,
}
impl TaskParentTurnRead {
    pub(crate) fn headers(&self) -> &BTreeMap<String, TaskParentTurnHeader> {
        self.captured
            .coverage
            .query
            .parent_turn_tasks
            .values()
            .nth(self.head_index)
            .expect("validated immutable parent Turn head")
    }
}
impl RuntimeHandle {
    pub(crate) async fn child_monitor_read(
        &self,
        expected_incarnation: u64,
    ) -> Result<ChildMonitorRead> {
        anyhow::ensure!(
            self.inner.identity_incarnation == expected_incarnation,
            "child monitor incarnation changed"
        );
        let read = self.capture_child_observability_projection().await?;
        anyhow::ensure!(
            read.state.id == self.instance_agent_id()
                && read.tasks.scope.identity_incarnation == expected_incarnation
                && read.tasks.scope == read.work.scope,
            "child monitor publication scope differs"
        );
        Ok(ChildMonitorRead { read })
    }

    pub(crate) async fn parent_turn_session_is_current(
        &self,
        session: &TaskParentTurnSession,
    ) -> Result<bool> {
        let _reservation = self.inner.mutation_owner.reserve().await?;
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("monitor Task root is unpublished")?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            tasks.scope().agent_id == self.instance_agent_id()
                && tasks.scope().identity_incarnation == self.inner.identity_incarnation
                && root.token.identity_incarnation == self.inner.identity_incarnation,
            "monitor Task role scope differs"
        );
        Ok(self
            .inner
            .task_queries
            .validate_lease(&session.lease, tasks.scope(), root.token.owner_epoch)
            .is_ok())
    }

    /// A newly observed protocol Turn is an explicit finite metadata acquisition.
    /// Repeated polls use capture_parent_turn_tasks and cannot prepare again.
    pub(crate) async fn begin_parent_turn_task_session(
        &self,
        expected_incarnation: u64,
        turn: &str,
    ) -> Result<TaskParentTurnSession> {
        anyhow::ensure!(
            self.inner.identity_incarnation == expected_incarnation,
            "monitor Task role incarnation changed"
        );
        let slot_budget = self
            .with_task_publication(|| {
                // A missing/retired Task view cannot be replaced by a historical read.
                let root = self
                    .inner
                    .mutation_owner
                    .capture()?
                    .context("monitor Task root is unpublished")?;
                let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
                anyhow::ensure!(
                    tasks.scope().agent_id == self.instance_agent_id()
                        && tasks.scope().identity_incarnation == expected_incarnation,
                    "monitor Task role scope differs"
                );
                let limits = self.inner.input_evidence_budget.limits();
                let bytes = std::mem::size_of::<TaskParentTurnSession>()
                    .checked_add(256)
                    .and_then(|n| n.checked_mul(8))
                    .context("monitor role slots overflow")?;
                let max_bytes = limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("monitor role limits overflow")?;
                anyhow::ensure!(
                    u64::try_from(bytes)? <= max_bytes && limits.record_limit >= 8,
                    TaskReadError::ResourceExhausted("monitor role slots")
                );
                // Eight slots cover the caller's Vec growth before inserting the role.
                Ok(self.inner.input_evidence_budget.register_owned_bytes(
                    tasks.scope(),
                    InputMemoryCategory::Pins,
                    OwnedBytesEstimate {
                        payload_bytes: 0,
                        metadata_estimate_bytes: u64::try_from(bytes)?,
                    },
                )?)
            })
            .await?;
        let mut owner = self.begin_mutation().await?;
        let lease = self.register_task_query(TaskQuerySpec {
            parent_turn_ids: &[turn],
            ..Default::default()
        })?;
        owner.prepare_owner_task_query(&lease).await?;
        Ok(TaskParentTurnSession {
            lease,
            _slot_budget: slot_budget,
        })
    }

    pub(crate) async fn capture_parent_turn_tasks(
        &self,
        expected_incarnation: u64,
        session: &TaskParentTurnSession,
    ) -> Result<TaskParentTurnRead> {
        anyhow::ensure!(
            self.inner.identity_incarnation == expected_incarnation,
            "monitor Task capture incarnation changed"
        );
        let reservation = self.inner.mutation_owner.reserve().await?;
        let root = reservation
            .capture_ready_snapshot()?
            .context("monitor Task root is unpublished")?;
        let tasks = root.tasks.as_ref().ok_or(TaskReadError::IncompleteActive)?;
        anyhow::ensure!(
            root.state.id == self.instance_agent_id()
                && tasks.scope().agent_id == root.state.id
                && tasks.scope().identity_incarnation == expected_incarnation
                && root.token.identity_incarnation == expected_incarnation
                && root.token.owner_epoch == reservation.owner_epoch(),
            "monitor Task publication scope differs"
        );
        self.inner.task_queries.validate_lease(
            &session.lease,
            tasks.scope(),
            root.token.owner_epoch,
        )?;
        let captured = tasks.capture_query(session.lease.selection())?;
        captured.parent_turn_task_headers(session.turn_id())?;
        let head_index = captured
            .coverage
            .query
            .parent_turn_tasks
            .keys()
            .position(|turn| turn == session.turn_id())
            .context("selected parent Turn head disappeared")?;
        Ok(TaskParentTurnRead {
            captured,
            head_index,
        })
    }
}
