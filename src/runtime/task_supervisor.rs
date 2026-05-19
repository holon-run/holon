use anyhow::Result;

use super::RuntimeHandle;
use crate::types::{
    AgentProfilePreset, CommandTaskSpec, ExecCommandDuplicatePolicy, ExecCommandResult,
    SpawnAgentResult, TaskInputResult, TaskListEntry, TaskOutputResult, TaskRecord,
    TaskStatusSnapshot, TrustLevel,
};

pub(crate) struct ManagedTaskSupervisor<'a> {
    runtime: &'a RuntimeHandle,
}

impl RuntimeHandle {
    pub(crate) fn managed_tasks(&self) -> ManagedTaskSupervisor<'_> {
        ManagedTaskSupervisor { runtime: self }
    }
}

impl ManagedTaskSupervisor<'_> {
    pub(crate) async fn execute_exec_command(
        &self,
        spec: CommandTaskSpec,
        duplicate_policy: ExecCommandDuplicatePolicy,
        trust: &TrustLevel,
    ) -> Result<ExecCommandResult> {
        self.runtime
            .execute_exec_command(spec, duplicate_policy, trust)
            .await
    }

    pub(crate) async fn execute_exec_command_once(
        &self,
        spec: CommandTaskSpec,
        trust: &TrustLevel,
    ) -> Result<ExecCommandResult> {
        self.runtime.execute_exec_command_once(spec, trust).await
    }

    pub(crate) async fn spawn_agent(
        &self,
        initial_message: Option<String>,
        trust: TrustLevel,
        preset: AgentProfilePreset,
        agent_id: Option<String>,
        worktree: bool,
        template: Option<String>,
    ) -> Result<SpawnAgentResult> {
        self.runtime
            .spawn_agent(initial_message, trust, preset, agent_id, worktree, template)
            .await
    }

    pub(crate) async fn latest_task_list_entries(&self) -> Result<Vec<TaskListEntry>> {
        self.runtime.latest_task_list_entries().await
    }

    pub(crate) async fn task_status_snapshot(&self, task_id: &str) -> Result<TaskStatusSnapshot> {
        self.runtime.task_status_snapshot(task_id).await
    }

    pub(crate) async fn task_output(
        &self,
        task_id: &str,
        block: bool,
        timeout_ms: u64,
    ) -> Result<TaskOutputResult> {
        self.runtime.task_output(task_id, block, timeout_ms).await
    }

    pub(crate) async fn stop_task(&self, task_id: &str, trust: &TrustLevel) -> Result<TaskRecord> {
        self.runtime.stop_task(task_id, trust).await
    }

    pub(crate) async fn task_input_with_trust(
        &self,
        task_id: &str,
        input: &str,
        trust: &TrustLevel,
    ) -> Result<TaskInputResult> {
        self.runtime
            .task_input_with_trust(task_id, input, trust)
            .await
    }
}
