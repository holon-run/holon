use super::*;
use crate::runtime::resident_input_evidence::{
    InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence as Fact,
};
use crate::runtime_db::{
    input_evidence_snapshot::InputEvidenceLoadRequest,
    prepared_input_evidence::PreparedInputEvidence,
};

impl RuntimeMutation {
    #[inline(never)]
    pub(in crate::runtime) fn set_current_turn_external_side_effect_possible<'a>(
        &'a mut self,
        tool_name: &'a str,
        possible: bool,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.set_current_turn_external_side_effect_possible_inner(tool_name, possible))
    }

    async fn set_current_turn_external_side_effect_possible_inner(
        &mut self,
        tool_name: &str,
        possible: bool,
    ) -> Result<()> {
        let Some(turn_id) = self.state.current_turn_id.clone() else {
            return Ok(());
        };
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        let request = InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: std::collections::BTreeSet::from([Key::new(Family::Turn, &turn_id)]),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        self.prepare_owner_input_request(&request, 0).await?;
        let inputs = self
            .input_evidence
            .as_ref()
            .context("Turn barrier has no prepared Input")?
            .capture_for_request(&request)?;
        let Some(prior) = inputs.lookup(&Key::new(Family::Turn, &turn_id))? else {
            return Ok(());
        };
        let Fact::Turn(prior) = &prior.record else {
            anyhow::bail!("Turn barrier prior has another family");
        };
        if prior.terminal.is_some() || prior.external_side_effect_possible == possible {
            return Ok(());
        }
        let scope = self.input_evidence_scope();
        let prepared = PreparedInputEvidence::turn_barrier(
            scope.clone(),
            crate::ids::audit_event_id(),
            Utc::now(),
            prior,
            possible,
            &self.runtime.inner.input_evidence_budget,
        )?;
        self.commit_prepared_evidence(prepared, None).await?;
        let (kind, recovery_policy) = if possible {
            (
                "external_command_no_replay_barrier",
                "never_replay_unknown_external_side_effect",
            )
        } else {
            (
                "external_command_checkpoint_durable",
                "resume_from_durable_tool_result",
            )
        };
        self.append_original_audit_event(AuditEvent::legacy(
            kind,
            serde_json::json!({
                "agent_id": scope.agent_id,
                "turn_id": turn_id,
                "tool_name": tool_name,
                "recovery_policy": recovery_policy,
            }),
        ))
        .await
    }
}
