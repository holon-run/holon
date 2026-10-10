use super::*;
use crate::config::ModelRouteRef;
use crate::runtime::turn::TurnModelSelection;
use crate::runtime::turn::TurnTerminalTransition;
use crate::tool::{ApplyPatchSurface, ToolSpec};
use crate::types::ExecutionAdmissionProvenance;

fn record_operator_span(
    parent: Option<&crate::observability::TraceContext>,
    name: &'static str,
    started_at: chrono::DateTime<chrono::Utc>,
    message: &MessageEnvelope,
) {
    let Some(parent) = parent else {
        return;
    };
    let context = parent.child();
    crate::observability::record_span(
        &context,
        crate::observability::completed_span(
            name,
            &context,
            Some(parent.span_id.clone()),
            started_at,
            crate::observability::TraceSpanStatus::Ok,
            crate::observability::TraceAttributes {
                agent_id: Some(message.agent_id.clone()),
                message_id: Some(message.id.clone()),
                turn_id: message.turn_id.clone(),
                work_item_id: message.work_item_id.clone(),
                task_id: message.task_id.clone(),
                ..Default::default()
            },
        ),
    );
}

impl RuntimeHandle {
    #[cfg(test)]
    pub(super) async fn process_interactive_message(
        &self,
        message: &MessageEnvelope,
        loop_control: LoopControlOptions,
    ) -> Result<()> {
        let persisted_message = self.enqueue(message.clone()).await?;
        let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(self)
            .poll()
            .await?
        {
            scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
            _ => return Err(anyhow!("test message was not admitted by the scheduler")),
        };
        if scheduled.message.id != persisted_message.id {
            return Err(anyhow!("scheduler claimed an unexpected test message"));
        }
        let terminal_transition = self
            .process_interactive_message_deferred_with_cleanup(
                &scheduled.message,
                scheduled.dispatch_plan.continuation_resolution.as_ref(),
                scheduled
                    .dispatch_plan
                    .execution_admission_provenance
                    .clone(),
                loop_control,
                scheduled
                    .message
                    .trace_context
                    .as_ref()
                    .map(crate::observability::TraceContext::child),
            )
            .await?;
        self.persist_terminal_transition(&terminal_transition)
            .await?;
        Ok(())
    }

    pub(super) async fn process_interactive_message_deferred_with_cleanup(
        &self,
        message: &MessageEnvelope,
        continuation_resolution: Option<&ContinuationResolution>,
        execution_admission_provenance: Option<ExecutionAdmissionProvenance>,
        loop_control: LoopControlOptions,
        trace_context: Option<crate::observability::TraceContext>,
    ) -> Result<TurnTerminalTransition> {
        let result = Box::pin(self.process_interactive_message_deferred(
            message,
            continuation_resolution,
            execution_admission_provenance,
            loop_control,
            trace_context,
        ))
        .await;
        let cleanup = self.reconfigure_provider_for_turn(None).await;
        match (result, cleanup) {
            (Ok(transition), Ok(())) => Ok(transition),
            (Ok(_), Err(error)) => Err(error.context("failed to clear turn-local model selection")),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(cleanup_error)) => Err(error.context(format!(
                "also failed to clear turn-local model selection: {cleanup_error}"
            ))),
        }
    }

    pub(super) async fn process_interactive_message_deferred(
        &self,
        message: &MessageEnvelope,
        continuation_resolution: Option<&ContinuationResolution>,
        execution_admission_provenance: Option<ExecutionAdmissionProvenance>,
        loop_control: LoopControlOptions,
        trace_context: Option<crate::observability::TraceContext>,
    ) -> Result<TurnTerminalTransition> {
        if !matches!(
            execution_admission_provenance.as_ref(),
            Some(ExecutionAdmissionProvenance::Canonical { .. })
        ) {
            return Err(anyhow!(
                "model turn requires canonical execution admission provenance"
            ));
        }
        let (operator_binding_id, operator_reply_route_id) =
            Self::operator_transport_from_message(message);
        self.begin_interactive_turn_with_provenance(
            Some(message),
            operator_binding_id.as_deref(),
            operator_reply_route_id.as_deref(),
            execution_admission_provenance,
        )
        .await?;
        let model_selection = TurnModelSelection::from_message(message)?;
        self.reconfigure_provider_for_turn(model_selection.fallback_model())
            .await?;
        if let Some(recovery) = model_selection.recovery.as_ref() {
            let source = crate::runtime::provider_recovery::resolve_for_context(
                &self.inner.storage,
                message,
            )?
            .ok_or_else(|| anyhow!("provider recovery source resolution missing"))?;
            let (turn_id, run_id) = {
                let guard = self.inner.agent.lock().await;
                (
                    guard.state.current_turn_id.clone(),
                    guard.state.current_run_id.clone(),
                )
            };
            self.inner.storage.append_event(&AuditEvent::legacy(
                "recovery_turn_started",
                serde_json::json!({
                    "agent_id": message.agent_id,
                    "message_id": message.id,
                    "turn_id": turn_id,
                    "run_id": run_id,
                    "fallback_model_ref": recovery.fallback_model_ref,
                    "source_turn_id": recovery.source_turn_id,
                    "source_message_id": recovery.source_message_id,
                    "root_message_id": source.root_message.id,
                    "predecessor_attempt_id": source.predecessor.attempt_id,
                    "source_binding": source.predecessor.binding,
                    "source_terminal_kind": recovery.source_terminal_kind,
                    "source_round": recovery.source_round,
                }),
            ))?;
        }
        self.record_incoming_transcript_entry(message)?;
        self.inner
            .storage
            .append_event(&brief::make_acknowledgement_event(message))?;
        let context_build_started = std::time::Instant::now();
        let context_build_started_at = chrono::Utc::now();
        let identity = self.agent_identity_view().await?;
        let default_external_ingress = self
            .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
            .await?;
        let default_external_ingress = self
            .inner
            .runtime_db
            .external_triggers()
            .latest(&default_external_ingress.external_trigger_id)?;
        let context_config = self.current_context_config().await;

        let built = {
            let mut guard = self.inner.agent.lock().await;
            let agent_changed = sync_agent_message_count(&self.inner.storage, &mut guard.state)?;
            if agent_changed {
                guard.persist_state(&self.inner.storage)?;
            }
            let state = guard.state.clone();
            drop(guard);
            let (provider, available_tools, apply_patch_surface, _, _) = self
                .provider_tool_selection_for_turn(&identity, model_selection.fallback_model())
                .await?;
            let prompt_tools = provider.prompt_tool_specs(&available_tools);
            let workspace = self.workspace_view_from_state(&state)?;
            let execution = self.execution_snapshot_for_view(
                state.execution_profile.clone(),
                &workspace,
                &state.attached_workspaces,
            );
            let loaded_agents_md = self.loaded_agents_md_for_state(&state)?;
            let loaded_agent_memory = self.loaded_agent_memory_for_state()?;
            let skills = self
                .skills_runtime_view_for_state(&state, &identity)
                .await?;
            build_effective_prompt_with_apply_patch_surface_and_default_external_ingress(
                &self.inner.storage,
                &state,
                &execution,
                message,
                &context_config,
                &execution.execution_root,
                self.agent_home().as_path(),
                &identity,
                loaded_agents_md,
                loaded_agent_memory,
                &skills,
                &prompt_tools,
                apply_patch_surface,
                continuation_resolution,
                default_external_ingress.as_ref(),
            )?
        };
        let context_build_ms = context_build_started.elapsed().as_millis() as u64;
        let (turn_index, run_id) = {
            let guard = self.inner.agent.lock().await;
            (guard.state.turn_index, guard.state.current_run_id.clone())
        };
        self.inner.storage.append_event(&AuditEvent::legacy(
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
        record_operator_span(
            trace_context.as_ref(),
            "holon.turn.context_build",
            context_build_started_at,
            message,
        );
        let mut outcome = self
            .run_agent_loop_deferred(
                &message.agent_id,
                message.authority_class.clone(),
                built,
                model_selection,
                loop_control,
                trace_context.clone(),
            )
            .await?;
        crate::diagnostics::record_turn_total(context_build_started.elapsed());
        let cleanup_started = std::time::Instant::now();
        let cleanup_started_at = chrono::Utc::now();

        if outcome.prepared_work_item_completion.is_some() {
            // The completion report brief, WorkItem transition, tool execution,
            // Turn terminal, queue claim, and execution outcome are committed
            // together by the outer canonical terminal settlement.
        } else if let Some(prepared) = outcome.prepared_wait_for.as_mut() {
            if prepared.delivery == crate::tool::tools::wait_for::WaitForDeliveryArg::Final
                && !outcome.final_text.trim().is_empty()
            {
                let mut brief =
                    brief::make_result(&message.agent_id, message, outcome.final_text.clone());
                if !outcome.final_citations.is_empty() {
                    brief.citations = Some(outcome.final_citations.clone());
                }
                brief.turn_index = Some(outcome.turn_index);
                brief.turn_id = Some(outcome.terminal.turn_id.clone());
                {
                    let guard = self.inner.agent.lock().await;
                    brief.workspace_id = guard
                        .state
                        .active_workspace_entry
                        .as_ref()
                        .map(|entry| entry.workspace_id.clone())
                        .unwrap_or_else(|| {
                            crate::types::agent_home_workspace_id(&message.agent_id)
                        });
                    brief.work_item_id = prepared
                        .registration
                        .condition
                        .work_item_id
                        .clone()
                        .or_else(|| guard.state.current_turn_work_item_id.clone());
                }
                bind_brief_to_assistant_round(
                    &mut brief,
                    outcome.final_text_source_assistant_round_id.as_deref(),
                );
                prepared.brief = Some(brief);
                outcome.terminal.no_brief_reason = None;
            } else if prepared.delivery == crate::tool::tools::wait_for::WaitForDeliveryArg::Silent
            {
                outcome.terminal.no_brief_reason = Some(TurnNoBriefReason::ToolOnlyWait);
            }
        } else if outcome.terminal_kind.is_failure() {
            let mut brief =
                brief::make_failure(&message.agent_id, message, outcome.final_text.clone());
            if !outcome.final_citations.is_empty() {
                brief.citations = Some(outcome.final_citations.clone());
            }
            brief.turn_index = Some(outcome.turn_index);
            bind_brief_to_assistant_round(
                &mut brief,
                outcome.final_text_source_assistant_round_id.as_deref(),
            );
            let delivery_started_at = chrono::Utc::now();
            self.persist_brief(&brief).await?;
            record_operator_span(
                trace_context.as_ref(),
                "holon.delivery",
                delivery_started_at,
                message,
            );
            outcome.terminal.no_brief_reason = None;
        } else if !outcome.final_text.trim().is_empty() {
            // Always generate the normal result brief (no longer suppressed for
            // promoted completion reports). The same turn supports multiple briefs,
            // and the normal brief and promoted completion reports serve different
            // purposes — the former records the turn-level operator delivery, the
            // latter records the work-item-level completion. A tool-only waiting turn
            // has no operator delivery, so it must not create an empty result brief.
            let mut brief =
                brief::make_result(&message.agent_id, message, outcome.final_text.clone());
            if !outcome.final_citations.is_empty() {
                brief.citations = Some(outcome.final_citations.clone());
            }
            brief.turn_index = Some(outcome.turn_index);
            bind_brief_to_assistant_round(
                &mut brief,
                outcome.final_text_source_assistant_round_id.as_deref(),
            );
            let delivery_started_at = chrono::Utc::now();
            self.persist_brief(&brief).await?;
            record_operator_span(
                trace_context.as_ref(),
                "holon.delivery",
                delivery_started_at,
                message,
            );
            outcome.terminal.no_brief_reason = None;
        }
        let mut turn_record = self.build_turn_record(&outcome.terminal).await?;
        if let Some(prepared) = outcome.prepared_work_item_completion.as_ref() {
            if !turn_record.produced_brief_ids.contains(&prepared.brief.id) {
                turn_record
                    .produced_brief_ids
                    .push(prepared.brief.id.clone());
            }
            if !turn_record
                .completed_work_item_ids
                .contains(&prepared.record.id)
            {
                turn_record
                    .completed_work_item_ids
                    .push(prepared.record.id.clone());
            }
            if let Some(tool_execution) = prepared.tool_execution.as_ref() {
                if !turn_record.tool_execution_ids.contains(&tool_execution.id) {
                    turn_record
                        .tool_execution_ids
                        .push(tool_execution.id.clone());
                }
            }
        }
        if let Some(prepared) = outcome.prepared_wait_for.as_mut() {
            prepared.brief_publication_scope = Some(crate::runtime::WaitForBriefPublicationScope {
                existing_brief_ids: turn_record.produced_brief_ids.clone(),
            });
            if let Some(brief) = prepared.brief.as_ref() {
                if !turn_record.produced_brief_ids.contains(&brief.id) {
                    turn_record.produced_brief_ids.push(brief.id.clone());
                }
            }
            if !turn_record
                .waiting_condition_ids
                .contains(&prepared.registration.condition.id)
            {
                turn_record
                    .waiting_condition_ids
                    .push(prepared.registration.condition.id.clone());
            }
            if let Some(tool_execution) = prepared.tool_execution.as_ref() {
                if !turn_record.tool_execution_ids.contains(&tool_execution.id) {
                    turn_record
                        .tool_execution_ids
                        .push(tool_execution.id.clone());
                }
            }
        }
        self.promote_turn_active_skills().await?;

        if outcome.should_sleep
            && outcome.prepared_work_item_completion.is_none()
            && outcome.prepared_wait_for.is_none()
        {
            if outcome.allow_sleep_runnable_work_override {
                self.transition_to_sleep(outcome.sleep_duration_ms).await?;
            } else {
                self.transition_to_sleep_with_runnable_override(outcome.sleep_duration_ms, false)
                    .await?;
            }
        }

        crate::diagnostics::record_turn_cleanup(cleanup_started.elapsed());
        record_operator_span(
            trace_context.as_ref(),
            "holon.turn.cleanup",
            cleanup_started_at,
            message,
        );
        let mut transition = TurnTerminalTransition {
            terminal: outcome.terminal,
            turn_record,
            prepared_work_item_completion: outcome.prepared_work_item_completion,
            prepared_wait_for: outcome.prepared_wait_for,
            terminal_tool_executions: outcome.terminal_tool_executions,
        };
        transition.normalize_brief_settlement();
        Ok(transition)
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
            .filter(|(_, tool)| {
                tool.name != crate::tool::names::X_SEARCH || self.x_search_config().is_some()
            })
            .filter(|(_, tool)| {
                tool.name != crate::tool::names::ADVISORY_DECISION
                    || self.advisory_decision_tool_config().0
            })
            .map(|(_, tool)| tool)
            .collect())
    }

    pub async fn preview_prompt(
        &self,
        text: String,
        authority_class: AuthorityClass,
    ) -> Result<EffectivePrompt> {
        let message = MessageEnvelope::new(
            self.agent_id().await?,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("debug_prompt".into()),
                actor_display_name: None,
            },
            authority_class.clone(),
            Priority::Normal,
            MessageBody::Text { text },
        )
        .with_admission(
            MessageDeliverySurface::CliPrompt,
            AdmissionContext::LocalProcess,
        );
        let mut agent = self.agent_state().await?;
        let _ = sync_agent_message_count(&self.inner.storage, &mut agent)?;
        let prior_closure = self.current_closure_decision().await?;
        let continuation = ContinuationTrigger::from_message(&message, None)
            .map(|trigger| resolve_continuation(&prior_closure, &trigger));
        let identity = self.agent_identity_view().await?;
        let default_external_ingress = self
            .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
            .await?;
        let default_external_ingress = self
            .inner
            .runtime_db
            .external_triggers()
            .latest(&default_external_ingress.external_trigger_id)?;
        let (provider, available_tools, apply_patch_surface, _, _) =
            self.provider_tool_selection(&identity).await?;
        let prompt_tools = provider.prompt_tool_specs(&available_tools);
        let execution = self.execution_snapshot().await?;
        let loaded_agents_md = self.loaded_agents_md_for_state(&agent)?;
        let loaded_agent_memory = self.loaded_agent_memory_for_state()?;
        let skills = self
            .skills_runtime_view_for_state(&agent, &identity)
            .await?;
        let context_config = self.current_context_config().await;
        build_effective_prompt_with_apply_patch_surface_and_default_external_ingress(
            &self.inner.storage,
            &agent,
            &execution,
            &message,
            &context_config,
            &execution.execution_root,
            self.agent_home().as_path(),
            &identity,
            loaded_agents_md,
            loaded_agent_memory,
            &skills,
            &prompt_tools,
            apply_patch_surface,
            continuation.as_ref(),
            default_external_ingress.as_ref(),
        )
    }

    pub(super) async fn build_subagent_prompt_for_workspace(
        &self,
        agent_id: &str,
        prompt: &str,
        authority_class: &AuthorityClass,
        execution: &EffectiveExecution,
    ) -> Result<EffectivePrompt> {
        let message = MessageEnvelope::new(
            agent_id.to_string(),
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "subagent".into(),
            },
            authority_class.clone(),
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
        let loaded_agent_memory = load_agent_memory(self.agent_home().as_path())?;
        let state = self
            .inner
            .storage
            .read_agent()?
            .unwrap_or_else(|| AgentState::new(agent_id.to_string()));
        let name = match self.inner.host_bridge.as_ref() {
            Some(bridge) => bridge
                .identity_for_agent(agent_id)
                .await?
                .and_then(|identity| identity.name),
            None => None,
        };
        let identity = AgentIdentityView {
            agent_id: agent_id.to_string(),
            name,
            kind: AgentKind::Child,
            visibility: crate::types::AgentVisibility::Private,
            ownership: crate::types::AgentOwnership::ParentSupervised,
            profile_preset: crate::types::AgentProfilePreset::PrivateChild,
            can_rename: false,
            status: crate::types::AgentRegistryStatus::Active,
            is_default_agent: false,
            incarnation: 1,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        };
        let skills = self
            .skills_runtime_view_for_state(&state, &identity)
            .await?;
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
        build_effective_prompt_with_apply_patch_surface(
            &self.inner.storage,
            &AgentState::new(agent_id.to_string()),
            &execution.snapshot(),
            &message,
            &context_config,
            execution.workspace.execution_root(),
            self.agent_home().as_path(),
            &identity,
            loaded_agents_md,
            loaded_agent_memory,
            &skills,
            &[],
            ApplyPatchSurface::UnifiedDiffJson,
            continuation.as_ref(),
        )
    }

    pub(super) async fn provider_tool_selection(
        &self,
        identity: &AgentIdentityView,
    ) -> Result<(
        Arc<dyn AgentProvider>,
        Vec<ToolSpec>,
        ApplyPatchSurface,
        Option<ProviderNativeWebSearchRequest>,
        BuiltinWebSearchSelectionDiagnostics,
    )> {
        self.provider_tool_selection_for_turn(identity, None).await
    }

    pub(super) async fn provider_tool_selection_for_turn(
        &self,
        identity: &AgentIdentityView,
        fallback_model: Option<&ModelRouteRef>,
    ) -> Result<(
        Arc<dyn AgentProvider>,
        Vec<ToolSpec>,
        ApplyPatchSurface,
        Option<ProviderNativeWebSearchRequest>,
        BuiltinWebSearchSelectionDiagnostics,
    )> {
        let provider = self.current_provider().await;
        let web_config = self.web_config();
        let native_search_provider = web_config.native_search_provider();
        let native_web_search_selection = native_web_search_request_for_config(
            provider.builtin_web_search(),
            native_search_provider.as_ref(),
            &web_config.search,
        );
        let native_web_search = native_web_search_selection.request;
        let apply_patch_surface = {
            let guard = self.inner.agent.lock().await;
            self.apply_patch_surface_for_turn(&guard.state, fallback_model)
        };
        let available_tools = filter_native_web_search_tools(
            self.filtered_tool_specs_for_apply_patch_surface(identity, apply_patch_surface)?,
            native_web_search.is_some(),
        );
        Ok((
            provider,
            available_tools,
            apply_patch_surface,
            native_web_search,
            native_web_search_selection.diagnostics,
        ))
    }
}

fn filter_native_web_search_tools(
    tools: Vec<ToolSpec>,
    native_search_configured: bool,
) -> Vec<ToolSpec> {
    // Managed WebSearch is always available alongside native search tools.
    // Native search uses different tool names (e.g. web_search_preview), so
    // there is no conflict — the agent can choose which search surface to use.
    let _ = native_search_configured;
    tools
}

fn validate_builtin_web_search_capability(
    capability: &crate::provider::ProviderBuiltinWebSearchCapability,
) -> Result<()> {
    if capability.provider_model_ref.trim().is_empty() {
        anyhow::bail!("builtin web search capability has empty provider model ref",);
    }
    if capability.provider_transport.trim().is_empty() {
        anyhow::bail!("builtin web search capability has empty provider transport",);
    }
    if capability.provider_base_url.trim().is_empty() {
        anyhow::bail!("builtin web search capability has empty provider base URL",);
    }
    if capability.advertised_tool_type.trim().is_empty() {
        anyhow::bail!("builtin web search capability has empty advertised tool type",);
    }
    if capability.backend_kind.trim().is_empty() {
        anyhow::bail!("builtin web search capability has empty backend kind",);
    }

    let locally_supported = match (
        capability.kind,
        capability.provider_transport.as_str(),
        capability.advertised_tool_type.as_str(),
    ) {
        (ProviderNativeWebSearchKind::OpenAi, "openai_responses", "web_search_preview")
        | (ProviderNativeWebSearchKind::OpenAi, "openai_codex_responses", "web_search")
        | (ProviderNativeWebSearchKind::DeepSeek, "openai_responses", "web_search")
        | (ProviderNativeWebSearchKind::DeepSeek, "openai_responses", "web_search_2025_08_26")
        | (ProviderNativeWebSearchKind::DeepSeek, "anthropic_messages", "web_search_20250305")
        | (ProviderNativeWebSearchKind::Anthropic, "anthropic_messages", "web_search_20250305") => {
            true
        }
        _ => false,
    };

    if !locally_supported {
        anyhow::bail!(
            "builtin web search capability is incompatible with transport {} and advertised tool type {}",
            capability.provider_transport, capability.advertised_tool_type
        );
    }

    Ok(())
}

fn native_web_search_request_for_config(
    provider_capability: Option<crate::provider::ProviderBuiltinWebSearchCapability>,
    native_search_provider: Option<&(String, WebProviderKind)>,
    web_search: &crate::web::WebSearchConfig,
) -> BuiltinWebSearchSelection {
    let Some(capability) = provider_capability else {
        return builtin_web_search_selection(
            None,
            BuiltinWebSearchSelectionStatus::NotDeclared,
            "active provider does not declare builtin web search",
        );
    };

    if !web_search.enabled {
        return builtin_web_search_selection(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::Disabled,
            "web.search.enabled is false",
        );
    }

    let explicit_native = native_search_provider.and_then(|(provider_id, provider_kind)| {
        provider_kind
            .is_native_search()
            .then_some((provider_id, *provider_kind))
    });
    let explicit_non_auto_provider = web_search.provider.trim() != "auto";
    let (provider_id, required_kind, selection_reason) =
        if let Some((provider_id, provider_kind)) = explicit_native {
            (
                provider_id.clone(),
                provider_kind_to_native_web_search_kind(provider_kind),
                "explicit native web search provider",
            )
        } else if explicit_non_auto_provider {
            return builtin_web_search_selection(
                Some(&capability),
                BuiltinWebSearchSelectionStatus::NotRequested,
                "web.search.provider explicitly selects managed WebSearch",
            );
        } else if web_search.builtin_provider_enabled {
            (
                capability.provider_id.clone(),
                Some(capability.kind),
                "provider-declared builtin web search default",
            )
        } else {
            return builtin_web_search_selection(
                Some(&capability),
                BuiltinWebSearchSelectionStatus::Disabled,
                "web.search.builtin_provider.enabled is false",
            );
        };

    if required_kind != Some(capability.kind) {
        return builtin_web_search_selection(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::NotRequested,
            "configured native web search provider kind does not match active provider capability",
        );
    }

    if let Err(error) = validate_builtin_web_search_capability(&capability) {
        return builtin_web_search_selection(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::Unsupported,
            &format!(
                "native builtin web search configuration is invalid: {error}; update the provider, model, endpoint, or web.search configuration",
            ),
        );
    }

    let request = ProviderNativeWebSearchRequest {
        kind: capability.kind,
        provider_id,
        provider_model_ref: capability.provider_model_ref.clone(),
        advertised_tool_type: capability.advertised_tool_type.clone(),
        backend_kind: capability.backend_kind.clone(),
        max_results: Some(web_search.max_results.max(1)),
    };
    BuiltinWebSearchSelection {
        request: Some(request),
        diagnostics: builtin_web_search_selection_diagnostics(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::Selected,
            Some(selection_reason.into()),
        ),
    }
}

fn provider_kind_to_native_web_search_kind(
    kind: WebProviderKind,
) -> Option<ProviderNativeWebSearchKind> {
    match kind {
        WebProviderKind::OpenAiNative => Some(ProviderNativeWebSearchKind::OpenAi),
        WebProviderKind::AnthropicNative => Some(ProviderNativeWebSearchKind::Anthropic),
        WebProviderKind::GeminiNative => Some(ProviderNativeWebSearchKind::Gemini),
        _ => None,
    }
}

fn builtin_web_search_selection(
    capability: Option<&crate::provider::ProviderBuiltinWebSearchCapability>,
    status: BuiltinWebSearchSelectionStatus,
    reason: &str,
) -> BuiltinWebSearchSelection {
    BuiltinWebSearchSelection {
        request: None,
        diagnostics: builtin_web_search_selection_diagnostics(
            capability,
            status,
            Some(reason.into()),
        ),
    }
}

fn builtin_web_search_selection_diagnostics(
    capability: Option<&crate::provider::ProviderBuiltinWebSearchCapability>,
    status: BuiltinWebSearchSelectionStatus,
    reason: Option<String>,
) -> BuiltinWebSearchSelectionDiagnostics {
    BuiltinWebSearchSelectionDiagnostics {
        status,
        reason,
        provider_id: capability.map(|capability| capability.provider_id.clone()),
        provider_model_ref: capability.map(|capability| capability.provider_model_ref.clone()),
        provider_transport: capability.map(|capability| capability.provider_transport.clone()),
        provider_base_url: capability.map(|capability| {
            crate::provider::sanitize_transport_url(&capability.provider_base_url)
        }),
        advertised_tool_type: capability.map(|capability| capability.advertised_tool_type.clone()),
        backend_kind: capability.map(|capability| capability.backend_kind.clone()),
    }
}

fn bind_brief_to_assistant_round(brief: &mut BriefRecord, entry_id: Option<&str>) {
    if let Some(entry_id) = entry_id {
        brief.finalizes_assistant_round_id = Some(entry_id.to_string());
        brief.content_source = crate::types::BriefContentSource::TranscriptEntry {
            entry_id: entry_id.to_string(),
            relation: crate::types::BriefContentSourceRelation::Finalizes,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability(
        kind: ProviderNativeWebSearchKind,
        provider_id: &str,
        model_ref: &str,
        transport: &str,
        tool_type: &str,
        backend_kind: &str,
    ) -> crate::provider::ProviderBuiltinWebSearchCapability {
        crate::provider::ProviderBuiltinWebSearchCapability {
            kind,
            provider_id: provider_id.into(),
            provider_model_ref: model_ref.into(),
            provider_transport: transport.into(),
            provider_base_url: "https://api.example.test".into(),
            advertised_tool_type: tool_type.into(),
            backend_kind: backend_kind.into(),
        }
    }

    fn search_config() -> crate::web::WebSearchConfig {
        crate::web::WebSearchConfig::default()
    }

    fn tool_spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: "test tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
            freeform_grammar: None,
        }
    }

    #[test]
    fn managed_web_search_tool_stays_visible_when_builtin_search_not_selected() {
        let tools = filter_native_web_search_tools(
            vec![
                tool_spec(crate::tool::tools::web_search::NAME),
                tool_spec("read_file"),
            ],
            false,
        );

        assert!(tools
            .iter()
            .any(|tool| tool.name == crate::tool::tools::web_search::NAME));
    }

    #[test]
    fn managed_web_search_tool_stays_visible_when_builtin_search_selected() {
        let tools = filter_native_web_search_tools(
            vec![
                tool_spec(crate::tool::tools::web_search::NAME),
                tool_spec("read_file"),
            ],
            true,
        );

        assert!(tools
            .iter()
            .any(|tool| tool.name == crate::tool::tools::web_search::NAME));
        assert!(tools.iter().any(|tool| tool.name == "read_file"));
    }

    #[test]
    fn native_web_search_request_uses_static_capability_without_probe() {
        let native_provider = ("openai-native".to_string(), WebProviderKind::OpenAiNative);
        let selection = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::OpenAi,
                "openai-native",
                "openai/gpt-test",
                "openai_responses",
                "web_search_preview",
                "openai_web_search",
            )),
            Some(&native_provider),
            &search_config(),
        );

        let request = selection.request.expect("static capability should select");
        assert_eq!(request.kind, ProviderNativeWebSearchKind::OpenAi);
        assert_eq!(request.advertised_tool_type, "web_search_preview");
    }

    #[test]
    fn native_web_search_request_rejects_invalid_configuration_without_fallback() {
        let native_provider = ("openai-native".to_string(), WebProviderKind::OpenAiNative);
        let selection = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::OpenAi,
                "openai-native",
                "openai/gpt-test",
                "unsupported_transport",
                "web_search_preview",
                "openai_web_search",
            )),
            Some(&native_provider),
            &search_config(),
        );

        assert!(selection.request.is_none());
        assert_eq!(
            selection.diagnostics.status,
            BuiltinWebSearchSelectionStatus::Unsupported
        );
        let reason = selection.diagnostics.reason.expect("configuration error");
        assert!(reason.contains("update the provider, model, endpoint"));
    }

    #[test]
    fn native_web_search_request_requires_matching_model_provider() {
        let native_provider = ("openai-native".to_string(), WebProviderKind::OpenAiNative);
        let selection = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::Anthropic,
                "anthropic",
                "anthropic/claude-test",
                "anthropic_messages",
                "web_search_20250305",
                "anthropic_web_search",
            )),
            Some(&native_provider),
            &search_config(),
        );

        assert!(selection.request.is_none());
        assert_eq!(
            selection.diagnostics.status,
            BuiltinWebSearchSelectionStatus::NotRequested
        );
    }

    #[test]
    fn native_web_search_request_clamps_max_results() {
        let native_provider = ("openai-native".to_string(), WebProviderKind::OpenAiNative);
        let mut config = search_config();
        config.max_results = 0;
        let selection = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::OpenAi,
                "openai-native",
                "openai/gpt-test",
                "openai_responses",
                "web_search_preview",
                "openai_web_search",
            )),
            Some(&native_provider),
            &config,
        );

        assert_eq!(
            selection.request.expect("valid capability").max_results,
            Some(1)
        );
    }

    #[test]
    fn builtin_web_search_selection_diagnostics_sanitizes_base_url() {
        let mut capability = capability(
            ProviderNativeWebSearchKind::OpenAi,
            "openai-codex",
            "openai-codex/gpt-codex-test",
            "openai_codex_responses",
            "web_search",
            "openai_codex_web_search",
        );
        capability.provider_base_url =
            "https://user:secret@example.test/path?token=abc#frag".into();

        let diagnostics = builtin_web_search_selection_diagnostics(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::Selected,
            None,
        );

        assert_eq!(
            diagnostics.provider_base_url.as_deref(),
            Some("https://example.test/path")
        );
    }
}
