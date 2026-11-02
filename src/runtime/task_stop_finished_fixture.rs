//! Real initial SQL/owned invocation for the Host finished-attempt tests.
use super::*;

impl RuntimeHandle {
    pub(crate) async fn original_task_stop_finished_host_fixture(
        &self,
        task_id: &str,
    ) -> Result<FinishedOriginalTaskStopAttempt> {
        let at = self.now();
        let task = TaskRecord {
            id: task_id.into(),
            agent_id: self.instance_agent_id().into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(serde_json::json!({"cancel_requested": false})),
            recovery: None,
        };
        self.persist_task_transition(&task, "finished_stop_host_fixture")
            .await?;
        let response = self
            .start_original_task_stop_owned_with_initial_fault(
                task_id,
                Some(TransitionFaultPoint::BeforeCacheUpdate),
            )?
            .await?;
        anyhow::ensure!(response.is_err(), "fixture must lose original initial ACK");
        self.join_owned_mutations().await?;
        let scope = resident_input_evidence::InputEvidenceScope {
            agent_id: self.instance_agent_id().into(),
            identity_incarnation: self.instance_incarnation(),
        };
        let (plans, _selectors) = self
            .inner
            .task_stop_effects
            .retained_plans(&self.inner.input_evidence_budget, &scope)?;
        anyhow::ensure!(
            plans.len() == 1 && plans[0].intent().task.key.id == task_id,
            "fixture must retain only the actual original SQL plan"
        );
        plans[0].finished_attempt()
    }
}
