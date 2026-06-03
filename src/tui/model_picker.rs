use crate::types::{AgentSummary, ResolvedModelAvailability};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ModelPickerChoice {
    InheritDefault,
    Provider {
        provider: String,
    },
    Model {
        model: String,
        supports_reasoning_effort: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelPickerRow {
    pub(super) choice: ModelPickerChoice,
    pub(super) title: String,
    pub(super) detail: String,
    pub(super) searchable: String,
    pub(super) available: bool,
}

pub(super) fn model_picker_rows(
    agent: Option<&AgentSummary>,
    model_availability: &[ResolvedModelAvailability],
    provider: Option<&str>,
    filter: &str,
) -> Vec<ModelPickerRow> {
    let Some(agent) = agent else {
        return Vec::new();
    };
    let inherit_row = inherit_default_row(agent);
    let query = filter.trim().to_ascii_lowercase();
    let choice_rows = match provider {
        Some(provider) => provider_model_rows(model_availability, provider),
        None => provider_rows(model_availability),
    };
    if query.is_empty() {
        let mut rows = vec![inherit_row];
        rows.extend(choice_rows);
        return rows;
    }

    let mut rows = vec![inherit_row];
    rows.extend(
        choice_rows
            .into_iter()
            .filter(|row| row.searchable.contains(&query)),
    );
    rows
}

pub(super) fn selected_model_choice(
    agent: Option<&AgentSummary>,
    model_availability: &[ResolvedModelAvailability],
    provider: Option<&str>,
    filter: &str,
    selected: usize,
) -> Option<ModelPickerChoice> {
    model_picker_rows(agent, model_availability, provider, filter)
        .into_iter()
        .nth(selected)
        .and_then(|row| row.available.then_some(row.choice))
}

pub(super) fn clamp_model_picker_selection(
    agent: Option<&AgentSummary>,
    model_availability: &[ResolvedModelAvailability],
    provider: Option<&str>,
    filter: &str,
    selected: usize,
) -> usize {
    let len = model_picker_rows(agent, model_availability, provider, filter).len();
    if len == 0 {
        0
    } else {
        selected.min(len - 1)
    }
}

fn inherit_default_row(agent: &AgentSummary) -> ModelPickerRow {
    let default_model = agent.model.runtime_default_model.as_string();
    let current = agent.model.override_model.is_none();
    let title = if current {
        format!("inherit runtime default: {default_model} (current)")
    } else {
        format!("inherit runtime default: {default_model}")
    };
    ModelPickerRow {
        choice: ModelPickerChoice::InheritDefault,
        detail: "clear this agent's model override".into(),
        searchable: format!("inherit default runtime {default_model}").to_ascii_lowercase(),
        title,
        available: true,
    }
}

fn provider_rows(model_availability: &[ResolvedModelAvailability]) -> Vec<ModelPickerRow> {
    let mut providers =
        std::collections::BTreeMap::<String, Vec<&ResolvedModelAvailability>>::new();
    for entry in model_availability {
        providers
            .entry(entry.provider.clone())
            .or_default()
            .push(entry);
    }

    providers
        .into_iter()
        .map(|(provider, models)| provider_row(provider, models))
        .collect()
}

fn provider_row(provider: String, models: Vec<&ResolvedModelAvailability>) -> ModelPickerRow {
    let available_count = models.iter().filter(|entry| entry.available).count();
    let discovered_count = models
        .iter()
        .filter(|entry| entry.metadata_source == "remote_discovered")
        .count();
    let first = models.first().copied();
    let transport = first
        .and_then(|entry| entry.transport.as_deref())
        .map(|transport| format!(" transport:{transport}"))
        .unwrap_or_default();
    let source = first
        .and_then(|entry| entry.provider_source.as_deref())
        .map(|source| format!(" provider:{source}"))
        .unwrap_or_default();
    let credential = first
        .and_then(|entry| entry.credential_source.as_deref())
        .map(|source| format!(" credential:{source}"))
        .unwrap_or_default();
    let status = if available_count > 0 {
        format!(
            "ready: {available_count}/{} models selectable",
            models.len()
        )
    } else {
        let reason = models
            .iter()
            .find_map(|entry| entry.unavailable_reason.as_deref())
            .unwrap_or("provider unavailable");
        format!("unavailable: {reason}")
    };
    let discovery = if discovered_count > 0 {
        format!(" remote-discovered:{discovered_count}")
    } else {
        String::new()
    };
    ModelPickerRow {
        choice: ModelPickerChoice::Provider {
            provider: provider.clone(),
        },
        title: provider.clone(),
        detail: format!("{status}{transport}{source}{credential}{discovery}"),
        searchable: format!("{provider} {status}{transport}{source}{credential}{discovery}")
            .to_ascii_lowercase(),
        available: true,
    }
}

fn provider_model_rows(
    model_availability: &[ResolvedModelAvailability],
    provider: &str,
) -> Vec<ModelPickerRow> {
    model_availability
        .iter()
        .filter(|entry| entry.provider == provider)
        .map(model_availability_row)
        .collect()
}

fn model_availability_row(entry: &ResolvedModelAvailability) -> ModelPickerRow {
    let status = if entry.available {
        "ready".to_string()
    } else {
        format!(
            "unavailable: {}",
            entry
                .unavailable_reason
                .as_deref()
                .unwrap_or("provider unavailable")
        )
    };
    let reasoning = if supports_reasoning_effort(entry) {
        " reasoning:configurable"
    } else {
        " reasoning:skipped"
    };
    let provider = entry
        .provider_source
        .as_deref()
        .map(|source| format!("{} provider:{source}", entry.provider))
        .unwrap_or_else(|| entry.provider.clone());
    let transport = entry
        .transport
        .as_deref()
        .map(|transport| format!(" transport:{transport}"))
        .unwrap_or_default();
    ModelPickerRow {
        choice: ModelPickerChoice::Model {
            model: entry.model.clone(),
            supports_reasoning_effort: supports_reasoning_effort(entry),
        },
        title: format!("{}  {}", entry.model, entry.display_name),
        detail: format!(
            "{status}  {provider}{transport}{reasoning}  source:{}",
            entry.metadata_source
        ),
        searchable: format!(
            "{} {} {} {} {} {}",
            entry.model,
            entry.display_name,
            entry.provider,
            entry.metadata_source,
            status,
            reasoning
        )
        .to_ascii_lowercase(),
        available: entry.available,
    }
}

fn supports_reasoning_effort(entry: &ResolvedModelAvailability) -> bool {
    entry.policy.capabilities.reasoning_summaries
}

#[cfg(test)]
mod tests {
    use super::{clamp_model_picker_selection, model_picker_rows, selected_model_choice};
    use crate::system::{ExecutionProfile, ExecutionSnapshot};
    use crate::{
        config::{ModelRef, ProviderId},
        model_catalog::{ModelCapabilityFlags, ModelMetadataSource, ResolvedRuntimeModelPolicy},
        types::{
            AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
            AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState, AgentSummary,
            AgentTokenUsageSummary, AgentVisibility, ChildAgentSummary, ClosureDecision,
            ClosureOutcome, LoadedAgentsMdView, ResolvedModelAvailability, RuntimePosture,
            SkillsRuntimeView, TokenUsage, WaitingIntentSummary,
        },
    };

    fn policy(model: &str, display_name: &str, reasoning: bool) -> ResolvedRuntimeModelPolicy {
        ResolvedRuntimeModelPolicy {
            model_ref: ModelRef::parse(model).unwrap(),
            display_name: display_name.into(),
            description: "test".into(),
            context_window_tokens: Some(200_000),
            effective_context_window_percent: 90,
            prompt_budget_estimated_tokens: 180_000,
            compaction_trigger_estimated_tokens: 180_000,
            compaction_keep_recent_estimated_tokens: 68_400,
            runtime_max_output_tokens: 32_000,
            tool_output_truncation_estimated_tokens: 2_500,
            max_output_tokens_upper_limit: Some(128_000),
            capabilities: ModelCapabilityFlags {
                reasoning_summaries: reasoning,
                ..ModelCapabilityFlags::default()
            },
            source: ModelMetadataSource::BuiltInCatalog,
        }
    }

    fn availability(
        model: &str,
        display_name: &str,
        available: bool,
        reasoning: bool,
    ) -> ResolvedModelAvailability {
        let model_ref = ModelRef::parse(model).unwrap();
        ResolvedModelAvailability {
            model: model.into(),
            provider: model_ref.provider.as_str().into(),
            display_name: display_name.into(),
            metadata_source: "built_in_catalog".into(),
            provider_configured: true,
            provider_source: Some("built_in".into()),
            transport: Some("chat_completions".into()),
            credential_source: Some("env".into()),
            credential_kind: Some("api_key".into()),
            credential_configured: available,
            available,
            unavailable_reason: (!available).then_some("credential_missing".into()),
            policy: policy(model, display_name, reasoning),
        }
    }

    fn model_availability() -> Vec<ResolvedModelAvailability> {
        vec![
            availability("openai/gpt-5.4", "GPT-5.4", true, true),
            availability(
                "anthropic/claude-sonnet-4-6",
                "Claude Sonnet 4.6",
                false,
                false,
            ),
            availability("openrouter/deepseek-v3", "DeepSeek V3", true, false),
        ]
    }

    fn summary() -> AgentSummary {
        AgentSummary {
            identity: AgentIdentityView {
                agent_id: "default".into(),
                kind: AgentKind::Default,
                visibility: AgentVisibility::Public,
                ownership: AgentOwnership::SelfOwned,
                profile_preset: AgentProfilePreset::PublicNamed,
                status: AgentRegistryStatus::Active,
                is_default_agent: true,
                parent_agent_id: None,
                lineage_parent_agent_id: None,
                delegated_from_task_id: None,
            },
            agent: AgentState::new("default"),
            active_task_count: 0,
            lifecycle: AgentLifecycleHint::default(),
            scheduling_posture: Default::default(),
            model: AgentModelState {
                source: AgentModelSource::RuntimeDefault,
                runtime_default_model: ModelRef::new(ProviderId::openai(), "gpt-5.4"),
                effective_model: ModelRef::new(ProviderId::openai(), "gpt-5.4"),
                requested_model: Some(ModelRef::new(ProviderId::openai(), "gpt-5.4")),
                active_model: Some(ModelRef::new(ProviderId::openai(), "gpt-5.4")),
                fallback_active: false,
                effective_fallback_models: Vec::new(),
                override_model: None,
                override_reasoning_effort: None,
                resolved_policy: policy("openai/gpt-5.4", "GPT-5.4", true),
            },
            token_usage: AgentTokenUsageSummary {
                total: TokenUsage::new(0, 0),
                total_model_rounds: 0,
                last_turn: None,
            },
            closure: ClosureDecision {
                outcome: ClosureOutcome::Completed,
                waiting_reason: None,
                work_signal: None,
                runtime_posture: RuntimePosture::Awake,
                evidence: Vec::new(),
            },
            execution: ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: Vec::new(),
                workspace_id: None,
                workspace_anchor: "/tmp".into(),
                execution_root: "/tmp".into(),
                cwd: "/tmp".into(),
                execution_root_id: None,
                projection_kind: None,
                access_mode: None,
                worktree_root: None,
            },
            active_workspace_occupancy: None,
            loaded_agents_md: LoadedAgentsMdView::default(),
            skills: SkillsRuntimeView::default(),
            active_children: Vec::<ChildAgentSummary>::new(),
            active_waiting_intents: Vec::<WaitingIntentSummary>::new(),
            active_wait_conditions: Vec::new(),
            active_external_triggers: Vec::new(),
            recent_operator_notifications: Vec::new(),
            recent_brief_count: 0,
            recent_event_count: 0,
        }
    }

    #[test]
    fn picker_rows_start_with_provider_choices() {
        let agent = summary();
        let availability = model_availability();
        let rows = model_picker_rows(Some(&agent), &availability, None, "");
        assert_eq!(rows.len(), 4);
        assert!(rows[0].title.contains("inherit runtime default"));
        assert!(rows.iter().any(|row| row.title == "openai"));
        assert!(rows.iter().any(|row| row.title == "openrouter"));
    }

    #[test]
    fn provider_model_page_shows_provider_specific_models() {
        let agent = summary();
        let availability = model_availability();
        let rows = model_picker_rows(Some(&agent), &availability, Some("openrouter"), "");
        assert_eq!(rows.len(), 2);
        assert!(rows[0].title.contains("inherit runtime default"));
        assert!(rows[1].title.contains("openrouter/deepseek-v3"));
        assert!(rows[1].available);
        assert!(rows[1].detail.contains("reasoning:skipped"));
    }

    #[test]
    fn provider_model_page_surfaces_unavailable_models() {
        let agent = summary();
        let availability = model_availability();
        let rows = model_picker_rows(Some(&agent), &availability, Some("anthropic"), "");
        assert!(rows
            .iter()
            .any(|row| row.title.contains("anthropic/claude-sonnet-4-6")));
        assert!(!rows[1].available);
        assert!(rows[1].detail.contains("credential_missing"));
    }

    #[test]
    fn picker_rows_filter_by_provider_or_model_context() {
        let agent = summary();
        let availability = model_availability();
        let rows = model_picker_rows(Some(&agent), &availability, None, "router");
        assert_eq!(rows.len(), 2);
        assert!(rows[0].title.contains("inherit runtime default"));

        let rows = model_picker_rows(Some(&agent), &availability, Some("anthropic"), "sonnet");
        assert_eq!(rows.len(), 2);
        assert!(rows[1].title.contains("claude-sonnet"));
    }

    #[test]
    fn picker_selection_clamps_to_filtered_rows() {
        let agent = summary();
        let availability = model_availability();
        assert_eq!(
            clamp_model_picker_selection(Some(&agent), &availability, Some("openai"), "gpt", 10),
            1
        );
        assert!(selected_model_choice(Some(&agent), &availability, None, "", 1).is_some());
        assert!(
            selected_model_choice(Some(&agent), &availability, Some("anthropic"), "", 1).is_none()
        );
    }
}
