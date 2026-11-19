use super::{clamp_model_picker_selection, model_picker_rows, selected_model_picker_row};
use crate::system::{ExecutionProfile, ExecutionSnapshot};
use crate::{
    config::{ModelRef, ModelRouteRef},
    types::{
        AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
        AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState, AgentSummary,
        AgentTokenUsageSummary, AgentVisibility, ChildAgentSummary, ClosureDecision,
        ClosureOutcome, LoadedAgentsMdView, ResolvedModelAvailability, RuntimePosture,
        SkillsRuntimeView, TokenUsage,
    },
};
use holon_domain::model_values::{
    ModelCapabilityFlags, ModelMetadataSource, ResolvedRuntimeModelPolicy,
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
        verbosity: None,
        tool_output_truncation_estimated_tokens: 2_500,
        max_output_tokens_upper_limit: Some(128_000),
        capabilities: ModelCapabilityFlags {
            supports_reasoning: reasoning,
            ..ModelCapabilityFlags::default()
        },
        agent_turn: true,
        decision_capable: false,
        decision_protocol: None,
        reasoning_effort_options: reasoning
            .then(|| vec!["low".into(), "medium".into(), "high".into()])
            .unwrap_or_default(),
        source: ModelMetadataSource::BuiltInCatalog,
        evidence: Default::default(),
    }
}

fn route_ref(model: &str) -> ModelRouteRef {
    ModelRouteRef::parse_compatible(model).unwrap()
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
        provider_family: model_ref.provider.as_str().into(),
        endpoint: "default".into(),
        route_provider: model_ref.provider.as_str().into(),
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
        failure_kind: None,
        failure_disposition: None,
        policy: policy(model, display_name, reasoning),
        resolved_capabilities: None,
        media: Default::default(),
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
            name: None,
            kind: AgentKind::Default,
            visibility: AgentVisibility::Public,
            ownership: AgentOwnership::SelfOwned,
            profile_preset: AgentProfilePreset::PublicNamed,
            can_rename: false,
            status: AgentRegistryStatus::Active,
            is_default_agent: true,
            incarnation: 1,
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
            runtime_default_model: route_ref("openai/gpt-5.4"),
            effective_model: route_ref("openai/gpt-5.4"),
            requested_model: Some(route_ref("openai/gpt-5.4")),
            active_model: Some(route_ref("openai/gpt-5.4")),
            fallback_active: false,
            effective_fallback_models: Vec::new(),
            override_model: None,
            override_reasoning_effort: None,
            override_service_tier: None,
            effective_service_tier: None,
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
            execution_roots: Vec::new(),
        },
        active_workspace_occupancy: None,
        loaded_agents_md: LoadedAgentsMdView::default(),
        skills: SkillsRuntimeView::default(),
        active_children: Vec::<ChildAgentSummary>::new(),
        active_wait_conditions: Vec::new(),
        active_external_triggers: Vec::new(),
        recent_operator_notifications: Vec::new(),
        recent_brief_count: 0,
        recent_event_count: 0,
        provider_cooldown_wait: None,
    }
}

#[test]
fn picker_rows_start_with_available_provider_choices() {
    let agent = summary();
    let availability = model_availability();
    let rows = model_picker_rows(Some(&agent), &availability, None, "");
    assert_eq!(rows.len(), 3);
    assert!(rows[0].title.contains("inherit runtime default"));
    assert!(rows.iter().any(|row| row.title == "openai"));
    assert!(rows.iter().any(|row| row.title == "openrouter"));
    assert!(!rows.iter().any(|row| row.title == "anthropic"));
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
fn provider_model_page_omits_unavailable_models() {
    let agent = summary();
    let availability = model_availability();
    let rows = model_picker_rows(Some(&agent), &availability, Some("anthropic"), "");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].title.contains("inherit runtime default"));
    assert!(!rows
        .iter()
        .any(|row| row.title.contains("anthropic/claude-sonnet-4-6")));
}

#[test]
fn picker_rows_filter_by_provider_or_model_context() {
    let agent = summary();
    let availability = model_availability();
    let rows = model_picker_rows(Some(&agent), &availability, None, "router");
    assert_eq!(rows.len(), 2);
    assert!(rows[0].title.contains("inherit runtime default"));

    let rows = model_picker_rows(Some(&agent), &availability, Some("anthropic"), "sonnet");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].title.contains("inherit runtime default"));
}

#[test]
fn picker_selection_clamps_to_filtered_rows() {
    let agent = summary();
    let availability = model_availability();
    assert_eq!(
        clamp_model_picker_selection(Some(&agent), &availability, Some("openai"), "gpt", 10),
        1
    );
    assert!(selected_model_picker_row(Some(&agent), &availability, None, "", 1).is_some());
    assert!(
        selected_model_picker_row(Some(&agent), &availability, Some("anthropic"), "", 1).is_none()
    );
}
