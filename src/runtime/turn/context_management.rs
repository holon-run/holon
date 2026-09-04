//! Context management eligibility diagnostics for tool results.

use crate::config::ModelRouteRef;
use crate::projection_eval::{HistorySelector, ProjectionDiagnostics};
use crate::provider::{
    AgentProvider, ConversationMessage, ModelBlock, ProviderPromptCapability, ProviderTurnRequest,
    ToolResultBlock,
};
use serde_json::Value;

pub(super) fn configured_history_selector(
    agent_id: &str,
    model: &ModelRouteRef,
) -> HistorySelector {
    configured_history_selector_with_lookup(agent_id, model, |key| std::env::var(key).ok())
}

pub(super) fn projection_fallback_reason(
    provider: &dyn AgentProvider,
    selector: HistorySelector,
) -> Option<&'static str> {
    if selector == HistorySelector::RecentTurns {
        return None;
    }
    let capabilities = provider.prompt_capabilities();
    if capabilities.contains(&ProviderPromptCapability::FullRequestOnly)
        || capabilities.contains(&ProviderPromptCapability::PromptCacheKey)
        || capabilities.contains(&ProviderPromptCapability::PromptCacheBlocks)
    {
        None
    } else {
        Some("provider_projection_capability_unavailable")
    }
}

fn configured_history_selector_with_lookup(
    agent_id: &str,
    model: &ModelRouteRef,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> HistorySelector {
    let model_ref = format!(
        "{}@{}/{}",
        model.provider.as_str(),
        model.endpoint.as_str(),
        model.model
    );
    let keys = [
        format!(
            "HOLON_CONTEXT_HISTORY_SELECTOR_AGENT_{}",
            selector_env_component(agent_id)
        ),
        format!(
            "HOLON_CONTEXT_HISTORY_SELECTOR_MODEL_{}",
            selector_env_component(&model_ref)
        ),
        format!(
            "HOLON_CONTEXT_HISTORY_SELECTOR_PROVIDER_{}",
            selector_env_component(model.provider.as_str())
        ),
        "HOLON_CONTEXT_HISTORY_SELECTOR".to_string(),
    ];
    keys.iter()
        .find_map(|key| lookup(key).and_then(|value| parse_configured_history_selector(&value)))
        .unwrap_or(HistorySelector::RecentTurns)
}

fn selector_env_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn parse_configured_history_selector(value: &str) -> Option<HistorySelector> {
    match value.trim() {
        "work_item_scoped" => Some(HistorySelector::WorkItemScoped),
        "recent_turns" => Some(HistorySelector::RecentTurns),
        _ => None,
    }
}

pub(super) fn projection_diagnostic(
    prompt: &crate::prompt::EffectivePrompt,
    selector: HistorySelector,
    fallback_reason: Option<&str>,
) -> Value {
    let mut diagnostic = prompt
        .projection_manifest_for_selector(selector)
        .diagnostics
        .unwrap_or_else(|| {
            ProjectionDiagnostics::new(selector, "request_scoped_projection", None, 0, 0, 0)
        });
    if let Some(reason) = fallback_reason {
        diagnostic = diagnostic.with_fallback_reason(reason);
    }
    serde_json::to_value(diagnostic).unwrap_or_else(|_| {
        serde_json::json!({
            "history_selector": selector.as_str(),
            "fallback_reason": fallback_reason,
        })
    })
}

pub(super) fn context_management_diagnostic(
    provider: &dyn AgentProvider,
    request: &ProviderTurnRequest,
) -> Value {
    let Some(policy) = provider.context_management_policy() else {
        return serde_json::json!({
            "enabled": false,
            "disabled_reason": "provider_context_management_not_enabled",
        });
    };

    let stats = estimate_context_management_eligible_tool_results(
        &request.conversation,
        policy.keep_recent_tool_uses,
    );
    serde_json::json!({
        "enabled": true,
        "policy": {
            "provider": policy.provider,
            "strategy": policy.strategy,
            "trigger_input_tokens": policy.trigger_input_tokens,
            "keep_recent_tool_uses": policy.keep_recent_tool_uses,
            "clear_at_least_input_tokens": policy.clear_at_least_input_tokens,
            "clears_tool_results_only": true,
            "excludes_errors": true,
            "excluded_tool_names": ["ApplyPatch"],
        },
        "eligible_tool_result_count": stats.eligible_tool_result_count,
        "eligible_tool_result_bytes": stats.eligible_tool_result_bytes,
        "retained_recent_tool_result_count": stats.retained_recent_tool_result_count,
        "excluded_tool_result_count": stats.excluded_tool_result_count,
    })
}

#[derive(Default)]
pub(super) struct ContextManagementEligibilityStats {
    pub(super) eligible_tool_result_count: usize,
    pub(super) eligible_tool_result_bytes: usize,
    pub(super) retained_recent_tool_result_count: usize,
    pub(super) excluded_tool_result_count: usize,
}

pub(super) fn estimate_context_management_eligible_tool_results(
    conversation: &[ConversationMessage],
    keep_recent_tool_uses: usize,
) -> ContextManagementEligibilityStats {
    let mut tool_names_by_id = std::collections::HashMap::<&str, &str>::new();
    let mut tool_results = Vec::<(&ToolResultBlock, Option<&str>)>::new();
    for message in conversation {
        match message {
            ConversationMessage::AssistantBlocks(blocks) => {
                for block in blocks {
                    if let ModelBlock::ToolUse { id, name, .. } = block {
                        tool_names_by_id.insert(id.as_str(), name.as_str());
                    }
                }
            }
            ConversationMessage::UserToolResults(results) => {
                for result in results {
                    tool_results.push((
                        result,
                        tool_names_by_id.get(result.tool_use_id.as_str()).copied(),
                    ));
                }
            }
            ConversationMessage::UserText(_)
            | ConversationMessage::UserBlocks(_)
            | ConversationMessage::UserImage { .. } => {}
        }
    }

    let recent_start = tool_results.len().saturating_sub(keep_recent_tool_uses);
    let mut stats = ContextManagementEligibilityStats::default();
    for (index, (result, tool_name)) in tool_results.into_iter().enumerate() {
        if index >= recent_start {
            stats.retained_recent_tool_result_count += 1;
            continue;
        }
        if result.is_error || is_context_management_excluded_tool(tool_name) {
            stats.excluded_tool_result_count += 1;
            continue;
        }
        stats.eligible_tool_result_count += 1;
        stats.eligible_tool_result_bytes = stats
            .eligible_tool_result_bytes
            .saturating_add(result.content.len());
    }
    stats
}

pub(super) fn is_context_management_excluded_tool(tool_name: Option<&str>) -> bool {
    matches!(tool_name, Some(crate::tool::names::APPLY_PATCH))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        config::ModelRouteRef,
        provider::{
            AgentProvider, ProviderPromptCapability, ProviderTurnRequest, ProviderTurnResponse,
        },
    };
    use async_trait::async_trait;

    use super::{
        configured_history_selector_with_lookup, parse_configured_history_selector, HistorySelector,
    };

    #[test]
    fn history_selector_defaults_to_recent_turns() {
        assert_eq!(parse_configured_history_selector("unknown"), None);
        assert_eq!(
            parse_configured_history_selector(" work_item_scoped "),
            Some(HistorySelector::WorkItemScoped)
        );
    }

    #[test]
    fn history_selector_prefers_agent_then_model_then_provider_then_global() {
        let model = ModelRouteRef::parse("openai@default/gpt-5").unwrap();
        let mut values = BTreeMap::from([
            (
                "HOLON_CONTEXT_HISTORY_SELECTOR".to_string(),
                "work_item_scoped".to_string(),
            ),
            (
                "HOLON_CONTEXT_HISTORY_SELECTOR_PROVIDER_OPENAI".to_string(),
                "recent_turns".to_string(),
            ),
            (
                "HOLON_CONTEXT_HISTORY_SELECTOR_MODEL_OPENAI_DEFAULT_GPT_5".to_string(),
                "work_item_scoped".to_string(),
            ),
            (
                "HOLON_CONTEXT_HISTORY_SELECTOR_AGENT_AGENT_1".to_string(),
                "recent_turns".to_string(),
            ),
        ]);
        let lookup = |key: &str| values.remove(key);
        assert_eq!(
            configured_history_selector_with_lookup("agent-1", &model, lookup),
            HistorySelector::RecentTurns
        );
    }

    #[test]
    fn invalid_higher_priority_selector_falls_back_to_valid_lower_priority_value() {
        let model = ModelRouteRef::parse("openai@default/gpt-5").unwrap();
        let values = BTreeMap::from([
            (
                "HOLON_CONTEXT_HISTORY_SELECTOR_AGENT_AGENT_1".to_string(),
                "typo".to_string(),
            ),
            (
                "HOLON_CONTEXT_HISTORY_SELECTOR".to_string(),
                "work_item_scoped".to_string(),
            ),
        ]);
        let lookup = |key: &str| values.get(key).cloned();
        assert_eq!(
            configured_history_selector_with_lookup("agent-1", &model, lookup),
            HistorySelector::WorkItemScoped
        );
    }

    struct NoPromptProjectionProvider;

    #[async_trait]
    impl AgentProvider for NoPromptProjectionProvider {
        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> anyhow::Result<ProviderTurnResponse> {
            Err(anyhow::anyhow!("test provider must not be called"))
        }

        fn prompt_capabilities(&self) -> Vec<ProviderPromptCapability> {
            Vec::new()
        }
    }

    #[test]
    fn scoped_projection_requires_a_provider_request_capability() {
        let provider = NoPromptProjectionProvider;
        assert_eq!(
            super::projection_fallback_reason(&provider, HistorySelector::WorkItemScoped),
            Some("provider_projection_capability_unavailable")
        );
        assert_eq!(
            super::projection_fallback_reason(&provider, HistorySelector::RecentTurns),
            None
        );
    }
}
