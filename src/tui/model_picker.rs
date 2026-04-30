use crate::types::{AgentSummary, ResolvedModelAvailability};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ModelPickerChoice {
    InheritDefault,
    Model { model: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelPickerRow {
    pub(super) choice: ModelPickerChoice,
    pub(super) title: String,
    pub(super) detail: String,
    pub(super) searchable: String,
    pub(super) available: bool,
}

pub(super) fn model_picker_rows(agent: Option<&AgentSummary>, filter: &str) -> Vec<ModelPickerRow> {
    let Some(agent) = agent else {
        return Vec::new();
    };
    let inherit_row = inherit_default_row(agent);
    let query = filter.trim().to_ascii_lowercase();
    let model_rows = agent
        .model
        .model_availability
        .iter()
        .map(model_availability_row);
    if query.is_empty() {
        let mut rows = vec![inherit_row];
        rows.extend(model_rows);
        return rows;
    }

    let mut rows = vec![inherit_row];
    rows.extend(model_rows.filter(|row| row.searchable.contains(&query)));
    rows
}

pub(super) fn selected_model_choice(
    agent: Option<&AgentSummary>,
    filter: &str,
    selected: usize,
) -> Option<ModelPickerChoice> {
    model_picker_rows(agent, filter)
        .into_iter()
        .nth(selected)
        .map(|row| row.choice)
}

pub(super) fn clamp_model_picker_selection(
    agent: Option<&AgentSummary>,
    filter: &str,
    selected: usize,
) -> usize {
    let len = model_picker_rows(agent, filter).len();
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
        },
        title: format!("{}  {}", entry.model, entry.display_name),
        detail: format!(
            "{status}  {provider}{transport}  source:{}",
            entry.metadata_source
        ),
        searchable: format!(
            "{} {} {} {} {}",
            entry.model, entry.display_name, entry.provider, entry.metadata_source, status
        )
        .to_ascii_lowercase(),
        available: entry.available,
    }
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

    fn policy(model: &str, display_name: &str) -> ResolvedRuntimeModelPolicy {
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
            capabilities: ModelCapabilityFlags::default(),
            source: ModelMetadataSource::BuiltInCatalog,
        }
    }

    fn availability(model: &str, display_name: &str, available: bool) -> ResolvedModelAvailability {
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
            policy: policy(model, display_name),
        }
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
            lifecycle: AgentLifecycleHint::default(),
            model: AgentModelState {
                source: AgentModelSource::RuntimeDefault,
                runtime_default_model: ModelRef::new(ProviderId::openai(), "gpt-5.4"),
                effective_model: ModelRef::new(ProviderId::openai(), "gpt-5.4"),
                requested_model: Some(ModelRef::new(ProviderId::openai(), "gpt-5.4")),
                active_model: Some(ModelRef::new(ProviderId::openai(), "gpt-5.4")),
                fallback_active: false,
                effective_fallback_models: Vec::new(),
                override_model: None,
                resolved_policy: policy("openai/gpt-5.4", "GPT-5.4"),
                available_models: Vec::new(),
                model_availability: vec![
                    availability("openai/gpt-5.4", "GPT-5.4", true),
                    availability("anthropic/claude-sonnet-4-6", "Claude Sonnet 4.6", false),
                ],
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
            active_external_triggers: Vec::new(),
            recent_operator_notifications: Vec::new(),
            recent_brief_count: 0,
            recent_event_count: 0,
        }
    }

    #[test]
    fn picker_rows_include_inherit_and_runtime_availability() {
        let agent = summary();
        let rows = model_picker_rows(Some(&agent), "");
        assert_eq!(rows.len(), 3);
        assert!(rows[0].title.contains("inherit runtime default"));
        assert!(rows[1].title.contains("openai/gpt-5.4"));
        assert!(!rows[2].available);
        assert!(rows[2].detail.contains("credential_missing"));
    }

    #[test]
    fn picker_rows_filter_by_model_and_label() {
        let agent = summary();
        let rows = model_picker_rows(Some(&agent), "sonnet");
        assert_eq!(rows.len(), 2);
        assert!(rows[0].title.contains("inherit runtime default"));
        assert!(rows[1].title.contains("claude-sonnet-4-6"));
    }

    #[test]
    fn picker_selection_clamps_to_filtered_rows() {
        let agent = summary();
        assert_eq!(clamp_model_picker_selection(Some(&agent), "gpt", 10), 1);
        assert!(selected_model_choice(Some(&agent), "", 1).is_some());
    }
}
