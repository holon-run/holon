//! Physical original-loop exit; never a durable State or terminal ACK.
use super::*;

pub(crate) struct JoinedRuntimeLoopExit {
    process_registry_id: uuid::Uuid,
    owner_epoch: u64,
    incarnation: u64,
}
impl JoinedRuntimeLoopExit {
    // Only Host collectors supply the original registered runtime/monitor pair.
    pub(super) fn from_finished_monitor(
        runtime: &RuntimeHandle,
        original: &tokio::task::AbortHandle,
    ) -> Result<Self> {
        anyhow::ensure!(
            !runtime.accepts_execution_admission(),
            "loop exit requires closed execution admission"
        );
        anyhow::ensure!(
            runtime.matches_original_loop_monitor(original),
            "runtime loop exit has another original monitor"
        );
        anyhow::ensure!(
            original.is_finished(),
            "original runtime loop has not exited"
        );
        Ok(Self {
            process_registry_id: runtime.original_process_registry_id(),
            owner_epoch: runtime.instance_owner_epoch(),
            incarnation: runtime.instance_incarnation(),
        })
    }
    pub(crate) fn matches(&self, runtime: &RuntimeHandle) -> bool {
        self.process_registry_id == runtime.original_process_registry_id()
            && self.owner_epoch == runtime.instance_owner_epoch()
            && self.incarnation == runtime.instance_incarnation()
            && !runtime.accepts_execution_admission()
    }
}
impl RuntimeHost {
    pub(super) async fn acknowledge_registered_loop_exit(
        &self,
        runtime: &RuntimeHandle,
        original: &tokio::task::AbortHandle,
    ) -> Result<()> {
        anyhow::ensure!(
            !runtime.accepts_execution_admission(),
            "original loop admission is still open"
        );
        anyhow::ensure!(
            runtime.matches_original_loop_monitor(original),
            "runtime retirement has another original monitor"
        );
        // A phase channel can close during Future drop, just before Tokio marks
        // the original monitor finished. Observe actual exit, not that hint.
        tokio::time::timeout(HOST_SHUTDOWN_GRACE, async {
            while !original.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("original runtime loop exit has not completed")?;
        let exit = JoinedRuntimeLoopExit::from_finished_monitor(runtime, original)?;
        runtime.acknowledge_joined_runtime_loop_exit(&exit).await
    }
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod tests {
    use super::*;
    fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
        RuntimeHandle::new(
            "default",
            home.into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "default".into(),
            crate::context::ContextConfig::default(),
        )
    }
    #[tokio::test]
    async fn original_loop_exit_requires_registered_finished_monitor_and_exact_instance(
    ) -> Result<()> {
        let first_home = tempfile::tempdir()?;
        let first = runtime(first_home.path())?;
        let actual = tokio::spawn(std::future::pending::<()>());
        let original = actual.abort_handle();
        first.register_original_loop_monitor(original.clone())?;
        first
            .retire_instance_locally("fixture original loop exit")
            .await;
        assert!(JoinedRuntimeLoopExit::from_finished_monitor(&first, &original).is_err());
        let foreign = tokio::spawn(async {});
        let wrong = foreign.abort_handle();
        foreign.await?;
        assert!(JoinedRuntimeLoopExit::from_finished_monitor(&first, &wrong).is_err());
        assert!(first.register_original_loop_monitor(wrong).is_err());
        actual.abort();
        let _ = actual.await;
        let proof = JoinedRuntimeLoopExit::from_finished_monitor(&first, &original)?;
        let other_home = tempfile::tempdir()?;
        let other = runtime(other_home.path())?;
        assert!(!proof.matches(&other));
        other
            .retire_instance_locally("fixture other instance")
            .await;
        assert!(other
            .acknowledge_joined_runtime_loop_exit(&proof)
            .await
            .is_err());
        first.acknowledge_joined_runtime_loop_exit(&proof).await?;
        first.acknowledge_joined_runtime_loop_exit(&proof).await?;
        Ok(())
    }
}
