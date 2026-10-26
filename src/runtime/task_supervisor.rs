use anyhow::Result;

use super::RuntimeHandle;
use crate::observability::TraceContext;
use crate::types::{
    AuthorityClass, CommandTaskSpec, ExecCommandDuplicatePolicy, ExecCommandResult,
    TaskInputResult, TaskRecord,
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
        authority_class: &AuthorityClass,
        trace_context: Option<&TraceContext>,
    ) -> Result<ExecCommandResult> {
        self.runtime
            .execute_exec_command(spec, duplicate_policy, authority_class, trace_context)
            .await
    }

    pub(crate) async fn execute_exec_command_once(
        &self,
        spec: CommandTaskSpec,
        authority_class: &AuthorityClass,
        trace_context: Option<&TraceContext>,
        tool_name: &str,
    ) -> Result<ExecCommandResult> {
        self.runtime
            .execute_exec_command_once(spec, authority_class, trace_context, tool_name)
            .await
    }

    pub(crate) async fn latest_task_list_entries(&self) -> Result<super::task_reads::TaskListRead> {
        self.runtime.active_task_list_snapshot(usize::MAX).await
    }

    pub(crate) async fn task_status_read(
        &self,
        task_id: &str,
    ) -> Result<super::task_reads::TaskStatusRead> {
        self.runtime.task_status_read(task_id).await
    }

    pub(crate) async fn task_output_read(
        &self,
        task_id: &str,
        block: bool,
        timeout_ms: u64,
    ) -> Result<super::tasks::TaskOutputResultRead> {
        self.runtime
            .task_output_read(task_id, block, timeout_ms)
            .await
    }

    pub(crate) async fn stop_task(
        &self,
        task_id: &str,
        authority_class: &AuthorityClass,
    ) -> Result<TaskRecord> {
        self.runtime.stop_task(task_id, authority_class).await
    }

    pub(crate) async fn task_input_with_trust(
        &self,
        task_id: &str,
        input: &str,
        authority_class: &AuthorityClass,
    ) -> Result<TaskInputResult> {
        self.runtime
            .task_input_with_trust(task_id, input, authority_class)
            .await
    }
}
