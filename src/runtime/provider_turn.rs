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
        provider_transport_error_with_code, AgentProvider, ContinuationScopeId,
        ConversationMessage, PromptContentBlock, ProviderFailureClassification,
        ProviderFailureKind, ProviderNativeWebSearchRequest, ProviderPromptCache,
        ProviderPromptFrame, ProviderTurnRequest, RetryDisposition,
    },
    system::ExecutionSnapshot,
    tool::ToolSpec,
};
use base64::Engine as _;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::debug;

// Runtime attachment limits, not vendor-specific limits. Base64 expansion of
// 20 MiB leaves room for prompt/tool JSON under common 32 MiB request limits.
const MAX_INLINE_IMAGES: usize = 8;
const MAX_INLINE_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
// Content deduplication must not permit unbounded reads of different files.
const MAX_MARKDOWN_IMAGE_READS: usize = 16;

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
    inline_markdown_images: bool,
) -> anyhow::Result<ProviderTurnRequest> {
    let (system_blocks, context_blocks) = build_prompt_content_blocks(
        &effective_prompt.system_sections,
        &effective_prompt.context_sections,
    );
    let mut conversation = vec![ConversationMessage::UserBlocks(context_blocks.clone())];
    if inline_markdown_images {
        conversation.extend(markdown_image_messages_from_sections(
            &effective_prompt.context_sections,
            &effective_prompt.execution,
            MAX_INLINE_IMAGES,
            MAX_INLINE_IMAGE_BYTES,
        )?);
    }

    Ok(ProviderTurnRequest {
        dispatch_guard: None,
        continuation_scope_id: ContinuationScopeId::new(
            effective_prompt.cache_identity.agent_id.clone(),
        ),
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
    })
}

pub fn build_initial_provider_turn_request(
    provider: &dyn AgentProvider,
    effective_prompt: &EffectivePrompt,
    available_tools: Vec<ToolSpec>,
    native_web_search: Option<ProviderNativeWebSearchRequest>,
) -> anyhow::Result<ProviderTurnRequest> {
    build_provider_turn_request(
        effective_prompt,
        available_tools,
        native_web_search,
        provider.resolved_image_input_support().unwrap_or(true),
    )
}

/// Runtime-owned dynamic time follows all materialized input, never the
/// stable prompt frame. Retrying this request reuses the same sampled text.
pub(crate) fn append_inference_time(conversation: &mut Vec<ConversationMessage>, text: &str) {
    conversation.push(ConversationMessage::UserBlocks(vec![PromptContentBlock {
        text: text.to_string(),
        stability: PromptStability::TurnScoped,
        cache_breakpoint: false,
    }]));
}

/// Builds a provider turn request from accumulated conversation state.
///
/// This is used for subsequent turns in a multi-turn conversation where
/// we need to pass the full conversation history to the provider.
pub fn build_continuation_request(
    continuation_scope_id: Option<ContinuationScopeId>,
    prompt_frame: ProviderPromptFrame,
    conversation: Vec<ConversationMessage>,
    available_tools: Vec<ToolSpec>,
    native_web_search: Option<ProviderNativeWebSearchRequest>,
) -> ProviderTurnRequest {
    ProviderTurnRequest {
        dispatch_guard: None,
        continuation_scope_id,
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
    max_images: usize,
    max_bytes: u64,
) -> anyhow::Result<Vec<ConversationMessage>> {
    let mut messages = Vec::new();
    let mut paths = HashSet::new();
    let mut hashes = HashSet::new();
    let mut inline_bytes = 0u64;
    let mut duplicates = 0usize;
    let mut omitted_history = 0usize;
    // Current input reserves capacity first. A history reference to the same
    // image must not make a current attachment optional.
    for optional_history in [false, true] {
        for section in sections.iter().filter(|section| {
            matches!(
                section.id.as_str(),
                "recent_turns" | "relevant_episode_memory" | "current_work_item_process_trace"
            ) == optional_history
        }) {
            for image in markdown_image_refs(&section.content) {
                let Some(path) = resolve_markdown_image_src(image.src, execution) else {
                    continue;
                };
                let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                if paths.contains(&canonical) {
                    duplicates += 1;
                    continue;
                }
                if paths.len() >= MAX_MARKDOWN_IMAGE_READS {
                    if optional_history {
                        omitted_history += 1;
                        continue;
                    }
                    return Err(image_input_error(
                        "image_scan_budget_exceeded",
                        format!(
                            "required markdown images exceed runtime read budget: \
                             max_paths={MAX_MARKDOWN_IMAGE_READS}; reduce current image references"
                        ),
                    ));
                }
                paths.insert(canonical);
                // Do not read further optional payloads after exhaustion.
                // read_visual_reference also bounds files changed after stat.
                if optional_history
                    && (hashes.len() >= max_images
                        || std::fs::metadata(&path)
                            .is_ok_and(|meta| meta.len() > max_bytes.saturating_sub(inline_bytes)))
                {
                    omitted_history += 1;
                    continue;
                }
                let read_image = match crate::tool::tools::view_image::read_visual_reference(&path)
                {
                    Ok(read_image) => read_image,
                    Err(error) if optional_history => {
                        debug!(path = %path.display(), %error, "omitting unavailable historical image");
                        omitted_history += 1;
                        continue;
                    }
                    Err(error) => {
                        return Err(image_input_error(
                            "image_input_invalid",
                            format!("required markdown image cannot be attached: {error}"),
                        ));
                    }
                };
                if hashes.contains(&read_image.visual_reference.sha256) {
                    duplicates += 1;
                    continue;
                }
                let next_bytes = inline_bytes.saturating_add(read_image.bytes.len() as u64);
                if hashes.len() >= max_images || next_bytes > max_bytes {
                    if optional_history {
                        omitted_history += 1;
                        continue;
                    }
                    return Err(image_input_error(
                        "image_budget_exceeded",
                        format!(
                            "required markdown images exceed runtime attachment budget: \
                             image_count={}, decoded_bytes={}, max_images={max_images}, \
                             max_decoded_bytes={max_bytes}; reduce current image attachments",
                            hashes.len() + 1,
                            next_bytes,
                        ),
                    ));
                }
                inline_bytes = next_bytes;
                hashes.insert(read_image.visual_reference.sha256);
                messages.push(ConversationMessage::UserImage {
                    prompt: if image.alt.trim().is_empty() {
                        format!("Image from {}", image.src)
                    } else {
                        image.alt.trim().to_string()
                    },
                    media_type: read_image.visual_reference.mime,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(read_image.bytes),
                    optional_history,
                });
            }
        }
    }
    debug!(
        image_count = hashes.len(),
        decoded_bytes = inline_bytes,
        duplicates,
        omitted_history,
        scanned_paths = paths.len(),
        max_scanned_paths = MAX_MARKDOWN_IMAGE_READS,
        max_images,
        max_bytes,
        "assembled bounded markdown image attachments"
    );
    if omitted_history > 0 {
        messages.push(ConversationMessage::UserText(format!(
            "[Holon image attachment budget: omitted {omitted_history} optional historical \
             image attachment(s) that exceeded the budget or could not be read. \
             Original references and any existing text/observations remain; omitted \
             pixels are not available in this request. Attached {} unique image(s), \
             {inline_bytes} decoded bytes; limits {max_images} images / {max_bytes} bytes.]",
            hashes.len(),
        )));
    }
    Ok(messages)
}

fn image_input_error(code: &str, message: String) -> anyhow::Error {
    provider_transport_error_with_code(
        ProviderFailureClassification {
            kind: ProviderFailureKind::ContractError,
            disposition: RetryDisposition::FailFast,
        },
        Some(code),
        None,
        None,
        message,
    )
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

fn resolve_markdown_image_src(src: &str, execution: &ExecutionSnapshot) -> Option<PathBuf> {
    if src.starts_with("workspace://") {
        let reference = crate::system::parse_workspace_uri(src).ok()?;
        let root = if let Some(root_id) = reference.execution_root_id.as_deref() {
            let root_ref = execution.execution_roots.iter().find(|root| {
                root.execution_root_id == root_id && root.workspace_id == reference.workspace_id
            })?;
            root_ref.filesystem_path.as_path()
        } else {
            workspace_root_for_id(&reference.workspace_id, execution)?
        };
        return crate::system::resolve_path_within_root(root, &reference.path).ok();
    }
    if src.contains("://") {
        return None;
    }
    let path = PathBuf::from(crate::system::decode_percent_encoded_path(src).ok()?);
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
    let decoded = crate::system::decode_percent_encoded_path(relative).ok()?;
    let relative = Path::new(&decoded);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    crate::system::resolve_path_within_root(root, &decoded).ok()
}

fn path_is_allowed(path: &Path, execution: &ExecutionSnapshot) -> bool {
    crate::system::path_is_within_root(path, &execution.workspace_anchor)
        || execution
            .attached_workspaces
            .iter()
            .any(|(_, root)| crate::system::path_is_within_root(path, root))
        || execution
            .execution_roots
            .iter()
            .any(|root| crate::system::path_is_within_root(path, &root.filesystem_path))
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
        config::{AppConfig, ModelRouteRef},
        prompt::{PromptCacheIdentity, PromptSection, PromptStability},
        provider::build_provider_from_model_chain,
        system::ExecutionProfile,
        system::ExecutionRootRef,
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
                name: "InvokeAgent".to_string(),
                description: "Invoke an agent".to_string(),
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

        let request = build_provider_turn_request(&fixture_prompt(), tools.clone(), None, true)
            .expect("build image-budgeted request");
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
    fn inference_time_does_not_mutate_stable_frame_or_input() {
        let prompt = fixture_prompt();
        let mut request = build_provider_turn_request(&prompt, vec![], None, false)
            .expect("build image-budgeted request");
        let frame = request.prompt_frame.clone();
        let ConversationMessage::UserBlocks(input) = &request.conversation[0] else {
            panic!("input must be materialized context");
        };
        let input = input.clone();
        append_inference_time(
            &mut request.conversation,
            "current_time: 2026-10-08T12:00:00+00:00\ntimezone: UTC",
        );
        assert_eq!(request.prompt_frame, frame);
        assert_eq!(request.conversation.len(), 2);
        let ConversationMessage::UserBlocks(preserved_input) = &request.conversation[0] else {
            panic!("input was replaced");
        };
        assert_eq!(*preserved_input, input);
        let ConversationMessage::UserBlocks(blocks) = request.conversation.last().unwrap() else {
            panic!("time must be a typed dynamic content block");
        };
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].stability, PromptStability::TurnScoped);
        assert!(!blocks[0].cache_breakpoint);
        let retry = request.clone();
        assert_eq!(retry.prompt_frame, request.prompt_frame);
        let ConversationMessage::UserBlocks(retry_time) = retry.conversation.last().unwrap() else {
            panic!("retry lost runtime time");
        };
        assert_eq!(retry_time, blocks);
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

        let request = build_provider_turn_request(&effective_prompt, tools, None, true)
            .expect("build image-budgeted request");

        assert_eq!(request.prompt_frame.system_prompt, "system prompt");
        assert_eq!(
            request
                .continuation_scope_id
                .as_ref()
                .map(ContinuationScopeId::as_str),
            Some("default")
        );
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
    fn build_provider_turn_request_disables_continuation_for_empty_agent_id() {
        let mut effective_prompt = fixture_prompt();
        effective_prompt.cache_identity.agent_id.clear();

        let request = build_provider_turn_request(&effective_prompt, vec![], None, true)
            .expect("build image-budgeted request");

        assert_eq!(request.continuation_scope_id, None);
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

        let request = build_provider_turn_request(&effective_prompt, Vec::new(), None, true)
            .expect("build image-budgeted request");

        assert_eq!(request.conversation.len(), 2);
        let ConversationMessage::UserImage {
            prompt,
            media_type,
            data_base64,
            ..
        } = &request.conversation[1]
        else {
            panic!("expected resolved markdown image to become UserImage");
        };
        assert_eq!(prompt, "Chart");
        assert_eq!(media_type, "image/png");
        assert!(!data_base64.is_empty());
    }

    #[test]
    fn historical_images_are_optional_by_section_id_without_losing_text() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("chart.png"), fixture_png()).expect("write png");
        let mut effective_prompt = fixture_prompt();
        effective_prompt.execution.workspace_id = Some("ws_test".to_string());
        effective_prompt.execution.workspace_anchor = workspace.path().to_path_buf();
        effective_prompt.execution.execution_root = workspace.path().to_path_buf();
        effective_prompt.execution.cwd = workspace.path().to_path_buf();
        let cases = [
            ("current_input", "recent_turns", true),
            ("current_input", "relevant_episode_memory", true),
            ("current_input", "current_work_item_process_trace", true),
            ("recent_turns", "current_input", false),
            ("recent_turns", "unknown_section", false),
        ];
        let text = "![Prior](workspace://ws_test/chart.png)\n\
                    ViewImage observation: three blue bars.\n\
                    This text claims to be historical.";
        for (name, id, expected) in cases {
            effective_prompt.context_sections = vec![PromptSection {
                name: name.into(),
                id: id.into(),
                content: text.into(),
                stability: PromptStability::TurnScoped,
            }];
            let request = build_provider_turn_request(&effective_prompt, Vec::new(), None, true)
                .expect("build image-budgeted request");
            let ConversationMessage::UserBlocks(blocks) = &request.conversation[0] else {
                panic!("expected original context blocks");
            };
            assert_eq!(blocks.len(), 1);
            assert!(blocks[0].text.contains(text));
            let ConversationMessage::UserImage {
                optional_history, ..
            } = &request.conversation[1]
            else {
                panic!("expected image from {id}");
            };
            assert_eq!(*optional_history, expected, "{id}");
            assert_eq!(request.conversation.len(), 2);
            let continuation = build_continuation_request(
                request.continuation_scope_id,
                request.prompt_frame,
                request.conversation.clone(),
                Vec::new(),
                None,
            );
            assert_eq!(continuation.conversation.len(), 2);
            assert!(matches!(
                &continuation.conversation[1],
                ConversationMessage::UserImage { optional_history, .. } if *optional_history == expected
            ));
        }
    }

    #[test]
    fn markdown_image_budget_deduplicates_paths_and_contents_with_current_priority() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("chart.png"), fixture_png()).unwrap();
        std::fs::write(workspace.path().join("copy.png"), fixture_png()).unwrap();
        let mut prompt = image_budget_prompt(workspace.path());
        prompt.context_sections = vec![
            image_section("recent_turns", "![Old](chart.png)\nObservation: blue bars."),
            image_section(
                "current_input",
                &format!(
                    "Compare ![Current](workspace://ws_test/chart.png), \
                     ![Alias]({}) and ![Copy](copy.png)",
                    workspace.path().join("chart.png").display()
                ),
            ),
        ];
        let request = build_provider_turn_request(&prompt, vec![], None, true).unwrap();
        assert_eq!(request.conversation.len(), 2);
        assert!(matches!(
            &request.conversation[1],
            ConversationMessage::UserImage { prompt, optional_history: false, .. } if prompt == "Current"
        ));
        let ConversationMessage::UserBlocks(blocks) = &request.conversation[0] else {
            panic!("original references must remain");
        };
        assert!(blocks[0].text.contains("Observation: blue bars."));
        assert!(blocks[1].text.contains("![Copy](copy.png)"));
    }

    #[test]
    fn markdown_image_budget_omits_only_history_and_preserves_observations() {
        let workspace = tempfile::tempdir().unwrap();
        let mut prompt = image_budget_prompt(workspace.path());
        let references = write_distinct_budget_images(workspace.path(), MAX_INLINE_IMAGES + 2);
        prompt.context_sections = vec![
            image_section(
                "recent_turns",
                &format!(
                    "{}\nViewImage observation: prior blue bars.",
                    references[1..].join("\n")
                ),
            ),
            image_section("current_input", &references[0]),
        ];
        let request = build_provider_turn_request(&prompt, vec![], None, true).unwrap();
        assert_eq!(
            request
                .conversation
                .iter()
                .filter(|m| matches!(m, ConversationMessage::UserImage { .. }))
                .count(),
            MAX_INLINE_IMAGES
        );
        assert!(matches!(
            request.conversation[1],
            ConversationMessage::UserImage {
                optional_history: false,
                ..
            }
        ));
        assert!(matches!(
            request.conversation.last().unwrap(),
            ConversationMessage::UserText(text) if text.contains("omitted 2 optional historical")
                && text.contains("pixels are not available")
        ));
        let ConversationMessage::UserBlocks(blocks) = &request.conversation[0] else {
            panic!("original context must remain");
        };
        assert!(blocks[0]
            .text
            .contains("ViewImage observation: prior blue bars."));
        assert!(blocks[1].text.contains(&references[0]));
    }

    #[test]
    fn markdown_image_budget_rejects_required_count_overflow_before_provider_dispatch() {
        let workspace = tempfile::tempdir().unwrap();
        let mut prompt = image_budget_prompt(workspace.path());
        let references = write_distinct_budget_images(workspace.path(), MAX_INLINE_IMAGES + 1);
        prompt.context_sections = vec![image_section("current_input", &references.join("\n"))];
        let error = build_provider_turn_request(&prompt, vec![], None, true).unwrap_err();
        let typed = error
            .downcast_ref::<crate::provider::ProviderTransportError>()
            .unwrap();
        assert_eq!(
            typed.classification.kind,
            ProviderFailureKind::ContractError
        );
        assert_eq!(typed.classification.disposition, RetryDisposition::FailFast);
        assert_eq!(typed.code.as_deref(), Some("image_budget_exceeded"));
        assert!(error.to_string().contains("image_count=9"));
        // A fallback to a text-only model must not attempt local inlining.
        let text_only = build_provider_turn_request(&prompt, vec![], None, false).unwrap();
        assert_eq!(text_only.conversation.len(), 1);
    }

    #[test]
    fn markdown_image_budget_bounds_duplicate_content_reads() {
        let workspace = tempfile::tempdir().unwrap();
        let mut prompt = image_budget_prompt(workspace.path());
        write_distinct_budget_images(workspace.path(), 1);
        let mut references = vec!["![image](budget-0.png)".to_string()];
        for index in 1..=MAX_MARKDOWN_IMAGE_READS {
            let name = format!("copy-{index}.png");
            std::fs::copy(
                workspace.path().join("budget-0.png"),
                workspace.path().join(&name),
            )
            .unwrap();
            references.push(format!("![image]({name})"));
        }
        prompt.context_sections = vec![image_section("current_input", &references.join("\n"))];
        let error = build_provider_turn_request(&prompt, vec![], None, true).unwrap_err();
        let typed = error
            .downcast_ref::<crate::provider::ProviderTransportError>()
            .unwrap();
        assert_eq!(typed.code.as_deref(), Some("image_scan_budget_exceeded"));
        assert_eq!(typed.classification.disposition, RetryDisposition::FailFast);

        prompt.context_sections[0].id = "recent_turns".into();
        let request = build_provider_turn_request(&prompt, vec![], None, true).unwrap();
        assert_eq!(
            request
                .conversation
                .iter()
                .filter(|message| matches!(message, ConversationMessage::UserImage { .. }))
                .count(),
            1
        );
        assert!(request.conversation.iter().any(|message| matches!(
            message, ConversationMessage::UserText(note)
                if note.contains("omitted 1 optional historical")
        )));
    }

    #[test]
    fn markdown_image_budget_decoded_byte_boundary_is_exact() {
        let workspace = tempfile::tempdir().unwrap();
        let mut prompt = image_budget_prompt(workspace.path());
        let references = write_distinct_budget_images(workspace.path(), 2);
        prompt.context_sections = vec![image_section("current_input", &references.join("\n"))];
        let bytes: u64 = (0..2)
            .map(|index| {
                std::fs::metadata(workspace.path().join(format!("budget-{index}.png")))
                    .unwrap()
                    .len()
            })
            .sum();
        let messages = markdown_image_messages_from_sections(
            &prompt.context_sections,
            &prompt.execution,
            2,
            bytes,
        )
        .unwrap();
        assert_eq!(messages.len(), 2);
        let error = markdown_image_messages_from_sections(
            &prompt.context_sections,
            &prompt.execution,
            2,
            bytes - 1,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains(&format!("decoded_bytes={bytes}")));
        assert!(error
            .to_string()
            .contains(&format!("max_decoded_bytes={}", bytes - 1)));
        prompt.context_sections[0].id = "recent_turns".into();
        let messages = markdown_image_messages_from_sections(
            &prompt.context_sections,
            &prompt.execution,
            2,
            bytes - 1,
        )
        .unwrap();
        assert!(matches!(messages.as_slice(), [
            ConversationMessage::UserImage { optional_history: true, .. },
            ConversationMessage::UserText(note),
        ] if note.contains("omitted 1 optional historical")));
    }

    #[test]
    fn markdown_image_budget_rejects_unreadable_current_image_not_silent_omission() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("broken.png"), b"not an image").unwrap();
        let mut prompt = image_budget_prompt(workspace.path());
        prompt.context_sections = vec![image_section("current_input", "![Required](broken.png)")];
        let error = build_provider_turn_request(&prompt, vec![], None, true).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::provider::ProviderTransportError>()
                .unwrap()
                .code
                .as_deref(),
            Some("image_input_invalid")
        );
        prompt.context_sections[0].id = "recent_turns".into();
        let request = build_provider_turn_request(&prompt, vec![], None, true).unwrap();
        assert!(
            matches!(request.conversation.last(), Some(ConversationMessage::UserText(note))
            if note.contains("omitted 1 optional historical"))
        );
    }

    fn image_budget_prompt(workspace: &Path) -> EffectivePrompt {
        let mut prompt = fixture_prompt();
        prompt.execution.workspace_id = Some("ws_test".into());
        prompt.execution.workspace_anchor = workspace.to_path_buf();
        prompt.execution.execution_root = workspace.to_path_buf();
        prompt.execution.cwd = workspace.to_path_buf();
        prompt
    }

    fn image_section(id: &str, text: &str) -> PromptSection {
        PromptSection {
            name: id.into(),
            id: id.into(),
            content: text.into(),
            stability: PromptStability::TurnScoped,
        }
    }

    fn write_distinct_budget_images(workspace: &Path, count: usize) -> Vec<String> {
        (0..count)
            .map(|index| {
                let name = format!("budget-{index}.png");
                image::RgbImage::from_pixel(2, 2, image::Rgb([index as u8, 64, 192]))
                    .save(workspace.join(&name))
                    .unwrap();
                format!("![Image {index}]({name})")
            })
            .collect()
    }

    #[test]
    fn build_provider_turn_request_skips_markdown_images_when_disabled() {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut effective_prompt = fixture_prompt();
        effective_prompt.execution.workspace_id = Some("ws_test".to_string());
        effective_prompt.execution.workspace_anchor = workspace.path().to_path_buf();
        effective_prompt.execution.execution_root = workspace.path().to_path_buf();
        effective_prompt.execution.cwd = workspace.path().to_path_buf();
        effective_prompt.context_sections = vec![PromptSection {
            name: "current_input".to_string(),
            id: "current_input".to_string(),
            content: "Please inspect ![Chart](workspace://ws_test/outputs/missing.png)."
                .to_string(),
            stability: PromptStability::TurnScoped,
        }];

        let request = build_provider_turn_request(&effective_prompt, Vec::new(), None, false)
            .expect("build image-budgeted request");

        assert_eq!(request.conversation.len(), 1);
        let ConversationMessage::UserBlocks(blocks) = &request.conversation[0] else {
            panic!("expected original user blocks");
        };
        assert!(blocks[0]
            .text
            .contains("![Chart](workspace://ws_test/outputs/missing.png)"));
    }

    #[test]
    fn initial_request_recomputes_markdown_image_gate_after_fallback_promotion() {
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

        let config = codex_image_fallback_config();
        let chain = config.provider_chain();
        let text_only_provider =
            build_provider_from_model_chain(&config, &chain).expect("text-only provider chain");
        let text_only_request = build_initial_provider_turn_request(
            text_only_provider.as_ref(),
            &effective_prompt,
            Vec::new(),
            None,
        )
        .expect("build image-budgeted request");
        assert_eq!(text_only_request.conversation.len(), 1);
        let ConversationMessage::UserBlocks(blocks) = &text_only_request.conversation[0] else {
            panic!("expected original user blocks");
        };
        assert!(blocks[0]
            .text
            .contains("![Chart](workspace://ws_test/outputs/chart.png)"));

        let image_provider = build_provider_from_model_chain(&config, &chain[1..])
            .expect("promoted image-capable provider chain");
        let image_request = build_initial_provider_turn_request(
            image_provider.as_ref(),
            &effective_prompt,
            Vec::new(),
            None,
        )
        .expect("build image-budgeted request");
        assert!(matches!(
            image_request.conversation.as_slice(),
            [
                ConversationMessage::UserBlocks(_),
                ConversationMessage::UserImage { .. }
            ]
        ));
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

        let request = build_provider_turn_request(&effective_prompt, Vec::new(), None, true)
            .expect("build image-budgeted request");

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

    fn codex_image_fallback_config() -> AppConfig {
        let home = tempfile::tempdir().expect("home").keep();
        std::fs::write(
            home.join("config.json"),
            r#"{
                "model":{"default":"openai-codex/test-text-only","fallbacks":["openai-codex/gpt-6-sol"]},
                "models":{"catalog":{"openai-codex/test-text-only":{"capabilities":{"image_input":false}}}}
            }"#,
        )
        .expect("write config");
        let mut config =
            AppConfig::load_with_home(Some(home)).expect("load fallback provider config");
        let provider = config
            .providers
            .get_mut(&crate::config::ProviderId::openai_codex())
            .expect("openai-codex provider");
        provider.credential = Some(
            r#"{"tokens":{"access_token":"test-token","refresh_token":"test-refresh","account_id":"test-account"}}"#
                .to_string(),
        );
        assert_eq!(
            config.provider_chain(),
            vec![
                ModelRouteRef::parse_compatible("openai-codex/test-text-only")
                    .expect("text-only route"),
                ModelRouteRef::parse_compatible("openai-codex/gpt-6-sol").expect("image route"),
            ]
        );
        config
    }

    fn fixture_prompt() -> EffectivePrompt {
        EffectivePrompt {
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
                execution_roots: Vec::new(),
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
            projection_owner: crate::projection_eval::ProjectionOwner::AgentLifecycle {
                agent_id: "default".into(),
            },
            projection_binding: None,
            projection_turn_id: None,
            projection_evidence: Default::default(),
            context_plan_evidence: Default::default(),
            recent_turns_reprojection: None,
        }
    }

    #[test]
    fn build_provider_turn_request_marks_last_stable_and_agent_scoped_breakpoints() {
        let effective_prompt = EffectivePrompt {
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
                execution_roots: Vec::new(),
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
            projection_owner: crate::projection_eval::ProjectionOwner::AgentLifecycle {
                agent_id: "default".into(),
            },
            projection_binding: None,
            projection_turn_id: None,
            projection_evidence: Default::default(),
            context_plan_evidence: Default::default(),
            recent_turns_reprojection: None,
        };

        let request = build_provider_turn_request(&effective_prompt, Vec::new(), None, true)
            .expect("build image-budgeted request");

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

        let request = build_continuation_request(
            ContinuationScopeId::new("default"),
            prompt_frame,
            conversation,
            tools,
            None,
        );

        assert_eq!(request.prompt_frame.system_prompt, "system");
        assert_eq!(
            request
                .continuation_scope_id
                .as_ref()
                .map(ContinuationScopeId::as_str),
            Some("default")
        );
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

    #[test]
    fn build_continuation_request_accepts_missing_scope() {
        let request = build_continuation_request(
            None,
            ProviderPromptFrame::plain("system"),
            vec![],
            vec![],
            None,
        );

        assert_eq!(request.continuation_scope_id, None);
    }

    #[test]
    fn resolve_markdown_image_src_with_root_param() {
        let mut execution = ExecutionSnapshot {
            profile: ExecutionProfile::default(),
            policy: ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: vec![],
            workspace_id: Some("ws_abc".into()),
            workspace_anchor: PathBuf::from("/tmp/canonical"),
            execution_root: PathBuf::from("/tmp/canonical"),
            cwd: PathBuf::from("/tmp/canonical"),
            execution_root_id: Some("canonical_root:ws_abc".into()),
            projection_kind: Some(crate::system::WorkspaceProjectionKind::CanonicalRoot),
            access_mode: None,
            worktree_root: None,
            execution_roots: vec![ExecutionRootRef {
                execution_root_id: "git_worktree_root:ws_abc:/tmp/wt".into(),
                workspace_id: "ws_abc".into(),
                filesystem_path: PathBuf::from("/tmp/wt"),
            }],
        };

        // With ?root=, resolves to the worktree path.
        let path = resolve_markdown_image_src(
            "workspace://ws_abc/media/img.png?root=git_worktree_root:ws_abc:/tmp/wt",
            &execution,
        );
        assert_eq!(path, Some(PathBuf::from("/tmp/wt/media/img.png")));

        // Without ?root=, resolves to canonical anchor (backward compatible).
        let path = resolve_markdown_image_src("workspace://ws_abc/media/img.png", &execution);
        assert_eq!(path, Some(PathBuf::from("/tmp/canonical/media/img.png")));

        // A root token from another workspace must not be accepted.
        execution.execution_roots[0].workspace_id = "ws_other".into();
        let path = resolve_markdown_image_src(
            "workspace://ws_abc/media/img.png?root=git_worktree_root:ws_abc:/tmp/wt",
            &execution,
        );
        assert_eq!(path, None);

        // Unknown root_id -> None.
        execution.execution_roots.clear();
        let path =
            resolve_markdown_image_src("workspace://ws_abc/media/img.png?root=unknown", &execution);
        assert_eq!(path, None);
    }

    #[test]
    fn resolve_markdown_image_src_allows_only_execution_visible_absolute_paths() {
        let canonical = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let worktree_image = worktree.path().join("media/image.png");
        std::fs::create_dir_all(worktree_image.parent().unwrap()).unwrap();
        std::fs::write(&worktree_image, "image").unwrap();
        let outside_image = outside.path().join("image.png");
        std::fs::write(&outside_image, "outside").unwrap();
        let execution = ExecutionSnapshot {
            profile: ExecutionProfile::default(),
            policy: ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: vec![],
            workspace_id: Some("ws_abc".into()),
            workspace_anchor: canonical.path().to_path_buf(),
            execution_root: worktree.path().to_path_buf(),
            cwd: worktree.path().to_path_buf(),
            execution_root_id: Some("worktree".into()),
            projection_kind: Some(crate::system::WorkspaceProjectionKind::GitWorktreeRoot),
            access_mode: None,
            worktree_root: Some(worktree.path().to_path_buf()),
            execution_roots: vec![ExecutionRootRef {
                execution_root_id: "worktree".into(),
                workspace_id: "ws_abc".into(),
                filesystem_path: worktree.path().to_path_buf(),
            }],
        };

        assert_eq!(
            resolve_markdown_image_src(&worktree_image.to_string_lossy(), &execution),
            Some(worktree_image)
        );
        assert_eq!(
            resolve_markdown_image_src(&outside_image.to_string_lossy(), &execution),
            None
        );
    }
}
