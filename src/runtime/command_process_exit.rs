//! Only the original Command runner's successful wait/status path constructs this.
use super::super::control_cancellation::ControlJoinLease;
use crate::{
    runtime::{
        input_evidence_budget::{
            serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
            OwnedBytesEstimate,
        },
        resident_input_evidence::InputEvidenceScope,
    },
    system::{
        process::{RegisteredProcessGroupProbe, RegisteredProcessGroupQuiescence},
        RunningProcess, RunningProcessExitStatus,
    },
    types::TaskRecord,
};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

pub(in crate::runtime) struct OriginalCommandProcessExit {
    scope: InputEvidenceScope,
    lease_id: String,
    task_id: String,
    generation: serde_json::Value,
    code: Option<i32>,
    signal: Option<String>,
    group: Option<Arc<dyn RegisteredProcessGroupProbe>>,
    _material: Arc<BudgetViewGuard>,
}
impl OriginalCommandProcessExit {
    // Visibility is only the Command runner module, not arbitrary runtime callers.
    pub(super) fn from_successful_process_exit(
        scope: &InputEvidenceScope,
        task: &TaskRecord,
        lease: &ControlJoinLease,
        status: &RunningProcessExitStatus,
        process: &dyn RunningProcess,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Arc<Self>> {
        ensure!(
            task.kind == crate::types::TaskKind::CommandTask
                && task.agent_id == scope.agent_id
                && lease.is_command_producer()
                && lease.target_id() == task.id,
            "original Command process exit differs from registered lease"
        );
        let detail = task.detail.as_ref();
        let bytes = serialized_bytes(&(
            scope,
            lease.lease_id(),
            &task.id,
            lease.domain_generation(),
            task.created_at,
            detail.and_then(|d| d.get("rejoin_obligation_id")),
            detail.and_then(|d| d.get("rejoin_generation")),
            detail.and_then(|d| d.get("parent_turn_id")),
            status.code(),
            status.signal(),
        ))?
        .checked_add(512)
        .and_then(|n| n.checked_mul(4))
        .context("Command exit proof material overflow")?;
        let limits = ledger.limits();
        ensure!(
            limits.record_limit >= 4
                && bytes
                    <= limits
                        .history_per_agent_bytes
                        .checked_add(limits.pending_per_agent_bytes)
                        .context("Command exit proof limit overflow")?
                        .min(64 * 1024 * 1024),
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "Command exit proof"
            )
        );
        let material = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        ensure!(
            lease.matches_task_generation(task),
            "original Command process exit changed generation"
        );
        Ok(Arc::new(Self {
            scope: scope.clone(),
            lease_id: lease.lease_id().to_owned(),
            task_id: task.id.clone(),
            generation: lease.domain_generation().clone(),
            code: status.code(),
            signal: status.signal().map(str::to_owned),
            group: process.registered_group_probe(),
            _material: material,
        }))
    }
    pub(in crate::runtime) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(in crate::runtime) fn matches_lease(&self, lease: &ControlJoinLease) -> bool {
        self.lease_id == lease.lease_id()
            && self.task_id == lease.target_id()
            && &self.generation == lease.domain_generation()
    }
    pub(in crate::runtime) fn same_exit(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.lease_id == other.lease_id
            && self.task_id == other.task_id
            && self.generation == other.generation
            && self.code == other.code
            && self.signal == other.signal
    }
    pub(in crate::runtime) fn group_is_empty(&self) -> bool {
        self.group
            .as_ref()
            .is_some_and(|p| p.observe() == RegisteredProcessGroupQuiescence::Empty)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits,
        runtime::control_cancellation::{ControlCancellationRegistry, SharedControlSignal},
        system::{
            CaptureSpec, EffectiveExecution, ExecutionProfile, ExecutionScopeKind, LocalSystem,
            ProcessHost, ProcessPurpose, ProcessRequest, ProgramInvocation, StdioSpec,
            WorkspaceProjectionKind, WorkspaceView,
        },
        types::{TaskKind, TaskStatus},
    };
    use tokio::sync::oneshot;
    fn task() -> TaskRecord {
        TaskRecord {
            id: "original-command-exit".into(),
            agent_id: "original-owner".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: "2026-10-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-10-01T00:00:00Z".parse().unwrap(),
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: None,
            recovery: None,
        }
    }
    fn scope() -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "original-owner".into(),
            identity_incarnation: 1,
        }
    }
    fn execution() -> (tempfile::TempDir, EffectiveExecution) {
        let dir = tempfile::tempdir().unwrap();
        let view = WorkspaceView::new(
            None,
            dir.path().into(),
            dir.path().into(),
            dir.path().into(),
            None,
            None,
            WorkspaceProjectionKind::CanonicalRoot,
            None,
        )
        .unwrap();
        (
            dir,
            EffectiveExecution {
                profile: ExecutionProfile::default(),
                workspace: view,
                scope: ExecutionScopeKind::AgentTurn,
                attached_workspaces: vec![],
            },
        )
    }
    fn request() -> ProcessRequest {
        ProcessRequest {
            program: ProgramInvocation::Shell {
                command: "exit 0".into(),
                shell: Some("sh".into()),
                login: false,
            },
            cwd: None,
            env: vec![],
            stdin: StdioSpec::Null,
            tty: false,
            capture: CaptureSpec::NONE,
            timeout: None,
            purpose: ProcessPurpose::CommandTask,
        }
    }
    fn lease(registry: &ControlCancellationRegistry) -> Arc<ControlJoinLease> {
        let (cancel, _) = oneshot::channel();
        let (force, _) = oneshot::channel();
        registry.command(
            &task(),
            SharedControlSignal::new(cancel),
            SharedControlSignal::new(force),
        )
    }

    #[tokio::test]
    async fn actual_original_wait_and_empty_group_can_mint_closed_instance_proof() -> Result<()> {
        let (_dir, e) = execution();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let registry = ControlCancellationRegistry::default();
        let lease = lease(&registry);
        lease.attach(tokio::spawn(async {}));
        let mut process = LocalSystem::new().spawn(&e, request()).await?;
        let status = process.wait().await?;
        let fact = OriginalCommandProcessExit::from_successful_process_exit(
            &scope(),
            &task(),
            &lease,
            &status,
            process.as_ref(),
            &ledger,
        )?;
        lease.record_original_command_process_exit(fact)?;
        registry.cancel_local_producers();
        let snapshot = registry.capture_retiring_producers(&scope(), 7, &ledger)?;
        registry.drain_retiring().await?;
        let cap = snapshot
            .finish(&registry)?
            .context("actual exit and group empty must prove this closed instance")?;
        assert_eq!(cap.scope(), &scope());
        assert_eq!(cap.owner_epoch(), 7);
        assert!(cap.registered_groups_still_empty());
        assert!(ledger.report()?.observed_bytes > 0);
        drop((cap, lease, registry));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[tokio::test]
    async fn cancelled_monitor_without_actual_process_observation_never_proves_os_exit(
    ) -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let registry = ControlCancellationRegistry::default();
        let lease = lease(&registry);
        let handle = tokio::spawn(std::future::pending());
        let abort = handle.abort_handle();
        lease.attach(handle);
        abort.abort();
        registry.cancel_local_producers();
        let snapshot = registry.capture_retiring_producers(&scope(), 7, &ledger)?;
        registry.drain_retiring().await?;
        assert!(snapshot.finish(&registry)?.is_none());
        assert!(lease.original_command_process_exit().is_none());
        drop((lease, registry));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[tokio::test]
    async fn original_exit_rejects_changed_generation_and_late_registration_invalidates_frontier(
    ) -> Result<()> {
        let (_dir, e) = execution();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let registry = ControlCancellationRegistry::default();
        let lease = lease(&registry);
        lease.attach(tokio::spawn(async {}));
        let mut process = LocalSystem::new().spawn(&e, request()).await?;
        let status = process.wait().await?;
        let mut changed = task();
        changed.created_at += chrono::Duration::seconds(1);
        assert!(OriginalCommandProcessExit::from_successful_process_exit(
            &scope(),
            &changed,
            &lease,
            &status,
            process.as_ref(),
            &ledger
        )
        .is_err());
        assert!(lease.original_command_process_exit().is_none());
        lease.record_original_command_process_exit(
            OriginalCommandProcessExit::from_successful_process_exit(
                &scope(),
                &task(),
                &lease,
                &status,
                process.as_ref(),
                &ledger,
            )?,
        )?;
        registry.cancel_local_producers();
        let snapshot = registry.capture_retiring_producers(&scope(), 7, &ledger)?;
        let mut late = task();
        late.id = "late-original-registration".into();
        late.kind = TaskKind::AgentMessageWait;
        let original_late = registry.task(&late);
        original_late.attach(tokio::spawn(async {}));
        registry.drain_retiring().await?;
        assert!(
            snapshot.finish(&registry)?.is_none(),
            "a changed original frontier cannot be silently omitted"
        );
        drop((original_late, lease, registry));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
