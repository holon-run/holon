//! One original spawn responsibility, including foreground exec commands.
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
        RunningProcess,
    },
};
use anyhow::{anyhow, ensure, Result};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

pub(in crate::runtime) struct OriginalManagedProcess {
    scope: InputEvidenceScope,
    token: String,
    attached: AtomicBool,
    successful_exit: AtomicBool,
    group: Mutex<Option<Arc<dyn RegisteredProcessGroupProbe>>>,
    material_bytes: u64,
    _material: Arc<BudgetViewGuard>,
}
impl OriginalManagedProcess {
    // Only the Command runner can allocate or complete this responsibility.
    pub(super) fn before_spawn(
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Arc<Self>> {
        let bytes = serialized_bytes(scope)?
            .checked_add(1024)
            .ok_or_else(|| anyhow!("original process material overflow"))?;
        let material = ledger.register_owned_bytes(
            scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        Ok(Arc::new(Self {
            scope: scope.clone(),
            token: uuid::Uuid::new_v4().to_string(),
            attached: AtomicBool::new(false),
            successful_exit: AtomicBool::new(false),
            group: Mutex::new(None),
            material_bytes: bytes,
            _material: material,
        }))
    }
    pub(super) fn attach_original_handle(&self, process: &dyn RunningProcess) -> Result<()> {
        let mut group = self
            .group
            .lock()
            .map_err(|_| anyhow!("original process group poisoned"))?;
        ensure!(
            !self.attached.swap(true, Ordering::AcqRel),
            "original process handle attached twice"
        );
        *group = process.registered_group_probe();
        Ok(())
    }
    pub(super) fn record_successful_exit(&self) {
        if self.attached.load(Ordering::Acquire) {
            self.successful_exit.store(true, Ordering::Release);
        }
    }
    pub(in crate::runtime) fn scope(&self) -> &InputEvidenceScope {
        &self.scope
    }
    pub(in crate::runtime) fn token(&self) -> &str {
        &self.token
    }
    pub(in crate::runtime) fn material_bytes(&self) -> u64 {
        self.material_bytes
    }
    pub(in crate::runtime) fn group_is_empty(&self) -> bool {
        self.successful_exit.load(Ordering::Acquire)
            && self.group.lock().ok().is_some_and(|group| {
                group
                    .as_ref()
                    .is_some_and(|p| p.observe() == RegisteredProcessGroupQuiescence::Empty)
            })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits,
        runtime::control_cancellation::ControlCancellationRegistry,
        system::{
            CaptureSpec, EffectiveExecution, ExecutionProfile, ExecutionScopeKind, LocalSystem,
            ProcessHost, ProcessPurpose, ProcessRequest, ProgramInvocation, StdioSpec, StopSignal,
            WorkspaceProjectionKind, WorkspaceView,
        },
    };
    fn scope() -> InputEvidenceScope {
        InputEvidenceScope {
            agent_id: "original-foreground".into(),
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
    fn request(cmd: &str) -> ProcessRequest {
        ProcessRequest {
            program: ProgramInvocation::Shell {
                command: cmd.into(),
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
    #[tokio::test]
    async fn foreground_without_task_wait_and_empty_group_is_in_original_drain() -> Result<()> {
        let (_dir, execution) = execution();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let registry = ControlCancellationRegistry::default();
        let original = OriginalManagedProcess::before_spawn(&scope(), &ledger)?;
        registry.retain_managed_process(original.clone(), &ledger)?;
        let mut process = LocalSystem::new()
            .spawn(&execution, request("exit 0"))
            .await?;
        original.attach_original_handle(process.as_ref())?;
        assert!(
            !original.group_is_empty(),
            "a probe alone does not establish foreground exit"
        );
        process.wait().await?;
        original.record_successful_exit();
        assert!(original.group_is_empty());
        registry.cancel_local_producers();
        let snapshot = registry.capture_retiring_producers(&scope(), 9, &ledger)?;
        registry.drain_retiring().await?;
        let cap = snapshot
            .finish(&registry)?
            .expect("original foreground is complete");
        assert!(cap.registered_groups_still_empty());
        drop((cap, original, registry));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[tokio::test]
    async fn foreground_exit_with_background_samegroup_is_retained_without_task_lease() -> Result<()>
    {
        let (_dir, execution) = execution();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let registry = ControlCancellationRegistry::default();
        let original = OriginalManagedProcess::before_spawn(&scope(), &ledger)?;
        registry.retain_managed_process(original.clone(), &ledger)?;
        let mut process = LocalSystem::new()
            .spawn(&execution, request("sleep 30 & exit 0"))
            .await?;
        original.attach_original_handle(process.as_ref())?;
        process.wait().await?;
        original.record_successful_exit();
        assert!(!original.group_is_empty());
        // A subsequent command must not reap the still-live original group.
        let later = OriginalManagedProcess::before_spawn(&scope(), &ledger)?;
        registry.retain_managed_process(later.clone(), &ledger)?;
        registry.cancel_local_producers();
        let snapshot = registry.capture_retiring_producers(&scope(), 9, &ledger)?;
        registry.drain_retiring().await?;
        assert!(snapshot.finish(&registry)?.is_none());
        process.stop(StopSignal::Force).await?; // only this test's real group
        drop((original, later, registry));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[tokio::test]
    async fn cancelled_spawn_never_becomes_empty_absence() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let registry = ControlCancellationRegistry::default();
        let original = OriginalManagedProcess::before_spawn(&scope(), &ledger)?;
        registry.retain_managed_process(original.clone(), &ledger)?;
        // No handle returned (cancelled spawn or post-fork error). Even an
        // erroneously repeated completion notification cannot create a probe.
        original.record_successful_exit();
        assert!(!original.group_is_empty());
        registry.cancel_local_producers();
        let snapshot = registry.capture_retiring_producers(&scope(), 9, &ledger)?;
        registry.drain_retiring().await?;
        assert!(snapshot.finish(&registry)?.is_none());
        drop((original, registry));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    struct UnsupportedProcess;
    #[async_trait::async_trait]
    impl RunningProcess for UnsupportedProcess {
        fn id(&self) -> String {
            "unsupported-test".into()
        }
        fn take_stdout(&mut self) -> Option<Box<dyn crate::system::process::ProcessOutput>> {
            None
        }
        fn take_stderr(&mut self) -> Option<Box<dyn crate::system::process::ProcessOutput>> {
            None
        }
        async fn write_stdin(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
        async fn wait(&mut self) -> Result<crate::system::RunningProcessExitStatus> {
            Ok(crate::system::RunningProcessExitStatus::new(Some(0), None))
        }
        async fn try_status(&mut self) -> Result<Option<crate::system::RunningProcessExitStatus>> {
            Ok(None)
        }
        async fn stop(&mut self, _: StopSignal) -> Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn unsupported_group_backend_stays_unknown_after_successful_wait() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let registry = ControlCancellationRegistry::default();
        let original = OriginalManagedProcess::before_spawn(&scope(), &ledger)?;
        registry.retain_managed_process(original.clone(), &ledger)?;
        let mut process = UnsupportedProcess;
        original.attach_original_handle(&process)?;
        process.wait().await?;
        original.record_successful_exit();
        registry.cancel_local_producers();
        let snapshot = registry.capture_retiring_producers(&scope(), 9, &ledger)?;
        registry.drain_retiring().await?;
        assert!(snapshot.finish(&registry)?.is_none());
        drop((original, registry));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[tokio::test]
    async fn finite_process_registry_reaps_only_actual_exit_with_empty_group() -> Result<()> {
        let (_dir, execution) = execution();
        let mut limits = StateMemoryLimits::default();
        limits.record_limit = 1;
        let ledger = InputEvidenceBudgetLedger::new(limits)?;
        let registry = ControlCancellationRegistry::default();
        let first = OriginalManagedProcess::before_spawn(&scope(), &ledger)?;
        registry.retain_managed_process(first.clone(), &ledger)?;
        let second = OriginalManagedProcess::before_spawn(&scope(), &ledger)?;
        assert!(registry
            .retain_managed_process(second.clone(), &ledger)
            .is_err());
        let mut process = LocalSystem::new()
            .spawn(&execution, request("exit 0"))
            .await?;
        first.attach_original_handle(process.as_ref())?;
        process.wait().await?;
        first.record_successful_exit();
        registry.retain_managed_process(second.clone(), &ledger)?;
        drop((first, second, registry));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
