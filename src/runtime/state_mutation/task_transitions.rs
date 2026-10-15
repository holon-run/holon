//! Task's parent and child metadata share one original transaction witness.

use super::*;
use crate::runtime_db::{
    input_mutation_receipts::{InputMutationResult, InputTaskDeletionResult},
    repositories::{
        AgentDeletionAdmission, AgentDeletionRequest, TerminalChildDeletionAdmissionRejected,
    },
    transitions::{
        ExecutionProtocolTransition, PreparedInputTransition, PreparedTaskTransition,
        TaskTransitionCommand,
    },
};

impl RuntimeMutation {
    // Metadata was read only after the exact child reservation was acquired.
    // This accessor neither discovers a new scope nor opens another read view.
    fn frozen_task_child_identity(
        &self,
        request: &AgentDeletionRequest,
    ) -> Result<crate::types::AgentIdentityRecord> {
        let reject = |reason: &str| -> anyhow::Error {
            match &request.admission {
                AgentDeletionAdmission::TerminalEphemeralChild { .. } => {
                    TerminalChildDeletionAdmissionRejected::new(&request.agent_id, reason).into()
                }
                _ => anyhow!("Task deletion target {}: {reason}", request.agent_id),
            }
        };
        if request.agent_id == self.input_evidence_scope().agent_id {
            return Err(reject("Task deletion requires a distinct child target"));
        }
        let (index, reservation) = self
            .reservations
            .iter()
            .enumerate()
            .find(|(_, reservation)| reservation.agent_id() == request.agent_id)
            .ok_or_else(|| reject("original child reservation is missing"))?;
        let identity = self.metadata[index]
            .as_ref()
            .and_then(|facts| facts.identity.as_ref())
            .ok_or_else(|| reject("identity not found"))?;
        anyhow::ensure!(
            identity.agent_id == request.agent_id
                && identity.incarnation == reservation.identity_incarnation(),
            "Task child metadata differs from its original reservation"
        );
        if !matches!(
            identity.status,
            crate::types::AgentRegistryStatus::Active | crate::types::AgentRegistryStatus::Deleting
        ) {
            return Err(reject("identity is already deleted"));
        }
        Ok(identity.as_ref().clone())
    }

    #[inline(never)]
    pub(in crate::runtime) fn commit_task_transition<'a>(
        &'a mut self,
        operation_id: String,
        command: TaskTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<
                    Output = Result<(TransitionCommit, Option<InputTaskDeletionResult>)>,
                > + Send
                + 'a,
        >,
    > {
        self.commit_task_transition_inner(
            operation_id,
            command,
            execution_protocol,
            synchronize_execution_protocol,
            None,
        )
    }

    #[inline(never)]
    pub(in crate::runtime) fn commit_task_transition_with_original_workspace_request<'a>(
        &'a mut self,
        operation_id: String,
        command: TaskTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
        material: Arc<super::super::OriginalWorkspaceRequestMaterial>,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<
                    Output = Result<(TransitionCommit, Option<InputTaskDeletionResult>)>,
                > + Send
                + 'a,
        >,
    > {
        self.commit_task_transition_inner(
            operation_id,
            command,
            execution_protocol,
            synchronize_execution_protocol,
            Some(material),
        )
    }

    #[inline(never)]
    fn commit_task_transition_inner<'a>(
        &'a mut self,
        operation_id: String,
        command: TaskTransitionCommand,
        execution_protocol: ExecutionProtocolTransition,
        synchronize_execution_protocol: bool,
        original_workspace_material: Option<Arc<super::super::OriginalWorkspaceRequestMaterial>>,
    ) -> std::pin::Pin<
        Box<
            impl std::future::Future<
                    Output = Result<(TransitionCommit, Option<InputTaskDeletionResult>)>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let task_id = command.task.id.clone();
            let expected_child = command
                .agent_deletion
                .as_ref()
                .map(|request| self.frozen_task_child_identity(request))
                .transpose()?;
            let deletion_id = expected_child
                .as_ref()
                .map(|_| crate::ids::agent_deletion_id());
            let mut prepared = PreparedInputTransition::task(
                self.input_evidence_scope(),
                operation_id,
                Utc::now(),
                PreparedTaskTransition::new(
                    command,
                    execution_protocol,
                    synchronize_execution_protocol,
                    expected_child,
                    deletion_id,
                )?,
            )?;
            if let Some(material) = original_workspace_material {
                prepared = prepared.with_original_task_workspace_request_material(
                    material,
                    self.reservations[self.primary_index].owner_epoch(),
                    &self.runtime.inner.input_evidence_budget,
                )?;
            }
            let (commit, result) = self.commit_prepared_input_with_result(prepared).await?;
            let InputMutationResult::Task {
                task_id: original_task_id,
                deletion,
                ..
            } = result
            else {
                bail!("Task receipt has another result family");
            };
            anyhow::ensure!(
                original_task_id == task_id,
                "Task receipt has another task identity"
            );
            Ok((commit, deletion.map(|result| *result)))
        })
    }
}

#[cfg(test)]
#[path = "task_transition_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "physical_worktree_task_loan_tests.rs"]
mod physical_worktree_task_loan_tests;
