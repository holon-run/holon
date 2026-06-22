use serde_json::Value;

use crate::provider::{
    AgentProvider, ConversationMessage, ModelBlock, ProviderTurnRequest, ToolResultBlock,
};

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
    matches!(tool_name, Some("ApplyPatch"))
}
