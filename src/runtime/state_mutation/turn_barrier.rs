use super::*;
use crate::runtime::resident_input_evidence::{
    InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence as Fact,
};
use crate::runtime_db::{
    input_evidence_snapshot::InputEvidenceLoadRequest,
    prepared_input_evidence::PreparedInputEvidence,
};

impl RuntimeMutation {
    pub(super) async fn tool_result_barrier(
        &mut self,
        record: &ToolExecutionRecord,
    ) -> Result<Option<TurnRecord>> {
        if record.call_binding.is_some() {
            return Ok(None);
        }
        if !matches!(
            record.status,
            ToolExecutionStatus::Success | ToolExecutionStatus::Error
        ) || !matches!(
            record.tool_name.as_str(),
            crate::tool::names::EXEC_COMMAND
                | crate::tool::names::EXEC_COMMAND_BATCH
                | crate::tool::names::SCHEDULE_DESTRUCTIVE_OPERATION
        ) {
            return Ok(None);
        }
        let Some(turn_id) = self.state.current_turn_id.as_deref() else {
            return Ok(None);
        };
        anyhow::ensure!(
            record.turn_id.as_deref() == Some(turn_id)
                && record.turn_index == self.state.turn_index,
            "Tool result does not belong to the current Turn"
        );
        let (_, record_limit, byte_limit) = self.context_input_evidence_policy()?;
        let request = InputEvidenceLoadRequest {
            query: None,
            windows: Vec::new(),
            mandatory_roots: std::collections::BTreeSet::from([Key::new(Family::Turn, turn_id)]),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit,
            byte_limit,
        };
        self.prepare_owner_input_request(&request, 0).await?;
        let inputs = self
            .input_evidence
            .as_ref()
            .context("Tool result has no prepared Input")?
            .capture_for_request(&request)?;
        let Some(prior) = inputs.lookup(
            request
                .mandatory_roots
                .first()
                .context("Tool result Turn selector missing")?,
        )?
        else {
            return Ok(None);
        };
        let Fact::Turn(prior) = &prior.record else {
            anyhow::bail!("Tool result prior is not a Turn");
        };
        if prior.terminal.is_some() || !prior.external_side_effect_possible {
            return Ok(None);
        }
        crate::diagnostics::resource_io::admit_cloned_value(prior)?;
        Ok(Some(prior.clone()))
    }

    #[inline(never)]
    pub(in crate::runtime) fn set_current_turn_external_side_effect_possible<'a>(
        &'a mut self,
        tool_name: &'a str,
        possible: bool,
        turn_tool_call: Option<(&'a str, &'a str)>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.set_current_turn_external_side_effect_possible_inner(
            tool_name,
            possible,
            turn_tool_call,
        ))
    }

    async fn set_current_turn_external_side_effect_possible_inner(
        &mut self,
        tool_name: &str,
        possible: bool,
        turn_tool_call: Option<(&str, &str)>,
    ) -> Result<()> {
        if let Some((expected_turn, _)) = turn_tool_call {
            anyhow::ensure!(
                self.state.current_turn_id.as_deref() == Some(expected_turn),
                "external tool call no longer owns the current Turn"
            );
        }
        let Some(turn_id) = self.state.current_turn_id.clone() else {
            return Ok(());
        };
        let tool_call_id = turn_tool_call.map(|(_, tool_call_id)| tool_call_id);
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
            anyhow::ensure!(
                tool_call_id.is_none(),
                "external tool call has no qualified Turn"
            );
            return Ok(());
        };
        let Fact::Turn(prior) = &prior.record else {
            anyhow::bail!("Turn barrier prior has another family");
        };
        if possible && (tool_call_id.is_some() || prior.external_side_effect_tool_call_id.is_some())
        {
            anyhow::ensure!(
                prior.terminal.is_none() && !prior.external_side_effect_possible,
                "Turn already has an unresolved external tool call"
            );
        }
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
            tool_call_id,
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
