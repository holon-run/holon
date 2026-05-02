use super::*;

impl RuntimeHandle {
    pub(super) async fn process_interactive_message(
        &self,
        message: &MessageEnvelope,
        continuation_resolution: Option<&ContinuationResolution>,
        loop_control: LoopControlOptions,
    ) -> Result<()> {
        let (operator_binding_id, operator_reply_route_id) =
            Self::operator_transport_from_message(message);
        self.begin_interactive_turn(
            operator_binding_id.as_deref(),
            operator_reply_route_id.as_deref(),
        )
        .await?;
        self.record_incoming_transcript_entry(message)?;
        let ack = brief::make_ack(&message.agent_id, message);
        self.persist_brief(&ack).await?;
        let identity = self.agent_identity_view().await?;
        let context_config = self.current_context_config().await;

        let built = {
            let mut guard = self.inner.agent.lock().await;
            let agent_changed =
                maybe_compact_agent(&self.inner.storage, &mut guard.state, &context_config)?;
            if agent_changed {
                self.inner.storage.write_agent(&guard.state)?;
            }
            let state = guard.state.clone();
            drop(guard);
            let available_tools = self.filtered_tool_specs(&identity)?;
            let provider = self.current_provider().await;
            let prompt_tools = provider.prompt_tool_specs(&available_tools);
            let workspace = self.workspace_view_from_state(&state)?;
            let execution = self.execution_snapshot_for_view(
                state.execution_profile.clone(),
                &workspace,
                &state.attached_workspaces,
            );
            let loaded_agents_md = self.loaded_agents_md_for_state(&state)?;
            let skills = self.skills_runtime_view_for_state(&state, &identity)?;
            build_effective_prompt(
                &self.inner.storage,
                &state,
                &execution,
                message,
                &context_config,
                &execution.execution_root,
                self.agent_home().as_path(),
                &identity,
                loaded_agents_md,
                &skills,
                &prompt_tools,
                continuation_resolution,
            )?
        };
        if built
            .context_sections
            .iter()
            .any(|section| section.name == "working_memory_delta")
        {
            let mut guard = self.inner.agent.lock().await;
            mark_working_memory_prompted(
                &mut guard.state,
                built.cache_identity.working_memory_revision,
            );
            self.inner.storage.write_agent(&guard.state)?;
        }

        let outcome = self
            .run_agent_loop(
                &message.agent_id,
                message.trust.clone(),
                built,
                loop_control,
            )
            .await?;

        let brief = if outcome.terminal_kind.is_failure() {
            brief::make_failure(&message.agent_id, message, outcome.final_text.clone())
        } else {
            brief::make_result(&message.agent_id, message, outcome.final_text.clone())
        };
        self.persist_brief(&brief).await?;
        self.promote_turn_active_skills().await?;

        if outcome.should_sleep {
            self.transition_to_sleep(outcome.sleep_duration_ms).await?;
        }

        Ok(())
    }

    pub(super) fn filtered_tool_specs(
        &self,
        identity: &AgentIdentityView,
    ) -> Result<Vec<crate::tool::ToolSpec>> {
        Ok(self
            .inner
            .tools
            .tool_specs_with_families()?
            .into_iter()
            .filter(|(family, _)| {
                identity
                    .profile_preset
                    .allows_tool_capability_family(*family)
            })
            .map(|(_, tool)| tool)
            .collect())
    }

    pub async fn preview_prompt(&self, text: String, trust: TrustLevel) -> Result<EffectivePrompt> {
        let message = MessageEnvelope::new(
            self.agent_id().await?,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("debug_prompt".into()),
            },
            trust.clone(),
            Priority::Normal,
            MessageBody::Text { text },
        )
        .with_admission(
            MessageDeliverySurface::CliPrompt,
            AdmissionContext::LocalProcess,
        );
        let mut agent = self.agent_state().await?;
        let context_config = self.current_context_config().await;
        let _ = maybe_compact_agent(&self.inner.storage, &mut agent, &context_config)?;
        let prior_closure = self.current_closure_decision().await?;
        let continuation = ContinuationTrigger::from_message(&message, None)
            .map(|trigger| resolve_continuation(&prior_closure, &trigger));
        let identity = self.agent_identity_view().await?;
        let available_tools = self.filtered_tool_specs(&identity)?;
        let provider = self.current_provider().await;
        let prompt_tools = provider.prompt_tool_specs(&available_tools);
        let execution = self.execution_snapshot().await?;
        let loaded_agents_md = self.loaded_agents_md_for_state(&agent)?;
        let skills = self.skills_runtime_view_for_state(&agent, &identity)?;
        build_effective_prompt(
            &self.inner.storage,
            &agent,
            &execution,
            &message,
            &context_config,
            &execution.execution_root,
            self.agent_home().as_path(),
            &identity,
            loaded_agents_md,
            &skills,
            &prompt_tools,
            continuation.as_ref(),
        )
    }

    pub(super) async fn build_subagent_prompt_for_workspace(
        &self,
        agent_id: &str,
        prompt: &str,
        trust: &TrustLevel,
        execution: &EffectiveExecution,
    ) -> Result<EffectivePrompt> {
        let message = MessageEnvelope::new(
            agent_id.to_string(),
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "subagent".into(),
            },
            trust.clone(),
            Priority::Next,
            MessageBody::Text {
                text: prompt.to_string(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        let loaded_agents_md = load_agents_md(
            self.agent_home().as_path(),
            execution
                .workspace
                .workspace_id()
                .map(|_| execution.workspace.workspace_anchor()),
        )?;
        let state = self
            .inner
            .storage
            .read_agent()?
            .unwrap_or_else(|| AgentState::new(agent_id.to_string()));
        let identity = AgentIdentityView {
            agent_id: agent_id.to_string(),
            kind: AgentKind::Child,
            visibility: crate::types::AgentVisibility::Private,
            ownership: crate::types::AgentOwnership::ParentSupervised,
            profile_preset: crate::types::AgentProfilePreset::PrivateChild,
            status: crate::types::AgentRegistryStatus::Active,
            is_default_agent: false,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        };
        let skills = self.skills_runtime_view_for_state(&state, &identity)?;
        let continuation = ContinuationTrigger::from_message(&message, None).map(|trigger| {
            resolve_continuation(
                &ClosureDecision {
                    outcome: crate::types::ClosureOutcome::Completed,
                    waiting_reason: None,
                    work_signal: None,
                    runtime_posture: RuntimePosture::Awake,
                    evidence: vec!["synthetic_subagent_prompt_preview".into()],
                },
                &trigger,
            )
        });
        let context_config = self.current_context_config().await;
        build_effective_prompt(
            &self.inner.storage,
            &AgentState::new(agent_id.to_string()),
            &execution.snapshot(),
            &message,
            &context_config,
            execution.workspace.execution_root(),
            self.agent_home().as_path(),
            &identity,
            loaded_agents_md,
            &skills,
            &[],
            continuation.as_ref(),
        )
    }
}
