//! Provider turn contract: prompt/context assembly and provider interaction.
//!
//! # Purpose (SVS-304)
//!
//! This module separates the core provider interaction contract from session
//! orchestration concerns. This makes provider interaction easier to test
//! without booting the full agent loop.
//!
//! # Separation of Concerns
//!
//! ## Provider Turn Contract (this module)
//! - Converting effective prompts into provider turn requests
//! - Provider request building for initial and continuation turns
//!
//! ## Session Orchestration (runtime/turn.rs)
//! - Managing conversation state across multiple turns
//! - Tool execution and result handling
//! - Token usage tracking
//! - Session state updates
//! - Loop control (max rounds, stagnation detection, etc.)
//!
//! # API Overview
//!
//! - [`build_provider_turn_request`]: Creates the initial provider request
//! - [`build_continuation_request`]: Creates requests for subsequent turns

use crate::{
    prompt::{render_section, EffectivePrompt, PromptSection, PromptStability},
    provider::{
        ConversationMessage, PromptContentBlock, ProviderPromptCache, ProviderPromptFrame,
        ProviderTurnRequest,
    },
    tool::ToolSpec,
};

pub fn build_provider_prompt_frame(effective_prompt: &EffectivePrompt) -> ProviderPromptFrame {
    let (system_blocks, context_blocks) = build_prompt_content_blocks(
        &effective_prompt.system_sections,
        &effective_prompt.context_sections,
    );
    ProviderPromptFrame::structured(
        effective_prompt.rendered_system_prompt.clone(),
        system_blocks,
        context_blocks,
        Some(prompt_cache_from_effective_prompt(effective_prompt)),
    )
}

/// Builds a provider turn request from an effective prompt and available tools.
pub fn build_provider_turn_request(
    effective_prompt: &EffectivePrompt,
    available_tools: Vec<ToolSpec>,
) -> ProviderTurnRequest {
    let (system_blocks, context_blocks) = build_prompt_content_blocks(
        &effective_prompt.system_sections,
        &effective_prompt.context_sections,
    );
    ProviderTurnRequest {
        prompt_frame: ProviderPromptFrame::structured(
            effective_prompt.rendered_system_prompt.clone(),
            system_blocks,
            context_blocks.clone(),
            Some(prompt_cache_from_effective_prompt(effective_prompt)),
        ),
        conversation: vec![ConversationMessage::UserBlocks(context_blocks)],
        tools: available_tools,
    }
}

/// Builds a provider turn request from accumulated conversation state.
///
/// This is used for subsequent turns in a multi-turn conversation where
/// we need to pass the full conversation history to the provider.
pub fn build_continuation_request(
    prompt_frame: ProviderPromptFrame,
    conversation: Vec<ConversationMessage>,
    available_tools: Vec<ToolSpec>,
) -> ProviderTurnRequest {
    ProviderTurnRequest {
        prompt_frame,
        conversation,
        tools: available_tools,
    }
}

fn prompt_cache_from_effective_prompt(effective_prompt: &EffectivePrompt) -> ProviderPromptCache {
    ProviderPromptCache {
        agent_id: effective_prompt.cache_identity.agent_id.clone(),
        prompt_cache_key: effective_prompt.cache_identity.prompt_cache_key.clone(),
        working_memory_revision: effective_prompt.cache_identity.working_memory_revision,
        compression_epoch: effective_prompt.cache_identity.compression_epoch,
    }
}

fn build_prompt_content_blocks(
    system_sections: &[PromptSection],
    context_sections: &[PromptSection],
) -> (Vec<PromptContentBlock>, Vec<PromptContentBlock>) {
    let mut system_blocks = section_slice_to_prompt_blocks(system_sections);
    let mut context_blocks = section_slice_to_prompt_blocks(context_sections);

    mark_last_cache_breakpoint(
        &mut system_blocks,
        &mut context_blocks,
        PromptStability::Stable,
    );
    mark_last_cache_breakpoint(
        &mut system_blocks,
        &mut context_blocks,
        PromptStability::AgentScoped,
    );

    (system_blocks, context_blocks)
}

fn section_slice_to_prompt_blocks(sections: &[PromptSection]) -> Vec<PromptContentBlock> {
    sections
        .iter()
        .enumerate()
        .map(|(index, section)| section_to_prompt_block(section, index + 1 != sections.len()))
        .collect()
}

fn section_to_prompt_block(
    section: &PromptSection,
    preserve_section_delimiter: bool,
) -> PromptContentBlock {
    let mut text = render_section(section);
    if preserve_section_delimiter {
        text.push_str("\n\n");
    }

    PromptContentBlock {
        text,
        stability: section.stability,
        cache_breakpoint: false,
    }
}

fn mark_last_cache_breakpoint(
    system_blocks: &mut [PromptContentBlock],
    context_blocks: &mut [PromptContentBlock],
    stability: PromptStability,
) {
    if let Some(block) = context_blocks
        .iter_mut()
        .rev()
        .find(|block| block.stability == stability)
    {
        block.cache_breakpoint = true;
        return;
    }

    if let Some(block) = system_blocks
        .iter_mut()
        .rev()
        .find(|block| block.stability == stability)
    {
        block.cache_breakpoint = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        prompt::{PromptCacheIdentity, PromptSection, PromptStability},
        system::ExecutionProfile,
        system::ExecutionSnapshot,
        types::{
            AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
            AgentVisibility, LoadedAgentsMd,
        },
    };
    use std::path::PathBuf;

    #[test]
    fn build_provider_turn_request_preserves_supplied_tool_list() {
        let tools = vec![
            ToolSpec {
                name: "Enqueue".to_string(),
                description: "Enqueue a message".to_string(),
                input_schema: serde_json::json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "SpawnAgent".to_string(),
                description: "Spawn an agent".to_string(),
                input_schema: serde_json::json!({}),
                freeform_grammar: None,
            },
            ToolSpec {
                name: "Sleep".to_string(),
                description: "Sleep".to_string(),
                input_schema: serde_json::json!({}),
                freeform_grammar: None,
            },
        ];

        let request = build_provider_turn_request(&fixture_prompt(), tools.clone());
        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn build_provider_turn_request_constructs_valid_request() {
        let effective_prompt = fixture_prompt();
        let tools = vec![ToolSpec {
            name: "Sleep".to_string(),
            description: "Sleep".to_string(),
            input_schema: serde_json::json!({}),
            freeform_grammar: None,
        }];

        let request = build_provider_turn_request(&effective_prompt, tools);

        assert_eq!(request.prompt_frame.system_prompt, "system prompt");
        assert_eq!(request.prompt_frame.system_blocks.len(), 0);
        assert_eq!(request.conversation.len(), 1);
        assert_eq!(request.tools.len(), 1);
        assert_eq!(
            request
                .prompt_frame
                .cache
                .as_ref()
                .map(|cache| cache.prompt_cache_key.as_str()),
            Some("default")
        );
        assert_eq!(
            request
                .prompt_frame
                .cache
                .as_ref()
                .map(|cache| cache.working_memory_revision),
            Some(4)
        );
        assert_eq!(
            request
                .prompt_frame
                .cache
                .as_ref()
                .map(|cache| cache.compression_epoch),
            Some(2)
        );
    }

    fn fixture_prompt() -> EffectivePrompt {
        EffectivePrompt {
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
            agent_home: PathBuf::from("/tmp/agent-home"),
            execution: ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: vec![],
                workspace_id: None,
                workspace_anchor: PathBuf::from("/tmp/agent-home"),
                execution_root: PathBuf::from("/tmp/agent-home"),
                cwd: PathBuf::from("/tmp/agent-home"),
                execution_root_id: None,
                projection_kind: None,
                access_mode: None,
                worktree_root: None,
            },
            loaded_agents_md: LoadedAgentsMd::default(),
            cache_identity: PromptCacheIdentity {
                agent_id: "default".into(),
                prompt_cache_key: "default".into(),
                working_memory_revision: 4,
                compression_epoch: 2,
            },
            system_sections: vec![],
            context_sections: vec![PromptSection {
                name: "test".to_string(),
                id: "test".to_string(),
                content: "test content".to_string(),
                stability: PromptStability::AgentScoped,
            }],
            rendered_system_prompt: "system prompt".to_string(),
            rendered_context_attachment: "context attachment".to_string(),
        }
    }

    #[test]
    fn build_provider_turn_request_marks_last_stable_and_agent_scoped_breakpoints() {
        let effective_prompt = EffectivePrompt {
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
            agent_home: PathBuf::from("/tmp/agent-home"),
            execution: ExecutionSnapshot {
                profile: ExecutionProfile::default(),
                policy: ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: vec![],
                workspace_id: None,
                workspace_anchor: PathBuf::from("/tmp/agent-home"),
                execution_root: PathBuf::from("/tmp/agent-home"),
                cwd: PathBuf::from("/tmp/agent-home"),
                execution_root_id: None,
                projection_kind: None,
                access_mode: None,
                worktree_root: None,
            },
            loaded_agents_md: LoadedAgentsMd::default(),
            cache_identity: PromptCacheIdentity {
                agent_id: "default".into(),
                prompt_cache_key: "default".into(),
                working_memory_revision: 7,
                compression_epoch: 3,
            },
            system_sections: vec![
                PromptSection {
                    name: "identity".to_string(),
                    id: "identity".to_string(),
                    content: "stable system".to_string(),
                    stability: PromptStability::Stable,
                },
                PromptSection {
                    name: "policy".to_string(),
                    id: "policy".to_string(),
                    content: "agent system".to_string(),
                    stability: PromptStability::AgentScoped,
                },
            ],
            context_sections: vec![
                PromptSection {
                    name: "memory".to_string(),
                    id: "memory".to_string(),
                    content: "stable context".to_string(),
                    stability: PromptStability::Stable,
                },
                PromptSection {
                    name: "working_memory".to_string(),
                    id: "working_memory".to_string(),
                    content: "agent context".to_string(),
                    stability: PromptStability::AgentScoped,
                },
                PromptSection {
                    name: "current_input".to_string(),
                    id: "current_input".to_string(),
                    content: "turn input".to_string(),
                    stability: PromptStability::TurnScoped,
                },
            ],
            rendered_system_prompt: "system prompt".to_string(),
            rendered_context_attachment: "context attachment".to_string(),
        };

        let request = build_provider_turn_request(&effective_prompt, Vec::new());

        let system_blocks = request.prompt_frame.system_blocks;
        assert_eq!(system_blocks[0].text, "## identity\nstable system\n\n");
        assert_eq!(system_blocks[1].text, "## policy\nagent system");
        assert_eq!(
            system_blocks
                .iter()
                .filter(|block| block.cache_breakpoint)
                .count(),
            0
        );

        let ConversationMessage::UserBlocks(context_blocks) = &request.conversation[0] else {
            panic!("expected user blocks");
        };
        assert_eq!(context_blocks[0].text, "## memory\nstable context\n\n");
        assert_eq!(
            context_blocks[1].text,
            "## working_memory\nagent context\n\n"
        );
        assert_eq!(context_blocks[2].text, "## current_input\nturn input");

        let flagged = context_blocks
            .iter()
            .filter(|block| block.cache_breakpoint)
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(flagged.len(), 2);
        assert!(flagged.iter().any(|text| text.contains("stable context")));
        assert!(flagged.iter().any(|text| text.contains("agent context")));
    }

    #[test]
    fn build_continuation_request_preserves_conversation_history() {
        let conversation = vec![
            ConversationMessage::UserText("First message".to_string()),
            ConversationMessage::AssistantBlocks(vec![]),
        ];

        let tools = vec![ToolSpec {
            name: "Sleep".to_string(),
            description: "Sleep".to_string(),
            input_schema: serde_json::json!({}),
            freeform_grammar: None,
        }];

        let prompt_frame = ProviderPromptFrame::structured(
            "system",
            vec![PromptContentBlock {
                text: "structured system".to_string(),
                stability: PromptStability::Stable,
                cache_breakpoint: true,
            }],
            vec![PromptContentBlock {
                text: "structured context".to_string(),
                stability: PromptStability::AgentScoped,
                cache_breakpoint: true,
            }],
            Some(ProviderPromptCache {
                agent_id: "default".to_string(),
                prompt_cache_key: "default".to_string(),
                working_memory_revision: 4,
                compression_epoch: 2,
            }),
        );

        let request = build_continuation_request(prompt_frame, conversation, tools);

        assert_eq!(request.prompt_frame.system_prompt, "system");
        assert_eq!(request.prompt_frame.system_blocks.len(), 1);
        assert_eq!(request.prompt_frame.context_blocks.len(), 1);
        assert!(request.prompt_frame.system_blocks[0].cache_breakpoint);
        assert!(request.prompt_frame.context_blocks[0].cache_breakpoint);
        assert_eq!(
            request
                .prompt_frame
                .cache
                .as_ref()
                .map(|cache| cache.compression_epoch),
            Some(2)
        );
        assert_eq!(request.conversation.len(), 2);
        assert_eq!(request.tools.len(), 1);
    }
}
