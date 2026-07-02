use super::*;
use crate::tool::{ApplyPatchSurface, ToolSpec};

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
        self.inner
            .storage
            .append_event(&brief::make_acknowledgement_event(message))?;
        let context_build_started = std::time::Instant::now();
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
            let (provider, available_tools, apply_patch_surface, _, _) =
                self.provider_tool_selection(&identity).await?;
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
        let outcome = self
            .run_agent_loop(
                &message.agent_id,
                message.authority_class.clone(),
                built,
                loop_control,
            )
            .await?;
        crate::diagnostics::record_turn_total(context_build_started.elapsed());
        let cleanup_started = std::time::Instant::now();

        if outcome.terminal_kind.is_failure() {
            let mut brief =
                brief::make_failure(&message.agent_id, message, outcome.final_text.clone());
            brief.turn_index = Some(outcome.turn_index);
            bind_brief_to_assistant_round(
                &mut brief,
                outcome.final_text_source_assistant_round_id.as_deref(),
            );
            self.persist_brief(&brief).await?;
        } else {
            // Always generate the normal result brief (no longer suppressed for
            // promoted completion reports). The same turn supports multiple briefs,
            // and the normal brief and promoted completion reports serve different
            // purposes — the former records the turn-level result, the latter records
            // the work-item-level completion.
            let mut brief =
                brief::make_result(&message.agent_id, message, outcome.final_text.clone());
            brief.turn_index = Some(outcome.turn_index);
            bind_brief_to_assistant_round(
                &mut brief,
                outcome.final_text_source_assistant_round_id.as_deref(),
            );
            self.persist_brief(&brief).await?;
        }
        self.persist_turn_record(&outcome.terminal).await?;
        self.promote_turn_active_skills().await?;

        if outcome.should_sleep {
            if outcome.allow_sleep_runnable_work_override {
                self.transition_to_sleep(outcome.sleep_duration_ms).await?;
            } else {
                self.transition_to_sleep_with_runnable_override(outcome.sleep_duration_ms, false)
                    .await?;
            }
        }

        crate::diagnostics::record_turn_cleanup(cleanup_started.elapsed());
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
            .map(|trigger| resolve_continuation(&prior_closure, &trigger, None));
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
                None,
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
        let provider = self.current_provider().await;
        let web_config = self.web_config();
        let native_search_provider = web_config.native_search_provider();
        let builtin_capability = provider.builtin_web_search();
        let probe_result = if let Some(capability) = builtin_capability.as_ref() {
            let probe_key = BuiltinWebSearchProbeKey::from_capability(capability);
            let cached_probe = {
                let cache = self.inner.builtin_web_search_probe_cache.lock().await;
                cache.get(&probe_key).cloned()
            };
            if let Some(cached_probe) = cached_probe {
                cached_probe
            } else if !builtin_web_search_probe_requested(
                capability,
                native_search_provider.as_ref(),
                &web_config.search,
            ) {
                BuiltinWebSearchProbeCacheEntry {
                    status: BuiltinWebSearchProbeStatus::Skipped,
                    reason: Some("builtin web search is not requested by current config".into()),
                }
            } else {
                probe_builtin_web_search_capability(
                    provider.as_ref(),
                    capability,
                    web_config.search.max_results,
                )
                .await
            }
        } else {
            BuiltinWebSearchProbeCacheEntry {
                status: BuiltinWebSearchProbeStatus::Skipped,
                reason: Some("active provider does not declare builtin web search".into()),
            }
        };
        let native_web_search_selection = {
            let mut cache = self.inner.builtin_web_search_probe_cache.lock().await;
            native_web_search_request_for_config(
                builtin_capability,
                native_search_provider.as_ref(),
                &web_config.search,
                &mut cache,
                probe_result,
            )
        };
        let native_web_search = native_web_search_selection.request;
        let apply_patch_surface = {
            let guard = self.inner.agent.lock().await;
            self.apply_patch_surface_for_state(&guard.state)
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

async fn probe_builtin_web_search_capability(
    provider: &dyn AgentProvider,
    capability: &crate::provider::ProviderBuiltinWebSearchCapability,
    max_results: usize,
) -> BuiltinWebSearchProbeCacheEntry {
    if capability.provider_model_ref.trim().is_empty() {
        return unsupported_builtin_web_search_probe(
            "builtin web search capability has empty provider model ref",
        );
    }
    if capability.provider_transport.trim().is_empty() {
        return unsupported_builtin_web_search_probe(
            "builtin web search capability has empty provider transport",
        );
    }
    if capability.provider_base_url.trim().is_empty() {
        return unsupported_builtin_web_search_probe(
            "builtin web search capability has empty provider base URL",
        );
    }
    if capability.advertised_tool_type.trim().is_empty() {
        return unsupported_builtin_web_search_probe(
            "builtin web search capability has empty advertised tool type",
        );
    }
    if capability.backend_kind.trim().is_empty() {
        return unsupported_builtin_web_search_probe(
            "builtin web search capability has empty backend kind",
        );
    }

    let locally_supported = match (
        capability.kind,
        capability.provider_transport.as_str(),
        capability.advertised_tool_type.as_str(),
    ) {
        (ProviderNativeWebSearchKind::OpenAi, "openai_responses", "web_search_preview")
        | (ProviderNativeWebSearchKind::OpenAi, "openai_codex_responses", "web_search")
        | (ProviderNativeWebSearchKind::Anthropic, "anthropic_messages", "web_search_20250305") => {
            true
        }
        _ => false,
    };

    if !locally_supported {
        return unsupported_builtin_web_search_probe(format!(
            "builtin web search capability is incompatible with transport {} and advertised tool type {}",
            capability.provider_transport, capability.advertised_tool_type
        ));
    }

    let request = ProviderNativeWebSearchRequest {
        kind: capability.kind,
        provider_id: capability.provider_id.clone(),
        provider_model_ref: capability.provider_model_ref.clone(),
        advertised_tool_type: capability.advertised_tool_type.clone(),
        backend_kind: capability.backend_kind.clone(),
        max_results: Some(max_results.max(1)),
    };

    match provider.probe_builtin_web_search(request).await {
        Ok(()) => BuiltinWebSearchProbeCacheEntry {
            status: BuiltinWebSearchProbeStatus::Supported,
            reason: None,
        },
        Err(error) => {
            let classification = crate::provider::classify_provider_error(&error);
            let status = if classification.disposition.as_str() == "retryable" {
                BuiltinWebSearchProbeStatus::TransientFailure
            } else {
                BuiltinWebSearchProbeStatus::Unsupported
            };
            BuiltinWebSearchProbeCacheEntry {
                status,
                reason: Some(format!("builtin web search provider probe failed: {error}")),
            }
        }
    }
}

fn unsupported_builtin_web_search_probe(
    reason: impl Into<String>,
) -> BuiltinWebSearchProbeCacheEntry {
    BuiltinWebSearchProbeCacheEntry {
        status: BuiltinWebSearchProbeStatus::Unsupported,
        reason: Some(reason.into()),
    }
}

fn builtin_web_search_probe_requested(
    capability: &crate::provider::ProviderBuiltinWebSearchCapability,
    native_search_provider: Option<&(String, WebProviderKind)>,
    web_search: &crate::web::WebSearchConfig,
) -> bool {
    if !web_search.enabled {
        return false;
    }

    if let Some((_, provider_kind)) = native_search_provider {
        if provider_kind.is_native_search() {
            return provider_kind_to_native_web_search_kind(*provider_kind)
                == Some(capability.kind);
        }
    }

    if web_search.provider.trim() != "auto" {
        return false;
    }

    web_search.builtin_provider_enabled
}

fn native_web_search_request_for_config(
    provider_capability: Option<crate::provider::ProviderBuiltinWebSearchCapability>,
    native_search_provider: Option<&(String, WebProviderKind)>,
    web_search: &crate::web::WebSearchConfig,
    probe_cache: &mut HashMap<BuiltinWebSearchProbeKey, BuiltinWebSearchProbeCacheEntry>,
    probe_result: BuiltinWebSearchProbeCacheEntry,
) -> BuiltinWebSearchSelection {
    let Some(capability) = provider_capability else {
        return builtin_web_search_selection(
            None,
            BuiltinWebSearchSelectionStatus::NotDeclared,
            "active provider does not declare builtin web search",
            BuiltinWebSearchProbeStatus::Skipped,
            false,
        );
    };

    if !web_search.enabled {
        return builtin_web_search_selection(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::Disabled,
            "web.search.enabled is false",
            BuiltinWebSearchProbeStatus::Skipped,
            false,
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
                BuiltinWebSearchProbeStatus::Skipped,
                false,
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
                BuiltinWebSearchProbeStatus::Skipped,
                false,
            );
        };

    if required_kind != Some(capability.kind) {
        return builtin_web_search_selection(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::NotRequested,
            "configured native web search provider kind does not match active provider capability",
            BuiltinWebSearchProbeStatus::Skipped,
            false,
        );
    }

    let probe_key = BuiltinWebSearchProbeKey::from_capability(&capability);
    let (probe, cache_hit) = if let Some(cached) = probe_cache.get(&probe_key).cloned() {
        (cached, true)
    } else {
        if matches!(
            probe_result.status,
            BuiltinWebSearchProbeStatus::Supported | BuiltinWebSearchProbeStatus::Unsupported
        ) {
            probe_cache.insert(probe_key, probe_result.clone());
        }
        (probe_result, false)
    };

    match probe.status {
        BuiltinWebSearchProbeStatus::Supported => {
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
                    BuiltinWebSearchProbeStatus::Supported,
                    cache_hit,
                ),
            }
        }
        BuiltinWebSearchProbeStatus::Unsupported => builtin_web_search_selection(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::Unsupported,
            probe
                .reason
                .as_deref()
                .unwrap_or("builtin web search probe reported unsupported"),
            BuiltinWebSearchProbeStatus::Unsupported,
            cache_hit,
        ),
        BuiltinWebSearchProbeStatus::TransientFailure => builtin_web_search_selection(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::TransientProbeFailure,
            probe
                .reason
                .as_deref()
                .unwrap_or("builtin web search probe failed transiently"),
            BuiltinWebSearchProbeStatus::TransientFailure,
            cache_hit,
        ),
        BuiltinWebSearchProbeStatus::Skipped => builtin_web_search_selection(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::NotRequested,
            probe
                .reason
                .as_deref()
                .unwrap_or("builtin web search probe skipped"),
            BuiltinWebSearchProbeStatus::Skipped,
            cache_hit,
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
    probe_status: BuiltinWebSearchProbeStatus,
    probe_cache_hit: bool,
) -> BuiltinWebSearchSelection {
    BuiltinWebSearchSelection {
        request: None,
        diagnostics: builtin_web_search_selection_diagnostics(
            capability,
            status,
            Some(reason.into()),
            probe_status,
            probe_cache_hit,
        ),
    }
}

fn builtin_web_search_selection_diagnostics(
    capability: Option<&crate::provider::ProviderBuiltinWebSearchCapability>,
    status: BuiltinWebSearchSelectionStatus,
    reason: Option<String>,
    probe_status: BuiltinWebSearchProbeStatus,
    probe_cache_hit: bool,
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
        probe_status,
        probe_cache_hit,
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

    struct ProbeProvider {
        result: std::result::Result<(), &'static str>,
    }

    #[async_trait::async_trait]
    impl crate::provider::AgentProvider for ProbeProvider {
        async fn complete_turn(
            &self,
            _request: crate::provider::ProviderTurnRequest,
        ) -> Result<crate::provider::ProviderTurnResponse> {
            Ok(crate::provider::ProviderTurnResponse {
                blocks: vec![crate::provider::ModelBlock::Text { text: "OK".into() }],
                stop_reason: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            })
        }

        async fn probe_builtin_web_search(
            &self,
            _request: ProviderNativeWebSearchRequest,
        ) -> Result<()> {
            match self.result {
                Ok(()) => Ok(()),
                Err("transient") => Err(crate::provider::provider_transport_error(
                    crate::provider::ProviderFailureClassification {
                        kind: crate::provider::ProviderFailureKind::RateLimited,
                        disposition: crate::provider::RetryDisposition::Retryable,
                    },
                    Some(429),
                    None,
                    "rate limited",
                )),
                Err(message) => Err(anyhow::anyhow!(message)),
            }
        }
    }

    fn capability(
        kind: ProviderNativeWebSearchKind,
        provider_id: &str,
        model_ref: &str,
        tool_type: &str,
        backend_kind: &str,
    ) -> crate::provider::ProviderBuiltinWebSearchCapability {
        crate::provider::ProviderBuiltinWebSearchCapability {
            kind,
            provider_id: provider_id.into(),
            provider_model_ref: model_ref.into(),
            provider_transport: "test_transport".into(),
            provider_base_url: "https://api.example.test".into(),
            advertised_tool_type: tool_type.into(),
            backend_kind: backend_kind.into(),
        }
    }

    fn search_config() -> crate::web::WebSearchConfig {
        crate::web::WebSearchConfig::default()
    }

    fn probe(status: BuiltinWebSearchProbeStatus) -> BuiltinWebSearchProbeCacheEntry {
        BuiltinWebSearchProbeCacheEntry {
            status,
            reason: (!matches!(status, BuiltinWebSearchProbeStatus::Supported))
                .then(|| "probe result".into()),
        }
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
    fn native_web_search_request_requires_matching_model_provider() {
        let native_provider = ("openai-native".to_string(), WebProviderKind::OpenAiNative);
        let mut cache = HashMap::new();

        let selection = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::Anthropic,
                "anthropic",
                "anthropic/claude-test",
                "web_search_20250305",
                "anthropic_web_search",
            )),
            Some(&native_provider),
            &search_config(),
            &mut cache,
            probe(BuiltinWebSearchProbeStatus::Supported),
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
        let mut cache = HashMap::new();

        let request = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::OpenAi,
                "openai",
                "openai/gpt-test",
                "web_search_preview",
                "openai_web_search",
            )),
            Some(&native_provider),
            &config,
            &mut cache,
            probe(BuiltinWebSearchProbeStatus::Supported),
        )
        .request
        .unwrap();

        assert_eq!(request.max_results, Some(1));
        assert_eq!(request.provider_model_ref, "openai/gpt-test");
        assert_eq!(request.advertised_tool_type, "web_search_preview");
        assert_eq!(request.backend_kind, "openai_web_search");
    }

    #[test]
    fn native_web_search_request_uses_codex_provider_capability_metadata() {
        let native_provider = ("openai-native".to_string(), WebProviderKind::OpenAiNative);
        let mut cache = HashMap::new();

        let request = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::OpenAi,
                "openai-codex",
                "openai-codex/gpt-codex-test",
                "web_search",
                "openai_codex_web_search",
            )),
            Some(&native_provider),
            &search_config(),
            &mut cache,
            probe(BuiltinWebSearchProbeStatus::Supported),
        )
        .request
        .unwrap();

        assert_eq!(request.provider_id, "openai-native");
        assert_eq!(request.provider_model_ref, "openai-codex/gpt-codex-test");
        assert_eq!(request.advertised_tool_type, "web_search");
        assert_eq!(request.backend_kind, "openai_codex_web_search");
    }

    #[test]
    fn native_web_search_request_defaults_to_declared_provider_capability() {
        let mut cache = HashMap::new();

        let selection = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::OpenAi,
                "openai-codex",
                "openai-codex/gpt-codex-test",
                "web_search",
                "openai_codex_web_search",
            )),
            None,
            &search_config(),
            &mut cache,
            probe(BuiltinWebSearchProbeStatus::Supported),
        );
        let request = selection.request.unwrap();

        assert_eq!(request.provider_id, "openai-codex");
        assert_eq!(request.advertised_tool_type, "web_search");
        assert_eq!(
            selection.diagnostics.status,
            BuiltinWebSearchSelectionStatus::Selected
        );
        assert!(!selection.diagnostics.probe_cache_hit);
    }

    #[test]
    fn native_web_search_request_respects_global_builtin_disable() {
        let mut config = search_config();
        config.builtin_provider_enabled = false;
        let mut cache = HashMap::new();

        let selection = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::OpenAi,
                "openai-codex",
                "openai-codex/gpt-codex-test",
                "web_search",
                "openai_codex_web_search",
            )),
            None,
            &config,
            &mut cache,
            probe(BuiltinWebSearchProbeStatus::Supported),
        );

        assert!(selection.request.is_none());
        assert_eq!(
            selection.diagnostics.status,
            BuiltinWebSearchSelectionStatus::Disabled
        );
    }

    #[test]
    fn native_web_search_request_uses_cached_unsupported_probe_decision() {
        let mut cache = HashMap::new();
        let capability = capability(
            ProviderNativeWebSearchKind::OpenAi,
            "openai-codex",
            "openai-codex/gpt-codex-test",
            "web_search",
            "openai_codex_web_search",
        );
        let key = BuiltinWebSearchProbeKey::from_capability(&capability);
        cache.insert(key, probe(BuiltinWebSearchProbeStatus::Unsupported));

        let selection = native_web_search_request_for_config(
            Some(capability),
            None,
            &search_config(),
            &mut cache,
            probe(BuiltinWebSearchProbeStatus::Supported),
        );

        assert!(selection.request.is_none());
        assert_eq!(
            selection.diagnostics.status,
            BuiltinWebSearchSelectionStatus::Unsupported
        );
        assert!(selection.diagnostics.probe_cache_hit);
    }

    #[tokio::test]
    async fn builtin_web_search_probe_rejects_invalid_contract_without_backend_call() {
        let mut cache = HashMap::new();
        let unsupported_capability = capability(
            ProviderNativeWebSearchKind::OpenAi,
            "openai-codex",
            "openai-codex/gpt-codex-test",
            "web_search_preview",
            "openai_codex_web_search",
        );
        let probe_result = probe_builtin_web_search_capability(
            &ProbeProvider { result: Ok(()) },
            &unsupported_capability,
            search_config().max_results,
        )
        .await;

        let selection = native_web_search_request_for_config(
            Some(unsupported_capability),
            None,
            &search_config(),
            &mut cache,
            probe_result,
        );

        assert!(selection.request.is_none());
        assert_eq!(
            selection.diagnostics.status,
            BuiltinWebSearchSelectionStatus::Unsupported
        );
        assert_eq!(
            selection.diagnostics.probe_status,
            BuiltinWebSearchProbeStatus::Unsupported
        );
        assert!(!selection.diagnostics.probe_cache_hit);
    }

    #[tokio::test]
    async fn builtin_web_search_probe_uses_backend_result() {
        let capability = crate::provider::ProviderBuiltinWebSearchCapability {
            kind: ProviderNativeWebSearchKind::OpenAi,
            provider_id: "openai-codex".into(),
            provider_model_ref: "openai-codex/gpt-codex-test".into(),
            provider_transport: "openai_codex_responses".into(),
            provider_base_url: "https://api.example.test".into(),
            advertised_tool_type: "web_search".into(),
            backend_kind: "openai_codex_web_search".into(),
        };

        let supported = probe_builtin_web_search_capability(
            &ProbeProvider { result: Ok(()) },
            &capability,
            search_config().max_results,
        )
        .await;
        let unsupported = probe_builtin_web_search_capability(
            &ProbeProvider {
                result: Err("unsupported"),
            },
            &capability,
            search_config().max_results,
        )
        .await;
        let transient = probe_builtin_web_search_capability(
            &ProbeProvider {
                result: Err("transient"),
            },
            &capability,
            search_config().max_results,
        )
        .await;

        assert_eq!(supported.status, BuiltinWebSearchProbeStatus::Supported);
        assert_eq!(unsupported.status, BuiltinWebSearchProbeStatus::Unsupported);
        assert_eq!(
            transient.status,
            BuiltinWebSearchProbeStatus::TransientFailure
        );
    }

    #[test]
    fn native_web_search_request_does_not_cache_transient_probe_failure() {
        let mut cache = HashMap::new();

        let first = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::OpenAi,
                "openai-codex",
                "openai-codex/gpt-codex-test",
                "web_search",
                "openai_codex_web_search",
            )),
            None,
            &search_config(),
            &mut cache,
            probe(BuiltinWebSearchProbeStatus::TransientFailure),
        );
        let second = native_web_search_request_for_config(
            Some(capability(
                ProviderNativeWebSearchKind::OpenAi,
                "openai-codex",
                "openai-codex/gpt-codex-test",
                "web_search",
                "openai_codex_web_search",
            )),
            None,
            &search_config(),
            &mut cache,
            probe(BuiltinWebSearchProbeStatus::Supported),
        );

        assert!(first.request.is_none());
        assert_eq!(
            first.diagnostics.status,
            BuiltinWebSearchSelectionStatus::TransientProbeFailure
        );
        assert!(second.request.is_some());
        assert!(!second.diagnostics.probe_cache_hit);
    }

    #[test]
    fn builtin_web_search_selection_diagnostics_sanitizes_base_url() {
        let capability = crate::provider::ProviderBuiltinWebSearchCapability {
            kind: ProviderNativeWebSearchKind::OpenAi,
            provider_id: "openai-codex".into(),
            provider_model_ref: "openai-codex/gpt-codex-test".into(),
            provider_transport: "openai_codex_responses".into(),
            provider_base_url: "https://user:secret@example.test/path?token=abc#frag".into(),
            advertised_tool_type: "web_search".into(),
            backend_kind: "openai_codex_web_search".into(),
        };

        let diagnostics = builtin_web_search_selection_diagnostics(
            Some(&capability),
            BuiltinWebSearchSelectionStatus::Selected,
            None,
            BuiltinWebSearchProbeStatus::Supported,
            false,
        );

        assert_eq!(
            diagnostics.provider_base_url.as_deref(),
            Some("https://example.test/path")
        );
    }
}
