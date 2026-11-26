//! Prompt assembly module
//!
//! This module organizes prompt construction into clear layers:
//! - `tools`: Tool-specific prompt guidance (registry-style)
//! - Top-level: execution-oriented sections and overall assembly

mod resources;
pub mod tools;
mod topology_inventory;

use holon_context::fingerprint::{
    prompt_cache_key, prompt_cache_scope_diagnostics, prompt_cache_scope_fingerprint,
    prompt_cache_scope_fingerprint_for_agent, prompt_context_fingerprint,
    reprojected_context_fingerprint, shared_prefix_fingerprint,
};
pub(crate) use holon_context::section;
pub use holon_context::{
    render_section, shared_prefix_len, time, PromptCacheIdentity, PromptCacheScopeDiagnostics,
    PromptLayout, PromptSection,
};
use holon_context::{
    render_sections,
    system_prompt::{build_system_sections, describe_agents_md_source},
};
pub use resources::PromptResource;
pub use tools::{tool_sections, ToolPromptContext};
pub use topology_inventory::prompt_topology_inventory;

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    context::{
        build_context_from_snapshot, build_context_with_default_external_ingress,
        reproject_recent_turns, reproject_work_item_scoped_with_limit, BuiltContext, ContextConfig,
        ContextHistoryPreparation, ContextPlanEvidence, ContextSnapshot, RecentTurnsReprojection,
        WorkItemScopedReprojection,
    },
    projection_eval::{
        compare_prompt_history_selectors, manifest_from_effective_prompt,
        manifest_from_effective_prompt_with_selector, HistorySelector, ProjectionBindingSummary,
        ProjectionEvidenceIndex, ProjectionManifest, ProjectionOwner, ProjectionScorecard,
    },
    storage::AppStorage,
    system::{execution_policy_summary_lines, ExecutionSnapshot},
    tool::{ApplyPatchSurface, ToolSpec},
    types::{
        AgentIdentityView, AgentState, ContinuationResolution, ExternalTriggerRecord,
        LoadedAgentMemory, LoadedAgentsMd, MessageEnvelope, SkillsRuntimeView,
    },
};

pub use holon_domain::PromptStability;

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error(
    "context build failed: configured_budget={configured_estimated_tokens}, effective_budget={effective_estimated_tokens}, runtime_time_reservation={runtime_time_reserved_tokens}"
)]
pub(crate) struct ContextBuildBudget {
    pub configured_estimated_tokens: usize,
    pub effective_estimated_tokens: usize,
    pub runtime_time_reserved_tokens: usize,
}

#[derive(Debug)]
pub struct EffectivePrompt {
    pub agent_home: PathBuf,
    pub identity: AgentIdentityView,
    pub execution: ExecutionSnapshot,
    pub loaded_agents_md: PromptResource<LoadedAgentsMd>,
    pub cache_identity: PromptCacheIdentity,
    pub loaded_agent_memory: PromptResource<LoadedAgentMemory>,
    pub system_sections: Vec<PromptSection>,
    pub context_sections: Vec<PromptSection>,
    pub rendered_system_prompt: String,
    pub rendered_context_attachment: String,
    pub(crate) projection_owner: ProjectionOwner,
    pub(crate) projection_binding: Option<ProjectionBindingSummary>,
    pub(crate) projection_turn_id: Option<String>,
    pub(crate) projection_evidence: ProjectionEvidenceIndex,
    pub(crate) context_plan_evidence: ContextPlanEvidence,
    pub(crate) recent_turns_reprojection: Option<RecentTurnsReprojection>,
    pub(crate) frozen_history_policy: ContextHistoryPreparation,
    pub(crate) frozen_projection_budget: Option<usize>,
    pub(crate) frozen_recent_message_limit: Option<usize>,
    pub(crate) _resource_read_material: Option<ResourcePromptMaterial>,
}

impl Clone for EffectivePrompt {
    fn clone(&self) -> Self {
        let material = self._resource_read_material.clone();
        Self {
            agent_home: self.agent_home.clone(),
            identity: self.identity.clone(),
            execution: self.execution.clone(),
            loaded_agents_md: self.loaded_agents_md.clone(),
            cache_identity: self.cache_identity.clone(),
            loaded_agent_memory: self.loaded_agent_memory.clone(),
            system_sections: self.system_sections.clone(),
            context_sections: self.context_sections.clone(),
            rendered_system_prompt: self.rendered_system_prompt.clone(),
            rendered_context_attachment: self.rendered_context_attachment.clone(),
            projection_owner: self.projection_owner.clone(),
            projection_binding: self.projection_binding.clone(),
            projection_turn_id: self.projection_turn_id.clone(),
            projection_evidence: self.projection_evidence.clone(),
            context_plan_evidence: self.context_plan_evidence.clone(),
            recent_turns_reprojection: self.recent_turns_reprojection.clone(),
            frozen_history_policy: self.frozen_history_policy.clone(),
            frozen_projection_budget: self.frozen_projection_budget.clone(),
            frozen_recent_message_limit: self.frozen_recent_message_limit.clone(),
            _resource_read_material: material,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ResourcePromptMaterial {
    pub(crate) observation:
        std::sync::Arc<crate::runtime::resource_observation::ResourceObservation>,
    pub(crate) allocation:
        Option<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    pub(crate) model: Option<std::sync::Arc<crate::runtime::PreparedAgentModelState>>,
}
impl Clone for ResourcePromptMaterial {
    fn clone(&self) -> Self {
        Self {
            observation: self.observation.clone(),
            allocation: self.allocation.as_ref().map(|guard| guard.fork_owned()),
            model: self.model.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum HistoryReprojectionOutcome {
    Adopted {
        prompt: EffectivePrompt,
        turns_loaded: usize,
    },
    NoCurrentWorkItem,
    NoOwnedTurnRecords,
    IdenticalRenderNoop {
        prompt: EffectivePrompt,
        turns_loaded: usize,
    },
}

impl EffectivePrompt {
    pub(crate) fn with_media_tool_defaults(
        mut self,
        _defaults: &crate::provider::media::MediaToolDefaults,
        available_tools: &[ToolSpec],
    ) -> Self {
        let view = available_tools
            .iter()
            .any(|tool| tool.name == crate::tool::names::VIEW_IMAGE);
        let generate = available_tools
            .iter()
            .any(|tool| tool.name == crate::tool::names::GENERATE_IMAGE);
        if !view && !generate {
            return self;
        }
        let mut lines = vec![
            "Media model overrides are optional and single-call only; omission preserves configured/default routing. Exact overrides do not silently fall back. Discover operation-specific candidates with ListModelProviders/ListProviderModels.".to_string(),
        ];
        // Route identifiers and live availability stay in tool results/error hints so
        // transient route-health shifts never churn the system prompt or cache key.
        if generate {
            lines.push(
                "GenerateImage final output_format defaults to PNG; omit size/background/aspect_ratio/resolution to use route-specific defaults unless the selected model's generation contract is known.".to_string(),
            );
        }
        self.system_sections
            .retain(|section| section.name != "media_tool_defaults");
        let footer = self
            .system_sections
            .iter()
            .position(|section| section.name == "response_language")
            .unwrap_or(self.system_sections.len());
        self.system_sections.insert(
            footer,
            section(
                "media_tool_defaults",
                PromptStability::AgentScoped,
                lines.join("\n"),
            ),
        );
        self.rendered_system_prompt = render_sections(&self.system_sections);
        let scope = prompt_cache_scope_fingerprint_for_agent(
            &self.cache_identity.agent_id,
            &self.execution,
            &self.system_sections,
            &self.context_sections,
            available_tools,
        );
        self.cache_identity.prompt_cache_key =
            prompt_cache_key(&self.cache_identity.agent_id, &scope);
        self.cache_identity.context_fingerprint = reprojected_context_fingerprint(
            &self.cache_identity,
            &self.execution,
            &self.system_sections,
            &self.context_sections,
            available_tools,
        );
        self
    }

    pub(crate) fn history_policy(&self) -> ContextHistoryPreparation {
        self.frozen_history_policy
    }

    pub(crate) fn frozen_projection_budget(&self) -> Option<usize> {
        self.frozen_projection_budget
    }

    pub(crate) fn frozen_recent_message_limit(&self) -> Option<usize> {
        self.frozen_recent_message_limit
    }

    /// Identity of the prepared history acquisition. Budget and selector rendering
    /// preserve it. This is not an owner publication revision.
    pub fn context_snapshot_id(&self) -> Option<u64> {
        self.recent_turns_reprojection
            .as_ref()
            .map(RecentTurnsReprojection::snapshot_id)
    }

    pub fn projection_manifest(&self) -> ProjectionManifest {
        manifest_from_effective_prompt(self)
    }

    /// Build a request-scoped manifest for selector comparison. This does not
    /// alter the prompt, runtime state, scheduler, or provider request.
    pub fn projection_manifest_for_selector(
        &self,
        selector: HistorySelector,
    ) -> ProjectionManifest {
        manifest_from_effective_prompt_with_selector(self, selector)
    }

    pub fn compare_history_selectors(&self) -> serde_json::Result<ProjectionScorecard> {
        compare_prompt_history_selectors(self)
    }

    pub(crate) fn reproject_for_history_selector(
        &self,
        budget: usize,
        available_tools: &[ToolSpec],
        selector: HistorySelector,
        work_item_scoped_window: usize,
    ) -> Result<HistoryReprojectionOutcome> {
        if selector == HistorySelector::WorkItemScoped
            && !matches!(self.projection_owner, ProjectionOwner::WorkItem { .. })
        {
            return Ok(HistoryReprojectionOutcome::NoCurrentWorkItem);
        }
        let Some(reprojection) = self.recent_turns_reprojection.as_ref() else {
            return Ok(HistoryReprojectionOutcome::NoOwnedTurnRecords);
        };
        let (selected, replacement, replacement_evidence, turns_loaded) = match selector {
            HistorySelector::RecentTurns => {
                let selected = reprojection.select_recent();
                let Some(replacement) = reproject_recent_turns(&selected, budget) else {
                    return Ok(HistoryReprojectionOutcome::NoOwnedTurnRecords);
                };
                let evidence = selected.agent_evidence();
                let turns_loaded = selected.turn_count();
                (selected, replacement, evidence, turns_loaded)
            }
            HistorySelector::WorkItemScoped => {
                match reproject_work_item_scoped_with_limit(
                    reprojection,
                    budget,
                    work_item_scoped_window,
                )? {
                    WorkItemScopedReprojection::NoCurrentWorkItem => {
                        return Ok(HistoryReprojectionOutcome::NoCurrentWorkItem);
                    }
                    WorkItemScopedReprojection::NoOwnedTurnRecords => {
                        return Ok(HistoryReprojectionOutcome::NoOwnedTurnRecords);
                    }
                    WorkItemScopedReprojection::Rendered {
                        section,
                        evidence,
                        turns_loaded,
                    } => (
                        reprojection.select_scoped(work_item_scoped_window)?,
                        section,
                        evidence,
                        turns_loaded,
                    ),
                }
            }
        };
        let replacement_for_evidence = replacement.clone();
        let mut context_sections = self
            .context_sections
            .iter()
            .filter(|section| section.name != "recent_turns")
            .cloned()
            .collect::<Vec<_>>();
        let insertion_index = self
            .context_sections
            .iter()
            .position(|section| section.name == "recent_turns")
            .unwrap_or(context_sections.len())
            .min(context_sections.len());
        context_sections.insert(insertion_index, replacement);
        let rendered_context_attachment = render_sections(&context_sections);
        let mut prompt = self.clone();
        prompt.recent_turns_reprojection = Some(selected);
        prompt
            .projection_evidence
            .insert("recent_turns".to_string(), replacement_evidence);
        prompt
            .context_plan_evidence
            .record_reprojection("recent_turns", Some(&replacement_for_evidence));
        if rendered_context_attachment == self.rendered_context_attachment {
            return Ok(HistoryReprojectionOutcome::IdenticalRenderNoop {
                prompt,
                turns_loaded,
            });
        }
        let context_fingerprint = reprojected_context_fingerprint(
            &self.cache_identity,
            &self.execution,
            &self.system_sections,
            &context_sections,
            available_tools,
        );
        prompt.context_sections = context_sections;
        prompt.rendered_context_attachment = rendered_context_attachment;
        prompt.cache_identity.context_fingerprint = context_fingerprint;
        Ok(HistoryReprojectionOutcome::Adopted {
            prompt,
            turns_loaded,
        })
    }

    pub(crate) fn recent_turns_initial_budget(&self) -> Option<usize> {
        self.recent_turns_reprojection
            .as_ref()
            .map(RecentTurnsReprojection::initial_budget)
    }

    pub(crate) fn recent_turn_count(&self) -> usize {
        self.recent_turns_reprojection
            .as_ref()
            .map_or(0, RecentTurnsReprojection::turn_count)
    }

    pub(crate) fn history_reprojection_budget(&self, maximum: usize) -> usize {
        let recent_turns_allocated = self
            .context_plan_evidence
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == "recent_turns")
            .map_or(0, |decision| decision.allocated_estimated_tokens);
        let other_allocated = self
            .context_plan_evidence
            .allocated_estimated_tokens
            .saturating_sub(recent_turns_allocated);
        self.context_plan_evidence
            .total_budget_estimated_tokens
            .saturating_sub(other_allocated)
            .min(maximum)
    }

    pub(crate) fn reproject_recent_turns(
        &self,
        budget: usize,
        available_tools: &[ToolSpec],
    ) -> Option<Self> {
        let reprojection = self.recent_turns_reprojection.as_ref()?;
        if budget >= reprojection.initial_budget() {
            return None;
        }
        let replacement = reproject_recent_turns(reprojection, budget);
        let replacement_for_evidence = replacement.clone();
        let mut context_sections = self
            .context_sections
            .iter()
            .filter(|section| section.name != "recent_turns")
            .cloned()
            .collect::<Vec<_>>();
        if let Some(replacement) = replacement {
            let insertion_index = self
                .context_sections
                .iter()
                .position(|section| section.name == "recent_turns")
                .unwrap_or(context_sections.len())
                .min(context_sections.len());
            context_sections.insert(insertion_index, replacement);
        }
        let rendered_context_attachment = render_sections(&context_sections);
        if rendered_context_attachment == self.rendered_context_attachment {
            return None;
        }
        let context_fingerprint = reprojected_context_fingerprint(
            &self.cache_identity,
            &self.execution,
            &self.system_sections,
            &context_sections,
            available_tools,
        );
        let mut prompt = self.clone();
        prompt.recent_turns_reprojection = Some(reprojection.with_initial_budget(budget));
        prompt.context_sections = context_sections;
        prompt.rendered_context_attachment = rendered_context_attachment;
        prompt.cache_identity.context_fingerprint = context_fingerprint;
        prompt
            .context_plan_evidence
            .record_reprojection("recent_turns", replacement_for_evidence.as_ref());
        Some(prompt)
    }

    pub fn cache_scope_diagnostics(&self) -> PromptCacheScopeDiagnostics {
        prompt_cache_scope_diagnostics(&self.system_sections, &self.context_sections)
    }

    pub fn render_dump(&self) -> String {
        let mut output = Vec::new();
        let cache_diagnostics = self.cache_scope_diagnostics();

        output.push("## Prompt Topology".to_string());
        output.push("".to_string());
        output.push("Section inventory:".to_string());
        append_section_inventory(&mut output, "system", &self.system_sections);
        append_section_inventory(&mut output, "context", &self.context_sections);
        let shared_sections = shared_prefix_len(&self.system_sections);
        output.push(format!(
            "Shared system prefix: sections={}, chars={}, fingerprint={}",
            shared_sections,
            render_sections(&self.system_sections[..shared_sections])
                .chars()
                .count(),
            shared_prefix_fingerprint(&self.system_sections)
        ));
        output.push(format!(
            "Rendered system chars: {}",
            self.rendered_system_prompt.chars().count()
        ));
        output.push(format!(
            "Rendered context chars: {}",
            self.rendered_context_attachment.chars().count()
        ));
        output.push("".to_string());
        output.push("## Execution State".to_string());
        output.push("".to_string());
        output.push(format!("Agent home: {}", self.agent_home.display()));
        output.push(format!("Agent id: {}", self.identity.agent_id));
        output.push(format!("Identity status: {:?}", self.identity.status));
        output.push(format!(
            "Identity relation: {}",
            self.identity.relation_summary()
        ));
        output.push(format!(
            "Prompt cache key: {}",
            self.cache_identity.prompt_cache_key
        ));
        output.push(format!(
            "Context fingerprint: {}",
            self.cache_identity.context_fingerprint
        ));
        output.push(format!(
            "Compression epoch: {}",
            self.cache_identity.compression_epoch
        ));
        output.push(format!(
            "Cacheable sections: system={}, context={}",
            cache_diagnostics.cacheable_system_sections,
            cache_diagnostics.cacheable_context_sections
        ));
        output.push(format!(
            "Turn-scoped sections: system={}, context={}",
            cache_diagnostics.turn_scoped_system_sections,
            cache_diagnostics.turn_scoped_context_sections
        ));
        output.push(format!(
            "Cacheable chars: system={}, context={}",
            cache_diagnostics.cacheable_system_chars, cache_diagnostics.cacheable_context_chars
        ));
        output.push(format!(
            "Turn-scoped chars: system={}, context={}",
            cache_diagnostics.turn_scoped_system_chars, cache_diagnostics.turn_scoped_context_chars
        ));
        output.push(format!(
            "Cacheable system fingerprint: {}",
            cache_diagnostics.cacheable_system_fingerprint
        ));
        output.push(format!(
            "Cacheable context fingerprint: {}",
            cache_diagnostics.cacheable_context_fingerprint
        ));
        output.push(format!(
            "Turn-scoped fingerprint: {}",
            cache_diagnostics.turn_scoped_fingerprint
        ));
        output.extend(execution_policy_summary_lines(&self.execution));
        output.push(format!(
            "User-global AGENTS.md: {}",
            describe_agents_md_source(
                self.loaded_agents_md.user_global_source.as_ref(),
                self.loaded_agents_md.user_global_status,
            )
        ));
        output.push(format!(
            "Agent AGENTS.md: {}",
            describe_agents_md_source(
                self.loaded_agents_md.agent_source.as_ref(),
                self.loaded_agents_md.agent_status,
            )
        ));
        output.push(format!(
            "Workspace AGENTS.md: {}",
            describe_agents_md_source(
                self.loaded_agents_md.workspace_source.as_ref(),
                self.loaded_agents_md.workspace_status,
            )
        ));
        output.push("".to_string());
        output.push("System sections:".to_string());
        for (index, section) in self.system_sections.iter().enumerate() {
            output.push(format!(
                "  - #{} [{}] (id: {}, stability: {:?}, cache scope: {}, chars: {})",
                index + 1,
                section.name,
                section.id,
                section.stability,
                prompt_cache_scope_label(section.stability),
                section.content.chars().count()
            ));
        }
        output.push("".to_string());
        output.push("Context sections:".to_string());
        for (index, section) in self.context_sections.iter().enumerate() {
            output.push(format!(
                "  - #{} [{}] (id: {}, stability: {:?}, cache scope: {}, chars: {})",
                index + 1,
                section.name,
                section.id,
                section.stability,
                prompt_cache_scope_label(section.stability),
                section.content.chars().count()
            ));
        }
        output.push("".to_string());
        output.push("Context budget plan:".to_string());
        output.push(format!(
            "  - total estimated tokens: {}",
            self.context_plan_evidence.total_budget_estimated_tokens
        ));
        output.push(format!(
            "  - allocated estimated tokens: {}",
            self.context_plan_evidence.allocated_estimated_tokens
        ));
        for decision in &self.context_plan_evidence.decisions {
            output.push(format!(
                "  - [{}] section={} outcome={} reason={} requested={} minimum={} allocated={}",
                decision.candidate_id,
                decision.section_name,
                decision.outcome.label(),
                decision.reason.code(),
                decision.requested_estimated_tokens,
                decision.minimum_estimated_tokens,
                decision.allocated_estimated_tokens,
            ));
        }
        output.push("".to_string());
        output.push("## Rendered Prompt Content".to_string());
        output.push("".to_string());

        output.push("== System Sections ==".to_string());
        for section in &self.system_sections {
            output.push(format!(
                "[{}][id: {}][{:?}][{:?}]\n{}",
                section.name, section.id, section.stability, section.layout, section.content
            ));
        }
        output.push("== Context Sections ==".to_string());
        for section in &self.context_sections {
            output.push(format!(
                "[{}][id: {}][{:?}][{:?}]\n{}",
                section.name, section.id, section.stability, section.layout, section.content
            ));
        }
        output.push("== Rendered System Prompt ==".to_string());
        output.push(self.rendered_system_prompt.clone());
        output.push("== Rendered Context Attachment ==".to_string());
        output.push(self.rendered_context_attachment.clone());
        output.join("\n\n")
    }
}

fn append_section_inventory(output: &mut Vec<String>, label: &str, sections: &[PromptSection]) {
    output.push(format!(
        "  - {}: total={}, stable={}, agent_scoped={}, turn_scoped={}",
        label,
        sections.len(),
        count_sections_by_stability(sections, PromptStability::Stable),
        count_sections_by_stability(sections, PromptStability::AgentScoped),
        count_sections_by_stability(sections, PromptStability::TurnScoped)
    ));
}

fn count_sections_by_stability(sections: &[PromptSection], stability: PromptStability) -> usize {
    sections
        .iter()
        .filter(|section| section.stability == stability)
        .count()
}

fn prompt_cache_scope_label(stability: PromptStability) -> &'static str {
    match stability {
        PromptStability::Stable | PromptStability::AgentScoped => "included",
        PromptStability::TurnScoped => "turn-only",
    }
}

pub fn build_effective_prompt(
    storage: &AppStorage,
    session: &AgentState,
    execution: &ExecutionSnapshot,
    current_message: &MessageEnvelope,
    config: &ContextConfig,
    workspace_root: &Path,
    agent_home: &Path,
    identity: &AgentIdentityView,
    loaded_agents_md: LoadedAgentsMd,
    loaded_agent_memory: LoadedAgentMemory,
    skills: &SkillsRuntimeView,
    available_tools: &[ToolSpec],
    continuation: Option<&ContinuationResolution>,
) -> Result<EffectivePrompt> {
    build_effective_prompt_with_default_external_ingress(
        storage,
        session,
        execution,
        current_message,
        config,
        workspace_root,
        agent_home,
        identity,
        loaded_agents_md,
        loaded_agent_memory,
        skills,
        available_tools,
        continuation,
        None,
    )
}

pub fn build_effective_prompt_with_default_external_ingress(
    storage: &AppStorage,
    session: &AgentState,
    execution: &ExecutionSnapshot,
    current_message: &MessageEnvelope,
    config: &ContextConfig,
    workspace_root: &Path,
    agent_home: &Path,
    identity: &AgentIdentityView,
    loaded_agents_md: LoadedAgentsMd,
    loaded_agent_memory: LoadedAgentMemory,
    skills: &SkillsRuntimeView,
    available_tools: &[ToolSpec],
    continuation: Option<&ContinuationResolution>,
    default_external_ingress: Option<&ExternalTriggerRecord>,
) -> Result<EffectivePrompt> {
    build_effective_prompt_with_tool_prompt_context_and_default_external_ingress(
        storage,
        session,
        execution,
        current_message,
        config,
        workspace_root,
        agent_home,
        identity,
        loaded_agents_md,
        loaded_agent_memory,
        skills,
        available_tools,
        ToolPromptContext::default(),
        continuation,
        default_external_ingress,
    )
}

pub fn build_effective_prompt_with_apply_patch_surface(
    storage: &AppStorage,
    session: &AgentState,
    execution: &ExecutionSnapshot,
    current_message: &MessageEnvelope,
    config: &ContextConfig,
    workspace_root: &Path,
    agent_home: &Path,
    identity: &AgentIdentityView,
    loaded_agents_md: LoadedAgentsMd,
    loaded_agent_memory: LoadedAgentMemory,
    skills: &SkillsRuntimeView,
    available_tools: &[ToolSpec],
    apply_patch_surface: ApplyPatchSurface,
    continuation: Option<&ContinuationResolution>,
) -> Result<EffectivePrompt> {
    build_effective_prompt_with_apply_patch_surface_and_default_external_ingress(
        storage,
        session,
        execution,
        current_message,
        config,
        workspace_root,
        agent_home,
        identity,
        loaded_agents_md,
        loaded_agent_memory,
        skills,
        available_tools,
        apply_patch_surface,
        continuation,
        None,
    )
}

pub fn build_effective_prompt_with_apply_patch_surface_and_default_external_ingress(
    storage: &AppStorage,
    session: &AgentState,
    execution: &ExecutionSnapshot,
    current_message: &MessageEnvelope,
    config: &ContextConfig,
    workspace_root: &Path,
    agent_home: &Path,
    identity: &AgentIdentityView,
    loaded_agents_md: LoadedAgentsMd,
    loaded_agent_memory: LoadedAgentMemory,
    skills: &SkillsRuntimeView,
    available_tools: &[ToolSpec],
    apply_patch_surface: ApplyPatchSurface,
    continuation: Option<&ContinuationResolution>,
    default_external_ingress: Option<&ExternalTriggerRecord>,
) -> Result<EffectivePrompt> {
    build_effective_prompt_with_tool_prompt_context_and_default_external_ingress(
        storage,
        session,
        execution,
        current_message,
        config,
        workspace_root,
        agent_home,
        identity,
        loaded_agents_md,
        loaded_agent_memory,
        skills,
        available_tools,
        ToolPromptContext {
            apply_patch_surface: Some(apply_patch_surface),
        },
        continuation,
        default_external_ingress,
    )
}

fn build_effective_prompt_with_tool_prompt_context_and_default_external_ingress(
    storage: &AppStorage,
    session: &AgentState,
    execution: &ExecutionSnapshot,
    current_message: &MessageEnvelope,
    config: &ContextConfig,
    workspace_root: &Path,
    agent_home: &Path,
    identity: &AgentIdentityView,
    loaded_agents_md: LoadedAgentsMd,
    loaded_agent_memory: LoadedAgentMemory,
    skills: &SkillsRuntimeView,
    available_tools: &[ToolSpec],
    tool_prompt_context: ToolPromptContext,
    continuation: Option<&ContinuationResolution>,
    default_external_ingress: Option<&ExternalTriggerRecord>,
) -> Result<EffectivePrompt> {
    let mut time_reserved_config = config.clone();
    time_reserved_config.prompt_budget_estimated_tokens = config
        .prompt_budget_estimated_tokens
        .checked_sub(time::RUNTIME_TIME_RESERVED_TOKENS)
        .ok_or_else(|| anyhow::anyhow!("context budget cannot reserve runtime time"))?;
    let built_context = build_context_with_default_external_ingress(
        storage,
        session,
        execution,
        skills,
        current_message,
        continuation,
        &time_reserved_config,
        agent_home,
        default_external_ingress,
    )
    .map_err(|error| {
        if error.is::<crate::context::ContextPlanningError>() {
            error.context(ContextBuildBudget {
                configured_estimated_tokens: config.prompt_budget_estimated_tokens,
                effective_estimated_tokens: time_reserved_config.prompt_budget_estimated_tokens,
                runtime_time_reserved_tokens: time::RUNTIME_TIME_RESERVED_TOKENS,
            })
        } else {
            error
        }
    })?;
    assemble_effective_prompt(
        built_context,
        session,
        execution,
        current_message,
        workspace_root,
        agent_home,
        identity,
        loaded_agents_md,
        loaded_agent_memory,
        skills,
        available_tools,
        tool_prompt_context,
    )
}

/// Pure prompt construction over a fully prepared context. The acquisition
/// config has already reserved runtime-time tokens before its SQLite view opens.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_effective_prompt_from_context_snapshot(
    snapshot: &ContextSnapshot,
    workspace_root: &Path,
    agent_home: &Path,
    identity: &AgentIdentityView,
    loaded_agents_md: impl Into<PromptResource<LoadedAgentsMd>>,
    loaded_agent_memory: impl Into<PromptResource<LoadedAgentMemory>>,
    available_tools: &[ToolSpec],
    tool_prompt_context: ToolPromptContext,
) -> Result<EffectivePrompt> {
    let effective_estimated_tokens = snapshot.prepared_budget();
    let built_context =
        build_context_from_snapshot(snapshot, effective_estimated_tokens).map_err(|error| {
            if error.is::<crate::context::ContextPlanningError>() {
                error.context(ContextBuildBudget {
                    configured_estimated_tokens: effective_estimated_tokens
                        + time::RUNTIME_TIME_RESERVED_TOKENS,
                    effective_estimated_tokens,
                    runtime_time_reserved_tokens: time::RUNTIME_TIME_RESERVED_TOKENS,
                })
            } else {
                error
            }
        })?;
    let mut prompt = assemble_effective_prompt(
        built_context,
        snapshot.agent(),
        snapshot.execution(),
        snapshot.current_message(),
        workspace_root,
        agent_home,
        identity,
        loaded_agents_md,
        loaded_agent_memory,
        snapshot.skills(),
        available_tools,
        tool_prompt_context,
    )?;
    prompt._resource_read_material =
        snapshot
            .resident_resources()
            .map(|observation| ResourcePromptMaterial {
                observation: observation.clone(),
                allocation: snapshot.resource_allocation().cloned(),
                model: None,
            });
    Ok(prompt)
}

#[allow(clippy::too_many_arguments)]
fn assemble_effective_prompt(
    built_context: BuiltContext,
    session: &AgentState,
    execution: &ExecutionSnapshot,
    current_message: &MessageEnvelope,
    workspace_root: &Path,
    agent_home: &Path,
    identity: &AgentIdentityView,
    loaded_agents_md: impl Into<PromptResource<LoadedAgentsMd>>,
    loaded_agent_memory: impl Into<PromptResource<LoadedAgentMemory>>,
    skills: &SkillsRuntimeView,
    available_tools: &[ToolSpec],
    tool_prompt_context: ToolPromptContext,
) -> Result<EffectivePrompt> {
    let loaded_agents_md = loaded_agents_md.into();
    let loaded_agent_memory = loaded_agent_memory.into();
    let system_sections = build_system_sections(
        identity,
        current_message,
        workspace_root,
        &loaded_agents_md,
        &loaded_agent_memory,
        skills,
        available_tools,
        tool_prompt_context,
    );
    let context_sections = built_context.sections;
    let rendered_system_prompt = render_sections(&system_sections);
    let rendered_context_attachment = render_sections(&context_sections);
    let context_fingerprint = prompt_context_fingerprint(
        session,
        execution,
        &system_sections,
        &context_sections,
        available_tools,
    );
    let cache_scope_fingerprint = prompt_cache_scope_fingerprint(
        session,
        execution,
        &system_sections,
        &context_sections,
        available_tools,
    );

    Ok(EffectivePrompt {
        agent_home: agent_home.to_path_buf(),
        identity: identity.clone(),
        execution: execution.clone(),
        loaded_agents_md,
        loaded_agent_memory,
        cache_identity: PromptCacheIdentity {
            agent_id: session.id.clone(),
            prompt_cache_key: prompt_cache_key(&session.id, &cache_scope_fingerprint),
            context_fingerprint,
            compression_epoch: session.working_memory.compression_epoch,
        },
        system_sections,
        context_sections,
        rendered_system_prompt,
        rendered_context_attachment,
        projection_owner: built_context_owner(session),
        projection_binding: session.current_execution_binding.as_ref().map(|binding| {
            ProjectionBindingSummary {
                source_message_id: binding.source_message_id.clone(),
                turn_id: binding.turn_id.clone(),
                owner: binding.owner.as_ref().map(ProjectionOwner::from),
                work_item_id: binding.work_item_id.clone(),
                claimed_work_revision: binding.claimed_work_revision,
            }
        }),
        projection_turn_id: current_message.turn_id.clone(),
        projection_evidence: built_context.projection_evidence,
        context_plan_evidence: built_context.plan_evidence,
        recent_turns_reprojection: built_context.recent_turns_reprojection,
        frozen_history_policy: built_context.history_policy,
        frozen_projection_budget: Some(built_context.projection_budget),
        frozen_recent_message_limit: Some(built_context.recent_message_limit),
        _resource_read_material: None,
    })
}

fn built_context_owner(session: &AgentState) -> ProjectionOwner {
    session
        .current_execution_binding
        .as_ref()
        .and_then(|binding| binding.owner.as_ref())
        .map(ProjectionOwner::from)
        .or_else(|| {
            session
                .current_execution_binding
                .as_ref()
                .and_then(|binding| binding.work_item_id.as_ref())
                .map(|work_item_id| ProjectionOwner::WorkItem {
                    work_item_id: work_item_id.clone(),
                })
        })
        .unwrap_or_else(|| ProjectionOwner::LegacyUnbound {
            agent_id: session.id.clone(),
        })
}

pub fn context_sections_from_built_context(built: BuiltContext) -> Vec<PromptSection> {
    built.sections
}

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod tests {
    use crate::types::{
        AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
        AgentVisibility, AgentsMdKind, AgentsMdSource, AuthorityClass, MessageBody, MessageKind,
        MessageOrigin, Priority,
    };

    use super::*;

    fn sample_message() -> MessageEnvelope {
        MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "hello".into(),
            },
        )
    }

    fn sample_identity() -> AgentIdentityView {
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

    fn sample_cache_identity() -> PromptCacheIdentity {
        PromptCacheIdentity {
            agent_id: "default".into(),
            prompt_cache_key: "default".into(),
            context_fingerprint: "fingerprint-default".into(),
            compression_epoch: 1,
        }
    }

    fn sample_execution_snapshot() -> ExecutionSnapshot {
        ExecutionSnapshot {
            profile: crate::system::ExecutionProfile::default(),
            policy: crate::system::ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: vec![],
            workspace_id: Some("workspace-1".into()),
            workspace_anchor: PathBuf::from("/repo"),
            execution_root: PathBuf::from("/repo"),
            cwd: PathBuf::from("/repo/src"),
            execution_root_id: Some("canonical_root:workspace-1".into()),
            projection_kind: Some(crate::system::types::WorkspaceProjectionKind::CanonicalRoot),
            access_mode: Some(crate::system::types::WorkspaceAccessMode::SharedRead),
            worktree_root: None,
            execution_roots: Vec::new(),
        }
    }

    #[test]
    fn real_loader_guidance_is_injected_once_only_for_same_file() {
        let agent = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let marker = "unique-agent-workspace-guidance-3889";
        for root in [agent.path(), workspace.path()] {
            std::fs::write(root.join("AGENTS.md"), marker).unwrap();
        }

        for (root, expected_count) in [(agent.path(), 1), (workspace.path(), 2)] {
            let loaded = crate::agents_md::load_agents_md(None, agent.path(), Some(root)).unwrap();
            let sections = build_system_sections(
                &sample_identity(),
                &sample_message(),
                agent.path(),
                &loaded,
                &LoadedAgentMemory::default(),
                &SkillsRuntimeView::default(),
                &[],
                ToolPromptContext::default(),
            );
            let system = render_sections(&sections);
            assert_eq!(system.matches(marker).count(), expected_count);
            assert!(sections
                .iter()
                .any(|section| section.name == "agent_agents_md"));
            assert_eq!(
                sections
                    .iter()
                    .any(|section| section.name == "workspace_agents_md"),
                expected_count == 2
            );
            if expected_count == 1 {
                assert_eq!(
                    describe_agents_md_source(None, loaded.workspace_status),
                    "duplicate of agent scope (guidance included there)"
                );
            }
        }
    }

    #[test]
    fn prompt_dump_includes_topology_and_ids() {
        let prompt = EffectivePrompt {
            identity: sample_identity(),
            agent_home: PathBuf::from("/tmp/agent-home"),
            execution: ExecutionSnapshot {
                profile: crate::system::ExecutionProfile::default(),
                policy: crate::system::ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: vec![],
                workspace_id: Some("workspace-1".into()),
                workspace_anchor: PathBuf::from("/repo"),
                execution_root: PathBuf::from("/repo"),
                cwd: PathBuf::from("/repo/src"),
                execution_root_id: Some("canonical_root:workspace-1".into()),
                projection_kind: Some(crate::system::types::WorkspaceProjectionKind::CanonicalRoot),
                access_mode: Some(crate::system::types::WorkspaceAccessMode::SharedRead),
                worktree_root: None,
                execution_roots: Vec::new(),
            },
            loaded_agents_md: LoadedAgentsMd {
                user_global_source: None,
                agent_source: Some(AgentsMdSource {
                    scope: crate::types::AgentsMdScope::Agent,
                    kind: AgentsMdKind::AgentsMd,
                    path: PathBuf::from("/tmp/agent-home/AGENTS.md"),
                    content: "agent guidance".into(),
                }),
                workspace_source: Some(AgentsMdSource {
                    scope: crate::types::AgentsMdScope::Workspace,
                    kind: AgentsMdKind::ClaudeMdFallback,
                    path: PathBuf::from("/repo/CLAUDE.md"),
                    content: "workspace guidance".into(),
                }),
                ..LoadedAgentsMd::default()
            }
            .into(),
            loaded_agent_memory: LoadedAgentMemory::default().into(),
            cache_identity: sample_cache_identity(),
            system_sections: vec![PromptSection {
                name: "test_section".to_string(),
                id: "test-stable-id-123".to_string(),
                content: "Test content here".to_string(),
                stability: PromptStability::Stable,
                layout: PromptLayout::SharedPrefix,
            }],
            context_sections: vec![PromptSection {
                name: "context_section".to_string(),
                id: "ctx-id-456".to_string(),
                content: "Context content".to_string(),
                stability: PromptStability::AgentScoped,
                layout: PromptLayout::ScopedSuffix,
            }],
            rendered_system_prompt: "rendered system".to_string(),
            rendered_context_attachment: "rendered context".to_string(),
            projection_owner: ProjectionOwner::AgentLifecycle {
                agent_id: "default".into(),
            },
            projection_binding: None,
            projection_turn_id: None,
            projection_evidence: ProjectionEvidenceIndex::new(),
            context_plan_evidence: ContextPlanEvidence {
                total_budget_estimated_tokens: 100,
                allocated_estimated_tokens: 20,
                decisions: vec![crate::context::ContextPlanDecision {
                    candidate_id: "context_section".to_string(),
                    section_name: "context_section".to_string(),
                    requested_estimated_tokens: 40,
                    minimum_estimated_tokens: 10,
                    allocated_estimated_tokens: 20,
                    outcome: crate::context::ContextPlanOutcome::Truncated,
                    reason: crate::context::ContextPlanReason::TruncatedToRemainingBudget,
                }],
            },
            recent_turns_reprojection: None,
            frozen_history_policy: ContextHistoryPreparation::default(),
            frozen_projection_budget: None,
            frozen_recent_message_limit: None,
            _resource_read_material: None,
        };

        let dump = prompt.render_dump();

        assert!(dump.contains("## Prompt Topology"));
        assert!(dump.contains("Agent home: /tmp/agent-home"));
        assert!(dump.contains("Workspace anchor: /repo"));
        assert!(dump.contains("Execution root: /repo"));
        assert!(dump.contains("Cwd: /repo/src"));
        assert!(dump.contains("User-global AGENTS.md: not evaluated"));
        assert!(dump.contains("Agent AGENTS.md: /tmp/agent-home/AGENTS.md (AGENTS.md)"));
        assert!(dump.contains("Workspace AGENTS.md: /repo/CLAUDE.md (CLAUDE.md fallback)"));
        assert!(dump.contains("Section inventory:"));
        assert!(dump.contains("  - system: total=1, stable=1, agent_scoped=0, turn_scoped=0"));
        assert!(dump.contains("  - context: total=1, stable=0, agent_scoped=1, turn_scoped=0"));
        assert!(dump.contains("Rendered system chars: 15"));
        assert!(dump.contains("Rendered context chars: 16"));
        assert!(dump.contains(
            "#1 [test_section] (id: test-stable-id-123, stability: Stable, cache scope: included"
        ));
        assert!(dump.contains(
            "#1 [context_section] (id: ctx-id-456, stability: AgentScoped, cache scope: included"
        ));
        assert!(dump.contains("Context budget plan:"));
        assert!(dump.contains("total estimated tokens: 100"));
        assert!(dump.contains(
            "[context_section] section=context_section outcome=truncated reason=truncated_to_remaining_budget requested=40 minimum=10 allocated=20"
        ));
        assert!(dump.contains("Shared system prefix: sections=1, chars="));
        assert!(dump.contains("[test_section][id: test-stable-id-123][Stable][SharedPrefix]"));
        assert!(dump.contains("[context_section][id: ctx-id-456][AgentScoped][ScopedSuffix]"));
    }

    #[test]
    fn media_defaults_keep_route_identifiers_out_of_system_and_cache_scope() {
        let prompt = EffectivePrompt {
            identity: sample_identity(),
            agent_home: PathBuf::from("/tmp/agent-home"),
            execution: sample_execution_snapshot(),
            loaded_agents_md: LoadedAgentsMd::default().into(),
            loaded_agent_memory: LoadedAgentMemory::default().into(),
            cache_identity: sample_cache_identity(),
            system_sections: Vec::new(),
            context_sections: Vec::new(),
            rendered_system_prompt: String::new(),
            rendered_context_attachment: String::new(),
            projection_owner: ProjectionOwner::AgentLifecycle {
                agent_id: "default".into(),
            },
            projection_binding: None,
            projection_turn_id: None,
            projection_evidence: ProjectionEvidenceIndex::new(),
            context_plan_evidence: ContextPlanEvidence::default(),
            recent_turns_reprojection: None,
            frozen_history_policy: ContextHistoryPreparation::default(),
            frozen_projection_budget: None,
            frozen_recent_message_limit: None,
            _resource_read_material: None,
        };
        let defaults = crate::provider::media::MediaToolDefaults {
            view_image: Some("test@default/vision-a".into()),
            generate_image: Some("test@default/image-a".into()),
            output_format: "png".into(),
        };
        assert!(prompt
            .clone()
            .with_media_tool_defaults(&defaults, &[])
            .system_sections
            .is_empty());
        let media_tools = crate::tool::ToolRegistry::new(".".into())
            .tool_specs()
            .unwrap()
            .into_iter()
            .filter(|tool| {
                tool.name == crate::tool::names::VIEW_IMAGE
                    || tool.name == crate::tool::names::GENERATE_IMAGE
            })
            .collect::<Vec<_>>();
        assert_eq!(media_tools.len(), 2);
        for visible_tools in [
            vec![crate::tool::names::VIEW_IMAGE],
            vec![crate::tool::names::GENERATE_IMAGE],
            vec![
                crate::tool::names::VIEW_IMAGE,
                crate::tool::names::GENERATE_IMAGE,
            ],
        ] {
            let tools = media_tools
                .iter()
                .filter(|tool| visible_tools.contains(&tool.name.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let first = prompt.clone().with_media_tool_defaults(&defaults, &tools);
            assert!(!first.rendered_system_prompt.contains("vision-a"));
            assert!(!first.rendered_system_prompt.contains("image-a"));
            assert!(first.context_sections.is_empty());
            assert!(first
                .rendered_system_prompt
                .contains("omission preserves configured/default routing"));
            assert!(first
                .rendered_system_prompt
                .contains("Exact overrides do not silently fall back"));
            assert!(first
                .rendered_system_prompt
                .contains("ListModelProviders/ListProviderModels"));
            assert!(!first
                .rendered_system_prompt
                .contains("default candidate: available"));
            assert!(!first
                .rendered_system_prompt
                .contains("default candidate: unavailable"));
            if visible_tools.contains(&crate::tool::names::GENERATE_IMAGE) {
                assert!(first
                    .rendered_system_prompt
                    .contains("final output_format defaults to PNG"));
            }
            let renamed = crate::provider::media::MediaToolDefaults {
                view_image: Some("test@plan/vision-b".into()),
                generate_image: Some("test@plan/image-b".into()),
                ..defaults.clone()
            };
            let second = first.clone().with_media_tool_defaults(&renamed, &tools);
            assert_eq!(second.system_sections.len(), 1);
            assert_eq!(first.rendered_system_prompt, second.rendered_system_prompt);
            assert_eq!(
                first.cache_identity.prompt_cache_key,
                second.cache_identity.prompt_cache_key
            );
            assert_eq!(
                first.cache_identity.context_fingerprint,
                second.cache_identity.context_fingerprint
            );
            let unavailable = crate::provider::media::MediaToolDefaults::default();
            let third = second
                .clone()
                .with_media_tool_defaults(&unavailable, &tools);
            assert_eq!(third.system_sections.len(), 1);
            assert_eq!(second.rendered_system_prompt, third.rendered_system_prompt);
            assert_eq!(
                second.cache_identity.prompt_cache_key,
                third.cache_identity.prompt_cache_key
            );
            assert_eq!(
                second.cache_identity.context_fingerprint,
                third.cache_identity.context_fingerprint
            );
            let repeated = third.clone().with_media_tool_defaults(&unavailable, &tools);
            assert_eq!(
                third.cache_identity.context_fingerprint,
                repeated.cache_identity.context_fingerprint
            );
            assert_eq!(
                third.cache_identity.prompt_cache_key,
                repeated.cache_identity.prompt_cache_key
            );
        }
        let mut canonical = prompt;
        canonical.system_sections = build_system_sections(
            &sample_identity(),
            &sample_message(),
            Path::new("/repo"),
            &LoadedAgentsMd::default(),
            &LoadedAgentMemory::default(),
            &SkillsRuntimeView::default(),
            &media_tools,
            ToolPromptContext::default(),
        );
        let prefix = shared_prefix_fingerprint(&canonical.system_sections);
        let updated = canonical.with_media_tool_defaults(&defaults, &media_tools);
        assert_eq!(
            updated.system_sections.last().unwrap().name,
            "response_language"
        );
        assert_eq!(prefix, shared_prefix_fingerprint(&updated.system_sections));
        let media = updated
            .system_sections
            .iter()
            .find(|section| section.name == "media_tool_defaults")
            .unwrap();
        assert_eq!(media.layout, PromptLayout::ScopedSuffix);
        assert_eq!(
            updated.rendered_system_prompt,
            render_sections(&updated.system_sections)
        );
    }

    #[test]
    fn history_reprojection_budget_uses_only_remaining_context_budget() {
        let mut prompt = EffectivePrompt {
            identity: sample_identity(),
            agent_home: PathBuf::from("/tmp/agent-home"),
            execution: sample_execution_snapshot(),
            loaded_agents_md: LoadedAgentsMd::default().into(),
            loaded_agent_memory: LoadedAgentMemory::default().into(),
            cache_identity: sample_cache_identity(),
            system_sections: Vec::new(),
            context_sections: Vec::new(),
            rendered_system_prompt: String::new(),
            rendered_context_attachment: String::new(),
            projection_owner: ProjectionOwner::AgentLifecycle {
                agent_id: "default".into(),
            },
            projection_binding: None,
            projection_turn_id: None,
            projection_evidence: ProjectionEvidenceIndex::new(),
            context_plan_evidence: ContextPlanEvidence {
                total_budget_estimated_tokens: 100,
                allocated_estimated_tokens: 90,
                decisions: vec![
                    crate::context::ContextPlanDecision {
                        candidate_id: "current_input".into(),
                        section_name: "current_input".into(),
                        requested_estimated_tokens: 70,
                        minimum_estimated_tokens: 70,
                        allocated_estimated_tokens: 70,
                        outcome: crate::context::ContextPlanOutcome::Full,
                        reason: crate::context::ContextPlanReason::SelectedFull,
                    },
                    crate::context::ContextPlanDecision {
                        candidate_id: "recent_turns".into(),
                        section_name: "recent_turns".into(),
                        requested_estimated_tokens: 20,
                        minimum_estimated_tokens: 0,
                        allocated_estimated_tokens: 20,
                        outcome: crate::context::ContextPlanOutcome::Full,
                        reason: crate::context::ContextPlanReason::SelectedFull,
                    },
                ],
            },
            recent_turns_reprojection: None,
            frozen_history_policy: ContextHistoryPreparation::default(),
            frozen_projection_budget: None,
            frozen_recent_message_limit: None,
            _resource_read_material: None,
        };

        assert_eq!(prompt.history_reprojection_budget(64), 30);
        prompt.context_plan_evidence.total_budget_estimated_tokens = 20;
        assert_eq!(prompt.history_reprojection_budget(64), 0);
    }

    #[test]
    fn prompt_dump_marks_turn_scoped_sections_as_turn_only() {
        let prompt = EffectivePrompt {
            identity: sample_identity(),
            agent_home: PathBuf::from("/tmp/agent-home"),
            execution: sample_execution_snapshot(),
            loaded_agents_md: LoadedAgentsMd::default().into(),
            loaded_agent_memory: LoadedAgentMemory::default().into(),
            cache_identity: sample_cache_identity(),
            system_sections: vec![PromptSection {
                name: "constrained_repair".to_string(),
                id: "constrained_repair".to_string(),
                content: "limit edits to src/prompt/mod.rs".to_string(),
                stability: PromptStability::TurnScoped,
                layout: PromptLayout::ScopedSuffix,
            }],
            context_sections: vec![PromptSection {
                name: "current_input".to_string(),
                id: "current_input".to_string(),
                content: "inspect prompt assembly".to_string(),
                stability: PromptStability::TurnScoped,
                layout: PromptLayout::ScopedSuffix,
            }],
            rendered_system_prompt: "rendered turn system".to_string(),
            rendered_context_attachment: "rendered turn context".to_string(),
            projection_owner: ProjectionOwner::AgentLifecycle {
                agent_id: "default".into(),
            },
            projection_binding: None,
            projection_turn_id: None,
            projection_evidence: ProjectionEvidenceIndex::new(),
            context_plan_evidence: ContextPlanEvidence::default(),
            recent_turns_reprojection: None,
            frozen_history_policy: ContextHistoryPreparation::default(),
            frozen_projection_budget: None,
            frozen_recent_message_limit: None,
            _resource_read_material: None,
        };

        let dump = prompt.render_dump();

        assert!(dump.contains("  - system: total=1, stable=0, agent_scoped=0, turn_scoped=1"));
        assert!(dump.contains("  - context: total=1, stable=0, agent_scoped=0, turn_scoped=1"));
        assert!(dump.contains("Shared system prefix: sections=0, chars=0, fingerprint="));
        assert!(dump.contains(
            "#1 [constrained_repair] (id: constrained_repair, stability: TurnScoped, cache scope: turn-only"
        ));
        assert!(dump.contains(
            "#1 [current_input] (id: current_input, stability: TurnScoped, cache scope: turn-only"
        ));
    }

    #[test]
    fn prompt_dump_includes_agents_md_content_for_debugging() {
        let prompt = EffectivePrompt {
            identity: sample_identity(),
            agent_home: PathBuf::from("/tmp/agent-home"),
            execution: ExecutionSnapshot {
                profile: crate::system::ExecutionProfile::default(),
                policy: crate::system::ExecutionProfile::default().policy_snapshot(),
                attached_workspaces: vec![],
                workspace_id: Some("workspace-1".into()),
                workspace_anchor: PathBuf::from("/repo"),
                execution_root: PathBuf::from("/repo"),
                cwd: PathBuf::from("/repo"),
                execution_root_id: Some("canonical_root:workspace-1".into()),
                projection_kind: Some(crate::system::types::WorkspaceProjectionKind::CanonicalRoot),
                access_mode: Some(crate::system::types::WorkspaceAccessMode::SharedRead),
                worktree_root: None,
                execution_roots: Vec::new(),
            },
            loaded_agents_md: LoadedAgentsMd {
                user_global_source: None,
                agent_source: Some(AgentsMdSource {
                    scope: crate::types::AgentsMdScope::Agent,
                    kind: AgentsMdKind::AgentsMd,
                    path: PathBuf::from("/tmp/agent-home/AGENTS.md"),
                    content: "very secret agent guidance".into(),
                }),
                workspace_source: None,
                ..LoadedAgentsMd::default()
            }
            .into(),
            loaded_agent_memory: LoadedAgentMemory::default().into(),
            cache_identity: sample_cache_identity(),
            system_sections: vec![PromptSection {
                name: "agent_agents_md".to_string(),
                id: "agent_agents_md".to_string(),
                content: "very secret agent guidance".to_string(),
                stability: PromptStability::Stable,
                layout: PromptLayout::ScopedSuffix,
            }],
            context_sections: vec![],
            rendered_system_prompt: "very secret agent guidance".to_string(),
            rendered_context_attachment: String::new(),
            projection_owner: ProjectionOwner::AgentLifecycle {
                agent_id: "default".into(),
            },
            projection_binding: None,
            projection_turn_id: None,
            projection_evidence: ProjectionEvidenceIndex::new(),
            context_plan_evidence: ContextPlanEvidence::default(),
            recent_turns_reprojection: None,
            frozen_history_policy: ContextHistoryPreparation::default(),
            frozen_projection_budget: None,
            frozen_recent_message_limit: None,
            _resource_read_material: None,
        };

        let dump = prompt.render_dump();

        assert!(dump.contains("very secret agent guidance"));
    }
}
