//! Deterministic multi-profile prompt and tool-description topology inventory.

use std::path::{Path, PathBuf};

use anyhow::Result;
use holon_context::{
    budget::estimate_text_tokens,
    checkpoint_prompts::{
        build_delta_checkpoint_prompt, CHECKPOINT_RESUME_PROMPT,
        COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT,
    },
    docs::DOCS_TOPICS,
    fingerprint::shared_prefix_fingerprint,
    render_sections, shared_prefix_len,
    system_prompt::build_system_sections,
    PromptSection,
};
use serde_json::{json, Value};

use super::ToolPromptContext;

use crate::{
    tool::{names as tn, ApplyPatchSurface, ToolRegistry, ToolSpec},
    types::{
        AgentIdentityView, AgentKind, AgentMemorySource, AgentOwnership, AgentProfilePreset,
        AgentRegistryStatus, AgentVisibility, AgentsMdKind, AgentsMdScope, AgentsMdSource,
        AuthorityClass, LoadedAgentMemory, LoadedAgentsMd, MessageBody, MessageEnvelope,
        MessageKind, MessageOrigin, Priority, SkillCatalogEntry, SkillScope, SkillsRuntimeView,
    },
};

fn exact_tokens_or_zero(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        estimate_text_tokens(text)
    }
}

fn default_identity() -> AgentIdentityView {
    AgentIdentityView {
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
    }
}

fn delegated_child_identity() -> AgentIdentityView {
    AgentIdentityView {
        agent_id: "child_worker".into(),
        name: None,
        kind: AgentKind::Child,
        visibility: AgentVisibility::Private,
        ownership: AgentOwnership::ParentSupervised,
        profile_preset: AgentProfilePreset::PrivateChild,
        can_rename: false,
        status: AgentRegistryStatus::Active,
        is_default_agent: false,
        incarnation: 1,
        parent_agent_id: Some("default".into()),
        lineage_parent_agent_id: Some("default".into()),
        delegated_from_task_id: Some("task-delegated-1".into()),
    }
}

fn operator_message(text: &str) -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text: text.into() },
    )
}

fn webhook_event_message() -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::WebhookEvent,
        MessageOrigin::Webhook {
            source: "github".into(),
            event_type: Some("check_suite".into()),
        },
        AuthorityClass::IntegrationSignal,
        Priority::Normal,
        MessageBody::Text {
            text: "check_suite completed".into(),
        },
    )
}

fn select_tools(all_tools: &[ToolSpec], wanted: &[&str]) -> Vec<ToolSpec> {
    wanted
        .iter()
        .map(|name| {
            all_tools
                .iter()
                .find(|spec| spec.name == *name)
                .unwrap_or_else(|| panic!("canonical tool spec `{name}` missing from ToolRegistry"))
                .clone()
        })
        .collect()
}

fn sample_skills(workspace_root: &Path) -> SkillsRuntimeView {
    SkillsRuntimeView {
        discoverable_skills: vec![SkillCatalogEntry {
            skill_id: "workspace:code-review".into(),
            root_id: "workspace-root".into(),
            skill_dir: "code-review".into(),
            name: "code-review".into(),
            description: "Review code changes with evidence-backed findings.".into(),
            path: workspace_root.join("skills/code-review/SKILL.md"),
            scope: SkillScope::Workspace,
        }]
        .into(),
        ..SkillsRuntimeView::default()
    }
}

fn serialize_section_entry(section: &PromptSection) -> Value {
    let chars = section.content.chars().count();
    json!({
        "name": section.name,
        "id": section.id,
        "layout": section.layout,
        "stability": section.stability,
        "chars": chars,
        "estimated_tokens": exact_tokens_or_zero(&section.content),
    })
}

struct TopologyProfileSpec {
    profile: &'static str,
    identity: AgentIdentityView,
    message: MessageEnvelope,
    workspace_root: PathBuf,
    agents_md: LoadedAgentsMd,
    memory: LoadedAgentMemory,
    skills: SkillsRuntimeView,
    tools: Vec<ToolSpec>,
    apply_patch_surface: Option<ApplyPatchSurface>,
}

fn build_profile_entry(spec: TopologyProfileSpec) -> Value {
    let sections = build_system_sections(
        &spec.identity,
        &spec.message,
        &spec.workspace_root,
        &spec.agents_md,
        &spec.memory,
        &spec.skills,
        &spec.tools,
        ToolPromptContext {
            apply_patch_surface: spec.apply_patch_surface,
        },
    );
    let shared_count = shared_prefix_len(&sections);
    let rendered_shared = render_sections(&sections[..shared_count]);
    let rendered_system = render_sections(&sections);
    let tool_description_chars: usize = spec
        .tools
        .iter()
        .map(|tool| tool.description.chars().count())
        .sum();
    let tool_description_estimated_tokens: usize = spec
        .tools
        .iter()
        .map(|tool| exact_tokens_or_zero(&tool.description))
        .sum();

    json!({
        "profile": spec.profile,
        "apply_patch_surface": spec.apply_patch_surface.map(|surface| match surface {
            ApplyPatchSurface::UnifiedDiffJson => "unified_diff_json",
            ApplyPatchSurface::CodexDslFreeform => "codex_dsl_freeform",
        }),
        "tool_count": spec.tools.len(),
        "tool_names": spec.tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>(),
        "tool_description_chars": tool_description_chars,
        "tool_description_estimated_tokens": tool_description_estimated_tokens,
        "shared_prefix_sections": shared_count,
        "shared_prefix_chars": rendered_shared.chars().count(),
        "shared_prefix_estimated_tokens": exact_tokens_or_zero(&rendered_shared),
        "shared_prefix_fingerprint": shared_prefix_fingerprint(&sections),
        "rendered_system_chars": rendered_system.chars().count(),
        "rendered_system_estimated_tokens": exact_tokens_or_zero(&rendered_system),
        "system_sections": sections.iter().map(serialize_section_entry).collect::<Vec<_>>(),
    })
}

/// Generate the checked-in multi-profile prompt and tool-description topology inventory.
pub fn prompt_topology_inventory() -> Result<Value> {
    let registry = ToolRegistry::new(PathBuf::from("."));
    let all_tools = registry.tool_specs()?;
    let workspace_root = PathBuf::from("/workspace/project");

    let steward_tools = select_tools(
        &all_tools,
        &[
            tn::WAIT_FOR,
            tn::CREATE_WORK_ITEM,
            tn::PICK_WORK_ITEM,
            tn::GET_WORK_ITEM,
            tn::LIST_WORK_ITEMS,
            tn::UPDATE_WORK_ITEM,
            tn::COMPLETE_WORK_ITEM,
            tn::CREATE_TIMER,
            tn::LIST_TIMERS,
            tn::GET_TIMER,
            tn::EDIT_TIMER_CONTENT,
            tn::CANCEL_TIMER,
            tn::MEMORY_SEARCH,
            tn::MEMORY_GET,
            tn::WEB_SEARCH,
            tn::WEB_FETCH,
            tn::SEND_AGENT_MESSAGE,
            tn::ENQUEUE,
        ],
    );

    let coding_tools = select_tools(
        &all_tools,
        &[
            tn::EXEC_COMMAND,
            tn::EXEC_COMMAND_BATCH,
            tn::APPLY_PATCH,
            tn::WAIT_FOR,
            tn::CREATE_WORK_ITEM,
            tn::UPDATE_WORK_ITEM,
            tn::COMPLETE_WORK_ITEM,
            tn::GET_WORK_ITEM,
            tn::LIST_WORK_ITEMS,
            tn::TASK_STATUS,
            tn::TASK_STOP,
            tn::GET_WORKSPACE_STATE,
            tn::SWITCH_WORKSPACE,
            tn::CREATE_WORKTREE,
        ],
    );

    let delegated_tools = select_tools(
        &all_tools,
        &[
            tn::EXEC_COMMAND,
            tn::APPLY_PATCH,
            tn::WAIT_FOR,
            tn::SEND_AGENT_MESSAGE,
        ],
    );

    let workspace_agents_md = LoadedAgentsMd {
        workspace_source: Some(AgentsMdSource {
            scope: AgentsMdScope::Workspace,
            kind: AgentsMdKind::AgentsMd,
            path: PathBuf::from("/workspace/project/AGENTS.md"),
            content: "# Project Guidelines\nRun cargo check before finishing.".into(),
        }),
        ..LoadedAgentsMd::default()
    };

    let steward_agents_md = LoadedAgentsMd {
        agent_source: Some(AgentsMdSource {
            scope: AgentsMdScope::Agent,
            kind: AgentsMdKind::AgentsMd,
            path: PathBuf::from("/agents/steward/AGENTS.md"),
            content: "# Steward Role\nTrack commitments without shell execution.".into(),
        }),
        ..LoadedAgentsMd::default()
    };

    let full_agents_md = LoadedAgentsMd {
        user_global_source: Some(AgentsMdSource {
            scope: AgentsMdScope::UserGlobal,
            kind: AgentsMdKind::AgentsMd,
            path: PathBuf::from("/users/operator/.agents/AGENTS.md"),
            content: "Prefer concise direct answers.".into(),
        }),
        agent_source: steward_agents_md.agent_source.clone(),
        workspace_source: workspace_agents_md.workspace_source.clone(),
        ..LoadedAgentsMd::default()
    };

    let sample_operator_memory = AgentMemorySource {
        path: PathBuf::from("/agents/steward/memory/operator.md"),
        content: "Default reply language: zh-CN.".into(),
        total_chars: 30,
        truncated: false,
    };
    let sample_self_memory = AgentMemorySource {
        path: PathBuf::from("/agents/steward/memory/self.md"),
        content: "Maintain weekly digest checkpoints.".into(),
        total_chars: 35,
        truncated: false,
    };

    let profiles = vec![
        build_profile_entry(TopologyProfileSpec {
            profile: "zero-tools",
            identity: default_identity(),
            message: operator_message("Explain the current architecture."),
            workspace_root: workspace_root.clone(),
            agents_md: LoadedAgentsMd::default(),
            memory: LoadedAgentMemory::default(),
            skills: SkillsRuntimeView::default(),
            tools: Vec::new(),
            apply_patch_surface: None,
        }),
        build_profile_entry(TopologyProfileSpec {
            profile: "no-shell-steward",
            identity: default_identity(),
            message: operator_message("Schedule the standup reminder and check notes."),
            workspace_root: PathBuf::from("/agents/steward"),
            agents_md: steward_agents_md,
            memory: LoadedAgentMemory {
                operator_source: Some(sample_operator_memory.clone()),
                ..LoadedAgentMemory::default()
            },
            skills: sample_skills(Path::new("/agents/steward")),
            tools: steward_tools,
            apply_patch_surface: None,
        }),
        build_profile_entry(TopologyProfileSpec {
            profile: "coding-shell-patch-json",
            identity: default_identity(),
            message: operator_message("Inspect crates/holon-context and run tests."),
            workspace_root: workspace_root.clone(),
            agents_md: workspace_agents_md.clone(),
            memory: LoadedAgentMemory::default(),
            skills: sample_skills(&workspace_root),
            tools: coding_tools.clone(),
            apply_patch_surface: Some(ApplyPatchSurface::UnifiedDiffJson),
        }),
        build_profile_entry(TopologyProfileSpec {
            profile: "coding-shell-patch-dsl",
            identity: default_identity(),
            message: operator_message("Inspect crates/holon-context and run tests."),
            workspace_root: workspace_root.clone(),
            agents_md: workspace_agents_md,
            memory: LoadedAgentMemory::default(),
            skills: sample_skills(&workspace_root),
            tools: coding_tools,
            apply_patch_surface: Some(ApplyPatchSurface::CodexDslFreeform),
        }),
        build_profile_entry(TopologyProfileSpec {
            profile: "delegated-child",
            identity: delegated_child_identity(),
            message: operator_message("Verify the bounded module slice and return findings."),
            workspace_root: workspace_root.clone(),
            agents_md: LoadedAgentsMd::default(),
            memory: LoadedAgentMemory::default(),
            skills: SkillsRuntimeView::default(),
            tools: delegated_tools,
            apply_patch_surface: Some(ApplyPatchSurface::UnifiedDiffJson),
        }),
        build_profile_entry(TopologyProfileSpec {
            profile: "full-surface",
            identity: default_identity(),
            message: webhook_event_message(),
            workspace_root: workspace_root.clone(),
            agents_md: full_agents_md,
            memory: LoadedAgentMemory {
                operator_source: Some(sample_operator_memory),
                self_source: Some(sample_self_memory),
                ..LoadedAgentMemory::default()
            },
            skills: sample_skills(&workspace_root),
            tools: all_tools,
            apply_patch_surface: Some(ApplyPatchSurface::UnifiedDiffJson),
        }),
    ];

    let sample_delta_checkpoint =
        build_delta_checkpoint_prompt(Some(2), Some(1), "Inspected src/lib.rs; next run check.");
    let checkpoint_prompts = json!({
        "compaction_full_progress": {
            "chars": COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT.chars().count(),
            "estimated_tokens": exact_tokens_or_zero(COMPACTION_BOUNDARY_FULL_PROGRESS_CHECKPOINT_PROMPT),
        },
        "checkpoint_resume": {
            "chars": CHECKPOINT_RESUME_PROMPT.chars().count(),
            "estimated_tokens": exact_tokens_or_zero(CHECKPOINT_RESUME_PROMPT),
        },
        "compaction_delta_sample": {
            "chars": sample_delta_checkpoint.chars().count(),
            "estimated_tokens": exact_tokens_or_zero(&sample_delta_checkpoint),
        },
    });
    let embedded_docs = DOCS_TOPICS
        .iter()
        .map(|topic| {
            json!({
                "slug": topic.slug,
                "title": topic.title,
                "chars": topic.content.chars().count(),
                "estimated_tokens": exact_tokens_or_zero(topic.content),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "version": 1,
        "source_of_truth": {
            "system_sections": "crates/holon-context/src/system_prompt.rs build_system_sections()",
            "tool_guidance": "crates/holon-context/src/tools.rs tool_sections_with_context()",
            "tool_descriptions": "crates/holon-tools/src/tool/tools/mod.rs builtin_tool_definitions()",
            "checkpoint_prompts": "crates/holon-context/src/checkpoint_prompts.rs",
            "embedded_docs": "crates/holon-context/src/docs/mod.rs DOCS_TOPICS",
        },
        "checkpoint_prompts": checkpoint_prompts,
        "embedded_docs": embedded_docs,
        "profiles": profiles,
    }))
}
