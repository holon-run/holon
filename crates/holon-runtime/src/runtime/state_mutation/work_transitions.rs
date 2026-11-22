//! Owned original Work commands share the input transaction's exact witness.

use super::*;
use crate::runtime_db::transitions::{
    ExecutionProtocolTransition, PreparedInputTransition, TaskExpectation, WaitTransitionCommand,
    WorkCommandLane, WorkItemFocusTransitionCommand, WorkItemTransitionCommand,
};

impl RuntimeMutation {
    #[inline(never)]
    pub(in crate::runtime) fn commit_work_transition<'a>(
        &'a mut self,
        operation_id: String,
        command: WorkItemTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<TransitionCommit>> + Send + 'a>>
    {
        Box::pin(self.commit_work_transition_in_lane(
            operation_id,
            command,
            execution_protocol,
            synchronize_execution_protocol,
            None,
        ))
    }

    pub(in crate::runtime) async fn commit_work_transition_in_lane(
        &mut self,
        operation_id: String,
        command: WorkItemTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
        lane: Option<WorkCommandLane>,
    ) -> Result<TransitionCommit> {
        let prepared = PreparedInputTransition::work(
            self.input_evidence_scope(),
            operation_id,
            Utc::now(),
            command,
            execution_protocol,
            synchronize_execution_protocol,
        )?
        .with_work_command_lane(lane);
        self.commit_prepared_input_transition(prepared).await
    }

    #[inline(never)]
    pub(in crate::runtime) fn commit_focus_transition<'a>(
        &'a mut self,
        operation_id: String,
        command: WorkItemFocusTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
        completion_tool_execution: Option<ToolExecutionRecord>,
        lane: Option<WorkCommandLane>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<TransitionCommit>> + Send + 'a>>
    {
        Box::pin(self.commit_focus_transition_inner(
            operation_id,
            command,
            execution_protocol,
            synchronize_execution_protocol,
            completion_tool_execution,
            lane,
        ))
    }

    async fn commit_focus_transition_inner(
        &mut self,
        operation_id: String,
        command: WorkItemFocusTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
        completion_tool_execution: Option<ToolExecutionRecord>,
        lane: Option<WorkCommandLane>,
    ) -> Result<TransitionCommit> {
        let prepared = PreparedInputTransition::focus(
            self.input_evidence_scope(),
            operation_id,
            Utc::now(),
            command,
            execution_protocol,
            synchronize_execution_protocol,
            completion_tool_execution,
        )?
        .with_work_command_lane(lane);
        self.commit_prepared_input_transition(prepared).await
    }

    #[inline(never)]
    pub(in crate::runtime) fn commit_wait_transition<'a>(
        &'a mut self,
        operation_id: String,
        command: WaitTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        task_expectation: Option<TaskExpectation>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<TransitionCommit>> + Send + 'a>>
    {
        Box::pin(self.commit_wait_transition_inner(
            operation_id,
            command,
            execution_protocol,
            task_expectation,
        ))
    }

    async fn commit_wait_transition_inner(
        &mut self,
        operation_id: String,
        command: WaitTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        task_expectation: Option<TaskExpectation>,
    ) -> Result<TransitionCommit> {
        let prepared = PreparedInputTransition::wait(
            self.input_evidence_scope(),
            operation_id,
            Utc::now(),
            command,
            execution_protocol,
            task_expectation,
        )?;
        self.commit_prepared_input_transition(prepared).await
    }
}
