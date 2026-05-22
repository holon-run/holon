use super::*;
use crate::tool::{apply_patch::ApplyPatchSurface, ToolSpec};

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
            Some(message),
            operator_binding_id.as_deref(),
            operator_reply_route_id.as_deref(),
        )
        .await?;
        self.record_incoming_transcript_entry(message)?;
        let ack = brief::make_ack(&message.agent_id, message);
        self.persist_brief(&ack).await?;
        let context_build_started = std::time::Instant::now();
        let identity = self.agent_identity_view().await?;
        self.ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
            .await?;
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
            let (provider, available_tools, _) = self.provider_tool_selection(&identity).await?;
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
        let context_build_ms = context_build_started.elapsed().as_millis() as u64;
        let (turn_index, run_id) = {
            let guard = self.inner.agent.lock().await;
            (guard.state.turn_index, guard.state.current_run_id.clone())
        };
        self.inner.storage.append_event(&AuditEvent::new(
            "turn_context_built",
            serde_json::json!({
                "agent_id": message.agent_id.clone(),
                "message_id": message.id.clone(),
                "turn_index": turn_index,
                "run_id": run_id,
                "duration_ms": context_build_ms,
                "context_section_count": built.context_sections.len(),
                "rendered_context_chars": built.rendered_context_attachment.chars().count(),
                "rendered_system_chars": built.rendered_system_prompt.chars().count(),
            }),
        ))?;
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

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn filtered_tool_specs(
        &self,
        identity: &AgentIdentityView,
    ) -> Result<Vec<crate::tool::ToolSpec>> {
        self.filtered_tool_specs_for_apply_patch_surface(
            identity,
            ApplyPatchSurface::UnifiedDiffJson,
        )
    }

    fn filtered_tool_specs_for_apply_patch_surface(
        &self,
        identity: &AgentIdentityView,
        apply_patch_surface: ApplyPatchSurface,
    ) -> Result<Vec<crate::tool::ToolSpec>> {
        Ok(self
            .inner
            .tools
            .tool_specs_with_families_for_apply_patch_surface(apply_patch_surface)?
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
        self.ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
            .await?;
        let (provider, available_tools, _) = self.provider_tool_selection(&identity).await?;
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
            self.user_home().as_deref(),
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

    pub(super) async fn provider_tool_selection(
        &self,
        identity: &AgentIdentityView,
    ) -> Result<(
        Arc<dyn AgentProvider>,
        Vec<ToolSpec>,
        Option<ProviderNativeWebSearchRequest>,
    )> {
        let provider = self.current_provider().await;
        let native_search_provider = self.web_config().native_search_provider();
        let native_web_search = self.native_web_search_request_for_provider(
            provider.as_ref(),
            native_search_provider.as_ref(),
        );
        let apply_patch_surface = {
            let guard = self.inner.agent.lock().await;
            self.apply_patch_surface_for_state(&guard.state)
        };
        let available_tools = self.filter_native_web_search_tools(
            self.filtered_tool_specs_for_apply_patch_surface(identity, apply_patch_surface)?,
            native_web_search.is_some(),
        );
        Ok((provider, available_tools, native_web_search))
    }

    fn native_web_search_request_for_provider(
        &self,
        provider: &dyn AgentProvider,
        native_search_provider: Option<&(String, WebProviderKind)>,
    ) -> Option<ProviderNativeWebSearchRequest> {
        native_web_search_request_for_config(
            provider.native_web_search_kind(),
            native_search_provider,
            self.web_config().search.max_results,
        )
    }

    fn filter_native_web_search_tools(
        &self,
        mut tools: Vec<ToolSpec>,
        native_search_configured: bool,
    ) -> Vec<ToolSpec> {
        if native_search_configured {
            tools.retain(|tool| tool.name != crate::tool::tools::web_search::NAME);
        }
        tools
    }
}

fn native_web_search_request_for_config(
    provider_native_kind: Option<ProviderNativeWebSearchKind>,
    native_search_provider: Option<&(String, WebProviderKind)>,
    max_results: usize,
) -> Option<ProviderNativeWebSearchRequest> {
    let (provider_id, provider_kind) = native_search_provider?;
    let kind = match *provider_kind {
        WebProviderKind::OpenAiNative => ProviderNativeWebSearchKind::OpenAi,
        WebProviderKind::AnthropicNative => ProviderNativeWebSearchKind::Anthropic,
        WebProviderKind::GeminiNative => ProviderNativeWebSearchKind::Gemini,
        _ => return None,
    };

    (provider_native_kind == Some(kind)).then_some(ProviderNativeWebSearchRequest {
        kind,
        provider_id: provider_id.clone(),
        max_results: Some(max_results.max(1)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_web_search_request_requires_matching_model_provider() {
        let native_provider = ("openai-native".to_string(), WebProviderKind::OpenAiNative);

        assert!(native_web_search_request_for_config(
            Some(ProviderNativeWebSearchKind::Anthropic),
            Some(&native_provider),
            5,
        )
        .is_none());
    }

    #[test]
    fn native_web_search_request_clamps_max_results() {
        let native_provider = ("openai-native".to_string(), WebProviderKind::OpenAiNative);

        let request = native_web_search_request_for_config(
            Some(ProviderNativeWebSearchKind::OpenAi),
            Some(&native_provider),
            0,
        )
        .unwrap();

        assert_eq!(request.max_results, Some(1));
    }
}
