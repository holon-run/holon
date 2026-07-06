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
        ConversationMessage, PromptContentBlock, ProviderNativeWebSearchRequest,
        ProviderPromptCache, ProviderPromptFrame, ProviderTurnRequest,
    },
    system::ExecutionSnapshot,
    tool::ToolSpec,
};
use base64::Engine as _;
use std::path::{Path, PathBuf};
use tracing::debug;

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
    native_web_search: Option<ProviderNativeWebSearchRequest>,
) -> ProviderTurnRequest {
    let (system_blocks, context_blocks) = build_prompt_content_blocks(
        &effective_prompt.system_sections,
        &effective_prompt.context_sections,
    );
    let mut conversation = vec![ConversationMessage::UserBlocks(context_blocks.clone())];
    conversation.extend(markdown_image_messages_from_sections(
        &effective_prompt.context_sections,
        &effective_prompt.execution,
    ));

    ProviderTurnRequest {
        prompt_frame: ProviderPromptFrame::structured(
            effective_prompt.rendered_system_prompt.clone(),
            system_blocks,
            context_blocks.clone(),
            Some(prompt_cache_from_effective_prompt(effective_prompt)),
        ),
        conversation,
        tools: available_tools,
        native_web_search,
        response_format: None,
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
    native_web_search: Option<ProviderNativeWebSearchRequest>,
) -> ProviderTurnRequest {
    ProviderTurnRequest {
        prompt_frame,
        conversation,
        tools: available_tools,
        native_web_search,
        response_format: None,
    }
}

fn prompt_cache_from_effective_prompt(effective_prompt: &EffectivePrompt) -> ProviderPromptCache {
    ProviderPromptCache {
        agent_id: effective_prompt.cache_identity.agent_id.clone(),
        prompt_cache_key: effective_prompt.cache_identity.prompt_cache_key.clone(),
        context_fingerprint: effective_prompt.cache_identity.context_fingerprint.clone(),
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

fn markdown_image_messages_from_sections(
    sections: &[PromptSection],
    execution: &ExecutionSnapshot,
) -> Vec<ConversationMessage> {
    sections
        .iter()
        .flat_map(|section| markdown_image_messages_from_text(&section.content, execution))
        .collect()
}

fn markdown_image_messages_from_text(
    text: &str,
    execution: &ExecutionSnapshot,
) -> Vec<ConversationMessage> {
    markdown_image_refs(text)
        .into_iter()
        .filter_map(|image| markdown_image_message(image, execution))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkdownImageRef<'a> {
    alt: &'a str,
    src: &'a str,
}

fn markdown_image_refs(text: &str) -> Vec<MarkdownImageRef<'_>> {
    let mut refs = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= text.len() {
        let Some(start) = text[offset..].find("![") else {
            break;
        };
        let alt_start = offset + start + 2;
        let Some(alt_end_rel) = text[alt_start..].find(']') else {
            break;
        };
        let alt_end = alt_start + alt_end_rel;
        let src_open = alt_end + 1;
        if text[src_open..].starts_with('(') {
            let src_start = src_open + 1;
            if let Some(src_end_rel) = text[src_start..].find(')') {
                let src_end = src_start + src_end_rel;
                let src = text[src_start..src_end]
                    .split_whitespace()
                    .next()
                    .unwrap_or_default();
                if !src.is_empty() {
                    refs.push(MarkdownImageRef {
                        alt: &text[alt_start..alt_end],
                        src,
                    });
                }
                offset = src_end + 1;
                continue;
            }
        }
        offset = alt_end + 1;
    }
    refs
}

fn markdown_image_message(
    image: MarkdownImageRef<'_>,
    execution: &ExecutionSnapshot,
) -> Option<ConversationMessage> {
    let path = resolve_markdown_image_src(image.src, execution)?;
    match crate::tool::tools::view_image::read_visual_reference(&path) {
        Ok(read_image) => Some(ConversationMessage::UserImage {
            prompt: if image.alt.trim().is_empty() {
                format!("Image from {}", image.src)
            } else {
                image.alt.trim().to_string()
            },
            media_type: read_image.visual_reference.mime,
            data_base64: base64::engine::general_purpose::STANDARD.encode(read_image.bytes),
        }),
        Err(error) => {
            debug!(
                image_src = image.src,
                path = %path.display(),
                error = %error,
                "skipping unresolved markdown image provider input"
            );
            None
        }
    }
}

fn resolve_markdown_image_src(src: &str, execution: &ExecutionSnapshot) -> Option<PathBuf> {
    if let Some(rest) = src.strip_prefix("workspace://") {
        let (workspace_id, relative) = rest.split_once('/')?;
        let root = workspace_root_for_id(workspace_id, execution)?;
        return resolve_relative_path(root, relative);
    }
    if src.contains("://") {
        return None;
    }
    let path = PathBuf::from(percent_decode_path(src));
    if path.is_absolute() {
        path_is_allowed(&path, execution).then_some(path)
    } else {
        resolve_relative_path(&execution.cwd, src)
    }
}

fn workspace_root_for_id<'a>(
    workspace_id: &str,
    execution: &'a ExecutionSnapshot,
) -> Option<&'a Path> {
    if execution.workspace_id.as_deref() == Some(workspace_id) {
        return Some(&execution.workspace_anchor);
    }
    execution
        .attached_workspaces
        .iter()
        .find(|(id, _)| id == workspace_id)
        .map(|(_, path)| path.as_path())
}

fn resolve_relative_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let decoded = percent_decode_path(relative);
    let relative_path = Path::new(decoded.trim_start_matches('/'));
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    let path = root.join(relative_path);
    path_is_within(&path, root).then_some(path)
}

fn path_is_allowed(path: &Path, execution: &ExecutionSnapshot) -> bool {
    path_is_within(path, &execution.workspace_anchor)
        || execution
            .attached_workspaces
            .iter()
            .any(|(_, root)| path_is_within(path, root))
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    let Ok(normalized) = crate::system::workspace::normalize_path(path) else {
        return false;
    };
    let Ok(normalized_root) = crate::system::workspace::normalize_path(root) else {
        return false;
    };
    if !normalized.starts_with(&normalized_root) {
        return false;
    }
    if let (Ok(canonical), Ok(canonical_root)) = (
        std::fs::canonicalize(&normalized),
        std::fs::canonicalize(&normalized_root),
    ) {
        return canonical.starts_with(canonical_root);
    }
    true
}

fn percent_decode_path(path: &str) -> String {
    let mut decoded = String::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &path[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                decoded.push(value as char);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index] as char);
        index += 1;
    }
    decoded
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
            AgentVisibility, LoadedAgentMemory, LoadedAgentsMd,
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

        let request = build_provider_turn_request(&fixture_prompt(), tools.clone(), None);
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

        let request = build_provider_turn_request(&effective_prompt, tools, None);

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
                .map(|cache| cache.context_fingerprint.as_str()),
            Some("fingerprint-default")
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

    #[test]
    fn build_provider_turn_request_adds_workspace_markdown_image_input() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::create_dir(workspace.path().join("outputs")).expect("outputs dir");
        std::fs::write(workspace.path().join("outputs/chart.png"), fixture_png())
            .expect("write png");

        let mut effective_prompt = fixture_prompt();
        effective_prompt.execution.workspace_id = Some("ws_test".to_string());
        effective_prompt.execution.workspace_anchor = workspace.path().to_path_buf();
        effective_prompt.execution.execution_root = workspace.path().to_path_buf();
        effective_prompt.execution.cwd = workspace.path().to_path_buf();
        effective_prompt.context_sections = vec![PromptSection {
            name: "current_input".to_string(),
            id: "current_input".to_string(),
            content: "Please inspect ![Chart](workspace://ws_test/outputs/chart.png).".to_string(),
            stability: PromptStability::TurnScoped,
        }];

        let request = build_provider_turn_request(&effective_prompt, Vec::new(), None);

        assert_eq!(request.conversation.len(), 2);
        let ConversationMessage::UserImage {
            prompt,
            media_type,
            data_base64,
        } = &request.conversation[1]
        else {
            panic!("expected resolved markdown image to become UserImage");
        };
        assert_eq!(prompt, "Chart");
        assert_eq!(media_type, "image/png");
        assert!(!data_base64.is_empty());
    }

    #[test]
    fn markdown_image_resolver_ignores_remote_urls_and_workspace_escapes() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut effective_prompt = fixture_prompt();
        effective_prompt.execution.workspace_id = Some("ws_test".to_string());
        effective_prompt.execution.workspace_anchor = workspace.path().to_path_buf();
        effective_prompt.execution.execution_root = workspace.path().to_path_buf();
        effective_prompt.execution.cwd = workspace.path().to_path_buf();
        effective_prompt.context_sections = vec![PromptSection {
            name: "current_input".to_string(),
            id: "current_input".to_string(),
            content: concat!(
                "Ignore ![remote](https://example.com/chart.png) ",
                "and ![escape](workspace://ws_test/../chart.png)."
            )
            .to_string(),
            stability: PromptStability::TurnScoped,
        }];

        let request = build_provider_turn_request(&effective_prompt, Vec::new(), None);

        assert_eq!(request.conversation.len(), 1);
    }

    fn fixture_png() -> &'static [u8] {
        &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ]
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
            loaded_agent_memory: LoadedAgentMemory::default(),
            cache_identity: PromptCacheIdentity {
                agent_id: "default".into(),
                prompt_cache_key: "default".into(),
                context_fingerprint: "fingerprint-default".into(),
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
            loaded_agent_memory: LoadedAgentMemory::default(),
            cache_identity: PromptCacheIdentity {
                agent_id: "default".into(),
                prompt_cache_key: "default".into(),
                context_fingerprint: "fingerprint-default".into(),
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

        let request = build_provider_turn_request(&effective_prompt, Vec::new(), None);

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
                context_fingerprint: "fingerprint-default".to_string(),
                compression_epoch: 2,
            }),
        );

        let request = build_continuation_request(prompt_frame, conversation, tools, None);

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
