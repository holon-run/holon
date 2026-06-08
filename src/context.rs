use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::Value;

use crate::{
    prompt::{PromptSection, PromptStability},
    storage::AppStorage,
    system::{execution_policy_summary_lines, ExecutionSnapshot},
    tool::helpers::truncate_text,
    types::{
        AdmissionContext, AgentState, AuthorityClass, BriefRecord, CallbackDeliveryMode,
        ContextEpisodeRecord, ContinuationClass, ContinuationResolution, ContinuationTriggerKind,
        ExternalTriggerRecord, ExternalTriggerScope, ExternalTriggerStatus, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, SkillsRuntimeView,
        TodoItemState, ToolExecutionRecord, TranscriptEntry, TranscriptEntryKind, TurnRecord,
        WaitingIntentRecord, WaitingIntentScope, WaitingIntentStatus, WorkItemRecord,
        WorkingMemoryDelta, WorkingMemorySnapshot,
    },
};

#[derive(Debug, Clone)]
pub struct ContextConfig {
    pub recent_messages: usize,
    pub recent_briefs: usize,
    pub compaction_trigger_messages: usize,
    pub compaction_keep_recent_messages: usize,
    pub prompt_budget_estimated_tokens: usize,
    pub compaction_trigger_estimated_tokens: usize,
    pub compaction_keep_recent_estimated_tokens: usize,
    pub recent_episode_candidates: usize,
    pub max_relevant_episodes: usize,
    pub turn_projection_budget_ratio: f32,
    pub turn_projection_min_budget: usize,
    pub turn_projection_max_budget: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            recent_messages: 12,
            recent_briefs: 8,
            compaction_trigger_messages: 20,
            compaction_keep_recent_messages: 8,
            prompt_budget_estimated_tokens: 4096,
            compaction_trigger_estimated_tokens: 2048,
            compaction_keep_recent_estimated_tokens: 768,
            recent_episode_candidates: 12,
            max_relevant_episodes: 3,
            turn_projection_budget_ratio: 0.30,
            turn_projection_min_budget: 4096,
            turn_projection_max_budget: 64000,
        }
    }
}
impl ContextConfig {
    /// Derive turn projection budget from resolved prompt budget.
    /// Applies ratio with floor and ceiling guards.
    pub fn turn_projection_budget(&self) -> usize {
        let budget = (self.prompt_budget_estimated_tokens as f32
            * self.turn_projection_budget_ratio) as usize;
        budget.clamp(
            self.turn_projection_min_budget,
            self.turn_projection_max_budget,
        )
    }
}

#[derive(Debug, Clone)]
pub struct BuiltContext {
    pub sections: Vec<PromptSection>,
}

pub fn build_context(
    storage: &AppStorage,
    agent: &AgentState,
    execution: &ExecutionSnapshot,
    skills: &SkillsRuntimeView,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
    config: &ContextConfig,
) -> Result<BuiltContext> {
    build_context_with_default_external_ingress(
        storage,
        agent,
        execution,
        skills,
        current_message,
        continuation,
        config,
        None,
    )
}

pub fn build_context_with_default_external_ingress(
    storage: &AppStorage,
    agent: &AgentState,
    execution: &ExecutionSnapshot,
    skills: &SkillsRuntimeView,
    current_message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
    config: &ContextConfig,
    default_external_ingress_override: Option<&ExternalTriggerRecord>,
) -> Result<BuiltContext> {
    let mut messages =
        storage.read_messages_from(agent.compacted_message_count, config.recent_messages)?;
    let continuation_anchor_messages = storage.read_all_messages()?;
    let mut briefs = storage.read_recent_briefs(config.recent_briefs)?;
    let mut tools = storage.read_recent_tool_executions(config.recent_messages)?;
    let mut turn_records = storage.read_recent_turns(config.recent_messages)?;
    if turn_records.is_empty() {
        turn_records = synthesize_legacy_recent_turn_records(
            &messages,
            &briefs,
            &tools,
            config.recent_messages,
        );
    }
    hydrate_recent_turn_references(
        storage,
        &turn_records,
        &mut messages,
        &mut briefs,
        &mut tools,
    )?;
    let transcript = storage.read_recent_transcript(config.recent_messages)?;
    let active_waiting_intents = storage
        .latest_waiting_intents()?
        .into_iter()
        .filter(|intent| intent.agent_id == agent.id)
        .filter(|intent| intent.scope == WaitingIntentScope::WorkItem)
        .filter(|intent| intent.status == WaitingIntentStatus::Active)
        .collect::<Vec<_>>();
    let episodes = storage.read_recent_context_episodes(config.recent_episode_candidates)?;
    let work_queue_projection = storage.work_queue_prompt_projection()?;
    let current_work_item = work_queue_projection.current.as_ref();

    let current_input_reserved_budget =
        reserve_current_input_budget(config.prompt_budget_estimated_tokens);
    let mut sections = Vec::new();
    let mut remaining_budget = config
        .prompt_budget_estimated_tokens
        .saturating_sub(current_input_reserved_budget);
    push_budgeted_section(
        &mut sections,
        &mut remaining_budget,
        section("agent", format!("Agent id: {}", agent.id)),
    );
    let default_ingress = match default_external_ingress_override {
        Some(record) => Some(record.clone()),
        None => default_external_ingress(storage, &agent.id)?,
    };
    if let Some(default_ingress) = default_ingress {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            section(
                "default_external_ingress",
                render_default_external_ingress(&default_ingress),
            ),
        );
    }
    let execution_summary = execution_policy_summary_lines(execution).join("\n");
    push_budgeted_section(
        &mut sections,
        &mut remaining_budget,
        section(
            "execution_environment",
            format!(
                "Execution environment summary (policy snapshot; host-local is not a strong sandbox guarantee):\n\
                 {}",
                execution_summary,
            ),
        ),
    );

    if !working_memory_is_empty(&agent.working_memory.current_working_memory) {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            section(
                "working_memory",
                render_working_memory(&agent.working_memory.current_working_memory),
            ),
        );
    } else if let Some(summary) = &agent.context_summary {
        if !summary.trim().is_empty() {
            push_budgeted_section(
                &mut sections,
                &mut remaining_budget,
                section(
                    "compacted_summary",
                    format!("Compacted agent summary:\n{summary}"),
                ),
            );
        }
    }

    if let Some(delta) = &agent.working_memory.pending_working_memory_delta {
        if let Some(content) = render_working_memory_delta_with_budget(delta, remaining_budget) {
            push_budgeted_section(
                &mut sections,
                &mut remaining_budget,
                turn_section("working_memory_delta", content),
            );
        }
    }

    if let Some(work_item) = current_work_item {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section(
                "current_work_item",
                render_current_work_item(work_item, storage.data_dir(), &active_waiting_intents),
            ),
        );
    }

    let recent_turn_window_start = recent_turn_window_start(&turn_records);
    if let Some(section) = build_relevant_episode_memory_section(
        &episodes,
        agent,
        current_work_item,
        current_message,
        config,
        remaining_budget,
        recent_turn_window_start,
    ) {
        push_budgeted_section(&mut sections, &mut remaining_budget, section);
    }

    if let Some(work_item) = current_work_item {
        if let Some(content) =
            render_current_work_item_process_trace(work_item, &briefs, &tools, &transcript)
        {
            push_budgeted_section(
                &mut sections,
                &mut remaining_budget,
                turn_section("current_work_item_process_trace", content),
            );
        }
    }

    if work_queue_projection.has_non_current_candidates() {
        let candidates = render_work_item_candidates(
            &work_queue_projection,
            storage,
            &agent.id,
            storage.data_dir(),
        )?;
        if let Some(content) = candidates {
            push_budgeted_section(
                &mut sections,
                &mut remaining_budget,
                turn_section("queued_blocked_work_items", content),
            );
        }
    }

    if let Some(worktree) = &agent.worktree_session {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            section(
                "worktree_session",
                format!(
                    "Managed worktree active:\n\
                     - Original working directory: {}\n\
                     - Original branch: {}\n\
                     - Worktree path: {}\n\
                     - Worktree branch: {}",
                    worktree.original_cwd.display(),
                    worktree.original_branch,
                    worktree.worktree_path.display(),
                    worktree.worktree_branch
                ),
            ),
        );
    }

    if !skills.agent_templates_catalog.is_empty() {
        let rendered = skills
            .agent_templates_catalog
            .iter()
            .map(|entry| {
                let skills = if entry.included_skills.is_empty() {
                    "skills=none".to_string()
                } else {
                    format!("skills={}", entry.included_skills.join(","))
                };
                let path = entry
                    .path
                    .as_ref()
                    .map(|path| format!(" path={}", path.display()))
                    .unwrap_or_default();
                format!(
                    "- [{}] {} template={} :: {}{} ({skills})",
                    entry.source.label(),
                    entry.catalog_id,
                    entry.template,
                    entry.description,
                    path
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            section(
                "agent_templates_catalog",
                format!("Discovered agent templates catalog:\n{rendered}"),
            ),
        );
    }

    if !skills.discoverable_skills.is_empty() {
        let rendered = skills
            .discoverable_skills
            .iter()
            .map(|skill| {
                format!(
                    "- [{}] {} :: {} ({})",
                    scope_label(&skill.scope),
                    skill.name,
                    skill.path.display(),
                    skill.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            section(
                "skills_catalog",
                format!("Discovered skills catalog:\n{rendered}"),
            ),
        );
    }

    if !skills.active_skills.is_empty() {
        let rendered = skills
            .active_skills
            .iter()
            .map(|skill| {
                format!(
                    "- [{}][{}][{}] {} :: {}",
                    scope_label(&skill.scope),
                    activation_source_label(skill.activation_source),
                    activation_state_label(skill.activation_state),
                    skill.skill_id,
                    skill.path.display()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            section("active_skills", format!("Active skills:\n{rendered}")),
        );
    }

    push_budgeted_section(
        &mut sections,
        &mut remaining_budget,
        section(
            "context_contract",
            "Interpret the memory block with this priority: current work item objective first, durable plan artifact second, todo_list third, working memory delta next, and rolling working memory after that. This is an interpretation priority, not a guarantee about section ordering. Use prior briefs and recent tool results as continuity evidence across turns. When sources differ on task scope, treat the current work item's `objective` and plan artifact as the ground truth unless the current input explicitly changes it."
                .to_string(),
        ),
    );

    if let Some(anchor) = render_continuation_anchor(
        &continuation_anchor_messages,
        current_message,
        current_work_item,
        remaining_budget,
    ) {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section("continuation_anchor", anchor),
        );
    }

    if let Some(content) = render_recent_turns_with_budget(
        &turn_records,
        &messages,
        &briefs,
        &tools,
        current_message,
        current_work_item,
        remaining_budget,
    ) {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section("recent_turns", content),
        );
    }

    let continuation_pushed =
        if let Some(content) = continuation_context(current_message, continuation) {
            push_budgeted_section(
                &mut sections,
                &mut remaining_budget,
                turn_section("continuation_context", content),
            )
        } else {
            false
        };

    let mut current_input_budget = current_input_reserved_budget.saturating_add(remaining_budget);
    let current_input_body = render_current_input_body_with_budget(
        &current_message.body,
        current_input_budget,
        (!continuation_pushed)
            .then(|| render_wake_hint_context(current_message))
            .flatten()
            .as_deref(),
    );
    push_budgeted_section(
        &mut sections,
        &mut current_input_budget,
        turn_section(
            "current_input",
            format!(
                "Current input:\n- {}\n{}",
                message_header(current_message),
                indent_block(&current_input_body, 2),
            ),
        ),
    );

    Ok(BuiltContext { sections })
}

fn hydrate_recent_turn_references(
    storage: &AppStorage,
    turn_records: &[TurnRecord],
    messages: &mut Vec<MessageEnvelope>,
    briefs: &mut Vec<BriefRecord>,
    tools: &mut Vec<ToolExecutionRecord>,
) -> Result<()> {
    let mut message_ids = messages
        .iter()
        .map(|message| message.id.clone())
        .collect::<BTreeSet<_>>();
    let mut brief_ids = briefs
        .iter()
        .map(|brief| brief.id.clone())
        .collect::<BTreeSet<_>>();
    let mut tool_ids = tools
        .iter()
        .map(|tool| tool.id.clone())
        .collect::<BTreeSet<_>>();

    for record in turn_records {
        for message_id in &record.input_message_ids {
            if message_ids.contains(message_id) {
                continue;
            }
            if let Some(message) = storage.read_message_by_id(message_id)? {
                message_ids.insert(message.id.clone());
                messages.push(message);
            } else {
                tracing::warn!(
                    turn_id = %record.turn_id,
                    message_id = %message_id,
                    "recent_turns turn record references missing input message"
                );
            }
        }

        if let Some(message_id) = record
            .trigger
            .as_ref()
            .and_then(|trigger| trigger.message_id.as_ref())
        {
            if !message_ids.contains(message_id) {
                if let Some(message) = storage.read_message_by_id(message_id)? {
                    message_ids.insert(message.id.clone());
                    messages.push(message);
                } else {
                    tracing::warn!(
                        turn_id = %record.turn_id,
                        message_id = %message_id,
                        "recent_turns turn record references missing trigger message"
                    );
                }
            }
        }

        for brief_id in &record.produced_brief_ids {
            if brief_ids.contains(brief_id) {
                continue;
            }
            if let Some(brief) = storage.read_brief_by_id(brief_id)? {
                brief_ids.insert(brief.id.clone());
                briefs.push(brief);
            } else {
                tracing::warn!(
                    turn_id = %record.turn_id,
                    brief_id = %brief_id,
                    "recent_turns turn record references missing brief"
                );
            }
        }

        for tool_id in &record.tool_execution_ids {
            if tool_ids.contains(tool_id) {
                continue;
            }
            if let Some(tool) = storage.read_tool_execution_by_id(tool_id)? {
                tool_ids.insert(tool.id.clone());
                tools.push(tool);
            } else {
                tracing::warn!(
                    turn_id = %record.turn_id,
                    tool_id = %tool_id,
                    "recent_turns turn record references missing tool execution"
                );
            }
        }
    }

    messages.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    briefs.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    tools.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });

    Ok(())
}

fn synthesize_legacy_recent_turn_records(
    messages: &[MessageEnvelope],
    briefs: &[BriefRecord],
    tools: &[ToolExecutionRecord],
    limit: usize,
) -> Vec<TurnRecord> {
    let mut records = messages
        .iter()
        .filter(|message| !matches!(message.kind, MessageKind::SystemTick))
        .filter_map(|message| {
            let message_turn_id = message.turn_id.as_deref().map(str::trim);
            let message_seq = message.message_seq.filter(|seq| *seq != 0);
            let produced_brief_ids = briefs
                .iter()
                .filter(|brief| {
                    legacy_brief_matches_message(brief, message, message_turn_id, message_seq)
                })
                .map(|brief| brief.id.clone())
                .collect::<Vec<_>>();

            let tool_execution_ids = tools
                .iter()
                .filter(|tool| legacy_tool_matches_message(tool, message_turn_id, message_seq))
                .map(|tool| tool.id.clone())
                .collect::<Vec<_>>();

            if produced_brief_ids.is_empty() && tool_execution_ids.is_empty() {
                return None;
            }

            let turn_index = message_seq.unwrap_or(0);
            let turn_id = message_seq
                .map(|seq| format!("legacy-message-seq-{seq}"))
                .or_else(|| {
                    message_turn_id
                        .filter(|turn_id| !turn_id.is_empty())
                        .map(|turn_id| format!("legacy-turn-id-{turn_id}"))
                })
                .unwrap_or_else(|| format!("legacy-message-{}", message.id));
            let mut record = TurnRecord::new(&message.agent_id, turn_id, turn_index);
            record.trigger = Some(crate::types::TurnTriggerSummary::from_message(message));
            record.input_message_ids = vec![message.id.clone()];
            record.produced_brief_ids = produced_brief_ids;
            record.tool_execution_ids = tool_execution_ids;
            record.created_at = message.created_at;
            Some(record)
        })
        .collect::<Vec<_>>();

    if records.len() > limit {
        records = records.split_off(records.len() - limit);
    }
    records
}

fn legacy_brief_matches_message(
    brief: &BriefRecord,
    message: &MessageEnvelope,
    message_turn_id: Option<&str>,
    message_seq: Option<u64>,
) -> bool {
    brief.related_message_id.as_deref() == Some(message.id.as_str())
        || legacy_turn_identity_matches(
            brief.turn_id.as_deref(),
            brief.turn_index,
            message_turn_id,
            message_seq,
        )
}

fn legacy_tool_matches_message(
    tool: &ToolExecutionRecord,
    message_turn_id: Option<&str>,
    message_seq: Option<u64>,
) -> bool {
    legacy_turn_identity_matches(
        tool.turn_id.as_deref(),
        Some(tool.turn_index),
        message_turn_id,
        message_seq,
    )
}

fn legacy_turn_identity_matches(
    evidence_turn_id: Option<&str>,
    evidence_turn_index: Option<u64>,
    message_turn_id: Option<&str>,
    message_seq: Option<u64>,
) -> bool {
    evidence_turn_id
        .map(str::trim)
        .filter(|turn_id| !turn_id.is_empty())
        .zip(message_turn_id.filter(|turn_id| !turn_id.is_empty()))
        .is_some_and(|(evidence_turn_id, message_turn_id)| evidence_turn_id == message_turn_id)
        || evidence_turn_index
            .filter(|turn_index| *turn_index != 0)
            .zip(message_seq)
            .is_some_and(|(evidence_turn_index, message_seq)| evidence_turn_index == message_seq)
}

fn message_header(message: &MessageEnvelope) -> String {
    let mut labels = vec![origin_label(&message.origin).to_string()];
    if let Some(surface) = message.delivery_surface {
        labels.push(delivery_surface_label(surface).to_string());
    }
    if let Some(context) = message.admission_context {
        labels.push(admission_context_label(context).to_string());
    }
    if let Some(trigger_kind) = message.trigger_kind {
        labels.push(format!("trigger:{}", enum_label(&trigger_kind)));
    }
    if let Some(work_item_id) = message.work_item_id.as_deref() {
        labels.push(format!("work_item:{}", header_label_value(work_item_id)));
    }
    if let Some(task_id) = message.task_id.as_deref() {
        labels.push(format!("task:{}", header_label_value(task_id)));
    }
    labels.push(authority_class_label(message.authority_class).to_string());
    labels.push(kind_label(message));
    format!("[{}]", labels.join("]["))
}

fn header_label_value(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' | ':' | '/' => ch,
            _ => '_',
        })
        .collect()
}

fn default_external_ingress(
    storage: &AppStorage,
    agent_id: &str,
) -> Result<Option<ExternalTriggerRecord>> {
    Ok(storage
        .latest_external_triggers()?
        .into_iter()
        .filter(|record| {
            record.target_agent_id == agent_id
                && record.scope == ExternalTriggerScope::Agent
                && record.delivery_mode == CallbackDeliveryMode::WakeHint
                && record.status == ExternalTriggerStatus::Active
        })
        .max_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.external_trigger_id.cmp(&right.external_trigger_id))
        }))
}

fn render_default_external_ingress(record: &ExternalTriggerRecord) -> String {
    let url = record.trigger_url.as_deref().unwrap_or("<unavailable>");
    format!(
        "Default external ingress:\n\
         - url: {url}\n\
         - mode: wake_hint\n\
         - status: active\n\
         - external_trigger_id: {}\n\
         - capability_secret: true\n\
         - handling: Treat this URL as a capability secret. Do not repeat, store, or forward it unless the current task is explicitly configuring an external system to wake this agent.",
        record.external_trigger_id
    )
}

pub fn maybe_compact_agent(
    storage: &AppStorage,
    agent: &mut AgentState,
    config: &ContextConfig,
) -> Result<bool> {
    let all_messages = storage.read_all_messages()?;
    let previous_total_message_count = agent.total_message_count;
    agent.total_message_count = all_messages.len();
    let mut changed = agent.total_message_count != previous_total_message_count;
    let has_working_memory = !working_memory_is_empty(&agent.working_memory.current_working_memory);
    if has_working_memory && agent.context_summary.is_some() {
        agent.context_summary = None;
        changed = true;
    }
    let visible_messages = all_messages
        .iter()
        .filter(|message| include_in_prompt_context(message))
        .collect::<Vec<_>>();
    let visible_estimated_tokens = visible_messages
        .iter()
        .map(|message| estimate_message_tokens(message))
        .sum::<usize>();
    if visible_estimated_tokens <= config.compaction_trigger_estimated_tokens {
        return Ok(changed);
    }

    let mut split_at = all_messages.len();
    let mut kept_visible_messages = 0usize;
    let mut kept_estimated_tokens = 0usize;
    for (idx, message) in all_messages.iter().enumerate().rev() {
        if !include_in_prompt_context(message) {
            continue;
        }
        let message_tokens = estimate_message_tokens(message);
        if kept_visible_messages >= config.compaction_keep_recent_messages
            && kept_estimated_tokens + message_tokens
                > config.compaction_keep_recent_estimated_tokens
        {
            split_at = idx + 1;
            break;
        }
        kept_visible_messages += 1;
        kept_estimated_tokens += message_tokens;
        split_at = idx;
    }

    if split_at == 0 || split_at <= agent.compacted_message_count {
        return Ok(changed);
    }

    if has_working_memory {
        agent.context_summary = None;
    } else {
        let compacted_slice = &all_messages[..split_at];
        let summary = compacted_slice
            .iter()
            .filter(|message| include_in_prompt_context(message))
            .map(|message| {
                format!(
                    "- {} {}",
                    message_header(message),
                    body_preview(&message.body)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        agent.context_summary = Some(summary);
    }
    agent.compacted_message_count = split_at;
    if !has_working_memory {
        agent.working_memory.compression_epoch =
            agent.working_memory.compression_epoch.saturating_add(1);
    }
    Ok(true)
}

fn section(name: &'static str, content: String) -> PromptSection {
    PromptSection {
        name: name.to_string(),
        id: name.to_string(),
        content,
        stability: PromptStability::AgentScoped,
    }
}

fn turn_section(name: &'static str, content: String) -> PromptSection {
    PromptSection {
        name: name.to_string(),
        id: name.to_string(),
        content,
        stability: PromptStability::TurnScoped,
    }
}

fn push_budgeted_section(
    sections: &mut Vec<PromptSection>,
    remaining_budget: &mut usize,
    section: PromptSection,
) -> bool {
    let Some(section) = fit_section_to_budget(section, *remaining_budget) else {
        return false;
    };
    *remaining_budget = remaining_budget.saturating_sub(estimate_section_tokens(&section));
    sections.push(section);
    true
}

fn fit_section_to_budget(section: PromptSection, budget: usize) -> Option<PromptSection> {
    if budget == 0 {
        return None;
    }

    if estimate_section_tokens(&section) <= budget {
        return Some(section);
    }

    let section_header_budget = estimate_text_tokens(&format!("[{}]\n", section.name));
    if budget <= section_header_budget {
        return None;
    }

    let truncated_content = truncate_section_content(
        "",
        &section.content,
        budget.saturating_sub(section_header_budget),
        Some("\n[truncated for budget]"),
    );
    let fitted = PromptSection {
        content: truncated_content,
        ..section
    };
    if fitted.content.trim().is_empty() || estimate_section_tokens(&fitted) > budget {
        None
    } else {
        Some(fitted)
    }
}

fn render_message(message: &MessageEnvelope) -> String {
    format!(
        "- {} {}",
        message_header(message),
        body_preview(&message.body)
    )
}

fn render_current_work_item(
    work_item: &WorkItemRecord,
    agent_home: &std::path::Path,
    active_waiting_intents: &[WaitingIntentRecord],
) -> String {
    let mut lines = vec![
        "Current work item:".to_string(),
        format!("- Id: {}", work_item.id),
        format!("- State: {:?}", work_item.state),
        format!("- Readiness: {:?}", work_item.readiness()),
        format!("- Objective: {}", work_item.objective),
        format!(
            "- Plan status: {}",
            work_item_plan_status_label(work_item.plan_status)
        ),
    ];
    lines.extend(render_work_item_plan_artifact_lines(
        work_item, agent_home, "- ",
    ));
    if !work_item.todo_list.is_empty() {
        lines.push("- Todo list:".to_string());
        lines.extend(work_item.todo_list.iter().map(|item| {
            let state = match item.state {
                TodoItemState::Pending => "pending",
                TodoItemState::InProgress => "in_progress",
                TodoItemState::Completed => "completed",
            };
            format!("  - [{state}] {}", item.text)
        }));
    }
    if let Some(blocked_by) = work_item.blocked_by.as_deref() {
        lines.push(format!("- Blocked by: {blocked_by}"));
    }
    let waits = active_waiting_intents
        .iter()
        .filter(|intent| intent.work_item_id.as_deref() == Some(work_item.id.as_str()))
        .collect::<Vec<_>>();
    if !waits.is_empty() {
        lines.push("- Active waits:".to_string());
        lines.extend(waits.into_iter().take(4).map(|intent| {
            let triggered = intent
                .last_triggered_at
                .map(|at| format!(" :: last_triggered_at={at}"))
                .unwrap_or_default();
            let resource = intent
                .resource
                .as_deref()
                .map(|resource| format!(" :: resource={resource}"))
                .unwrap_or_default();
            format!("  - {}{resource}{triggered}", intent.description)
        }));
    }
    lines.join("\n")
}

fn work_item_plan_status_label(status: crate::types::WorkItemPlanStatus) -> &'static str {
    match status {
        crate::types::WorkItemPlanStatus::Draft => "draft",
        crate::types::WorkItemPlanStatus::Ready => "ready",
        crate::types::WorkItemPlanStatus::NeedsInput => "needs_input",
    }
}

fn render_current_work_item_process_trace(
    work_item: &WorkItemRecord,
    briefs: &[BriefRecord],
    tools: &[ToolExecutionRecord],
    transcript: &[TranscriptEntry],
) -> Option<String> {
    let mut lines = vec!["Recent current WorkItem process trace:".to_string()];
    let mut added = 0usize;
    for brief in briefs
        .iter()
        .rev()
        .filter(|brief| brief.agent_id == work_item.agent_id)
        .filter(|brief| brief.work_item_id.as_deref() == Some(work_item.id.as_str()))
        .take(3)
    {
        lines.push(format!(
            "- brief:{:?} {}",
            brief.kind,
            truncate_text(&brief.text.replace('\n', " "), 180)
        ));
        added += 1;
    }
    for tool in tools
        .iter()
        .rev()
        .filter(|tool| tool.agent_id == work_item.agent_id)
        .filter(|tool| tool.work_item_id.as_deref() == Some(work_item.id.as_str()))
        .take(4)
    {
        lines.push(format!(
            "- tool:{} {}",
            tool.tool_name,
            truncate_text(&tool.summary.replace('\n', " "), 180)
        ));
        added += 1;
    }
    for entry in transcript
        .iter()
        .rev()
        .filter(|entry| entry.agent_id == work_item.agent_id)
        .filter(|entry| entry.kind == TranscriptEntryKind::AssistantRound)
        .filter(|entry| entry.data["work_item_id"].as_str() == Some(work_item.id.as_str()))
        .take(2)
    {
        if let Some(text) = assistant_round_text_preview(entry) {
            lines.push(format!("- assistant_round: {}", truncate_text(&text, 180)));
            added += 1;
        }
    }
    (added > 0).then(|| lines.join("\n"))
}

fn assistant_round_text_preview(entry: &TranscriptEntry) -> Option<String> {
    let text = entry
        .data
        .get("blocks")?
        .as_array()?
        .iter()
        .filter_map(|block| {
            if let Some(kind) = block.get("type").and_then(serde_json::Value::as_str) {
                if kind != "text" {
                    return None;
                }
            }
            block
                .get("Text")
                .and_then(|value| value.get("text"))
                .or_else(|| block.get("text"))
                .and_then(serde_json::Value::as_str)
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

fn render_work_item_candidates(
    projection: &crate::storage::WorkQueuePromptProjection,
    storage: &AppStorage,
    agent_id: &str,
    agent_home: &std::path::Path,
) -> Result<Option<String>> {
    let completion_reports = latest_delivery_summary_text_by_work_item(storage, agent_id)?;
    let mut lines = vec!["Work item candidates by scheduler ranking:".to_string()];
    append_candidate_group(
        &mut lines,
        "Triggered work items:",
        &projection.triggered_blocked,
        &completion_reports,
        agent_home,
    )?;
    append_candidate_group(
        &mut lines,
        "Queued runnable work items:",
        &projection.queued_runnable,
        &completion_reports,
        agent_home,
    )?;
    append_candidate_group(
        &mut lines,
        "Waiting for operator:",
        &projection.waiting_for_operator,
        &completion_reports,
        agent_home,
    )?;
    append_candidate_group(
        &mut lines,
        "Blocked work items:",
        &projection.blocked,
        &completion_reports,
        agent_home,
    )?;
    append_candidate_group(
        &mut lines,
        "Recently completed work items:",
        &projection.completed_recent,
        &completion_reports,
        agent_home,
    )?;
    if lines.len() == 1 {
        return Ok(None);
    }
    Ok(Some(lines.join("\n")))
}

fn append_candidate_group(
    lines: &mut Vec<String>,
    title: &str,
    items: &[crate::storage::WorkItemReadinessProjection],
    completion_reports: &BTreeMap<String, String>,
    agent_home: &std::path::Path,
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let items = items
        .iter()
        .filter(|item| !item.is_current)
        .collect::<Vec<_>>();
    if items.is_empty() {
        return Ok(());
    }
    let mut title_pushed = false;
    for item in items {
        let record = item.record();
        let completion_report = if record.state == crate::types::WorkItemState::Completed {
            let report = completion_report_for_work_item(completion_reports, record);
            if report.is_none() {
                continue;
            }
            report
        } else {
            None
        };
        if !title_pushed {
            lines.push(title.to_string());
            title_pushed = true;
        }
        let view = match item.candidate_class {
            crate::storage::WorkItemCandidateClass::TriggeredBlocked => "triggered_blocked",
            crate::storage::WorkItemCandidateClass::QueuedRunnable => "queued_runnable",
            crate::storage::WorkItemCandidateClass::WaitingForOperator => "waiting_for_operator",
            crate::storage::WorkItemCandidateClass::Blocked => "blocked",
            crate::storage::WorkItemCandidateClass::CompletedRecent => "completed_recent",
            crate::storage::WorkItemCandidateClass::CurrentRunnable => "current_runnable",
        };
        let mut summary = format!("- [{view}] {} :: {}", record.id, record.objective);
        if let Some(todo) = item.current_todo.as_ref() {
            summary.push_str(&format!(" :: current_todo={}", todo.text));
        }
        if let Some(triggered_at) = item.last_triggered_at {
            summary.push_str(&format!(" :: last_triggered_at={triggered_at}"));
        }
        if let Some(blocked_by) = record.blocked_by.as_deref() {
            summary.push_str(&format!(" :: blocked_by={blocked_by}"));
        }
        lines.push(summary);
        if let Some(report) = completion_report {
            lines.push(format!(
                "  - Completion report: {}",
                truncate_text(&report.replace('\n', " "), 240)
            ));
        }
        lines.extend(render_work_item_plan_artifact_lines(
            record, agent_home, "  - ",
        ));
    }
    Ok(())
}

fn completion_report_for_work_item(
    completion_reports: &BTreeMap<String, String>,
    record: &WorkItemRecord,
) -> Option<String> {
    if let Some(summary) = record
        .result_summary
        .as_ref()
        .filter(|text| !text.is_empty())
    {
        return Some(summary.clone());
    }
    completion_reports
        .get(&record.id)
        .filter(|text| !text.is_empty())
        .cloned()
}

fn latest_delivery_summary_text_by_work_item(
    storage: &AppStorage,
    agent_id: &str,
) -> Result<BTreeMap<String, String>> {
    let mut summaries = BTreeMap::new();
    for summary in storage
        .read_recent_delivery_summaries(usize::MAX)?
        .into_iter()
        .rev()
        .filter(|summary| summary.agent_id == agent_id)
        .filter(|summary| !summary.text.is_empty())
    {
        summaries
            .entry(summary.work_item_id)
            .or_insert(summary.text);
    }
    Ok(summaries)
}

fn render_work_item_plan_artifact_lines(
    work_item: &WorkItemRecord,
    agent_home: &std::path::Path,
    prefix: &str,
) -> Vec<String> {
    let plan_artifact =
        crate::work_item_plan::ensure_plan_artifact(agent_home, work_item, None).ok();
    let Some(plan_artifact) = plan_artifact else {
        return Vec::new();
    };
    let preview_indent = " ".repeat(prefix.chars().count());
    let mut lines = vec![
        format!("{prefix}Plan artifact: {}", plan_artifact.path.display()),
        format!(
            "{prefix}Plan preview complete: {}",
            plan_artifact.preview_complete
        ),
    ];
    if !plan_artifact.preview.is_empty() {
        lines.push(format!("{prefix}Plan preview:"));
        lines.extend(
            plan_artifact
                .preview
                .lines()
                .map(|line| format!("{preview_indent}{line}")),
        );
    }
    lines
}

fn working_memory_is_empty(snapshot: &WorkingMemorySnapshot) -> bool {
    snapshot == &WorkingMemorySnapshot::default()
}

fn render_working_memory(snapshot: &WorkingMemorySnapshot) -> String {
    let mut lines = vec!["Working memory:".to_string()];
    if let Some(current_work_item_id) = snapshot.current_work_item_id.as_deref() {
        lines.push(format!("- Current work item id: {current_work_item_id}"));
    }
    if let Some(objective) = snapshot.objective.as_deref() {
        lines.push(format!("- Objective: {objective}"));
    }
    if let Some(work_summary) = snapshot.work_summary.as_deref() {
        lines.push(format!("- Work summary: {work_summary}"));
    }
    if let Some(plan) = snapshot.plan.as_deref() {
        lines.push("- Plan:".to_string());
        lines.push(format!(
            "  {}",
            truncate_text(&plan.replace('\n', " "), 160)
        ));
    }
    if !snapshot.todo_list.is_empty() {
        lines.push("- Todo list:".to_string());
        let active_items = snapshot
            .todo_list
            .iter()
            .filter(|item| item.state != TodoItemState::Completed)
            .take(3);
        lines.extend(active_items.map(|item| format!("  - {}", render_todo_item_compact(item))));
        let omitted = snapshot
            .todo_list
            .iter()
            .filter(|item| item.state != TodoItemState::Completed)
            .skip(3)
            .count();
        if omitted > 0 {
            lines.push(format!(
                "  - ... {omitted} more active todo item(s) omitted"
            ));
        }
    }
    if !snapshot.working_set_files.is_empty() {
        lines.push("- Working set files:".to_string());
        lines.extend(
            snapshot
                .working_set_files
                .iter()
                .map(|path| format!("  - {path}")),
        );
    }
    if !snapshot.pending_followups.is_empty() {
        lines.push("- Pending follow-ups:".to_string());
        lines.extend(
            snapshot
                .pending_followups
                .iter()
                .map(|followup| format!("  - {followup}")),
        );
    }
    if !snapshot.waiting_on.is_empty() {
        lines.push("- Waiting on:".to_string());
        lines.extend(
            snapshot
                .waiting_on
                .iter()
                .map(|waiting| format!("  - {waiting}")),
        );
    }
    lines.join("\n")
}

fn render_todo_item_compact(item: &crate::types::TodoItem) -> String {
    let state = match item.state {
        TodoItemState::Pending => "pending",
        TodoItemState::InProgress => "in_progress",
        TodoItemState::Completed => "completed",
    };
    format!("[{state}] {}", truncate_text(&item.text, 120))
}

fn render_working_memory_delta_with_budget(
    delta: &WorkingMemoryDelta,
    budget: usize,
) -> Option<String> {
    let mut lines = vec!["Working memory updated since the last prompt:".to_string()];
    lines.push(format!(
        "- Revision: {} -> {}",
        delta.from_revision, delta.to_revision
    ));
    lines.push(format!(
        "- Reason: {}",
        serde_json::to_string(&delta.reason)
            .unwrap_or_else(|_| "\"terminal_turn_completed\"".to_string())
            .trim_matches('"')
    ));
    if !delta.changed_fields.is_empty() {
        lines.push("- Changed fields:".to_string());
        lines.extend(
            delta
                .changed_fields
                .iter()
                .map(|field| format!("  - {field}")),
        );
    }
    if !delta.summary_lines.is_empty() {
        lines.push("- Summary:".to_string());
        let mut summary_lines = Vec::new();
        let mut summary_budget = budget.saturating_sub(estimate_text_tokens(&lines.join("\n")));
        for line in &delta.summary_lines {
            let rendered = format!("  - {line}");
            let cost = estimate_text_tokens(&rendered);
            if !summary_lines.is_empty() && cost > summary_budget {
                summary_lines.push("  - [truncated working memory delta]".to_string());
                break;
            }
            summary_budget = summary_budget.saturating_sub(cost);
            summary_lines.push(rendered);
        }
        lines.extend(summary_lines);
    }
    if lines.len() <= 3 && delta.summary_lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

fn kind_label(message: &MessageEnvelope) -> String {
    format!("{:?}", message.kind)
}

fn origin_label(origin: &MessageOrigin) -> &'static str {
    match origin {
        MessageOrigin::Operator { .. } => "operator",
        MessageOrigin::Channel { .. } => "channel",
        MessageOrigin::Webhook { .. } => "webhook",
        MessageOrigin::Callback { .. } => "callback",
        MessageOrigin::Timer { .. } => "timer",
        MessageOrigin::System { .. } => "system",
        MessageOrigin::Task { .. } => "task",
    }
}

fn trust_label(authority_class: &AuthorityClass) -> &'static str {
    match authority_class {
        AuthorityClass::OperatorInstruction => "trusted_operator",
        AuthorityClass::RuntimeInstruction => "trusted_system",
        AuthorityClass::IntegrationSignal => "trusted_integration",
        AuthorityClass::ExternalEvidence => "untrusted_external",
    }
}

fn authority_class_label(authority_class: AuthorityClass) -> &'static str {
    match authority_class {
        AuthorityClass::OperatorInstruction => "operator_instruction",
        AuthorityClass::RuntimeInstruction => "runtime_instruction",
        AuthorityClass::IntegrationSignal => "integration_signal",
        AuthorityClass::ExternalEvidence => "external_evidence",
    }
}

fn delivery_surface_label(surface: MessageDeliverySurface) -> &'static str {
    match surface {
        MessageDeliverySurface::CliPrompt => "cli_prompt",
        MessageDeliverySurface::RunOnce => "run_once",
        MessageDeliverySurface::HttpPublicEnqueue => "http_public_enqueue",
        MessageDeliverySurface::HttpWebhook => "http_webhook",
        MessageDeliverySurface::HttpCallbackEnqueue => "http_callback_enqueue",
        MessageDeliverySurface::HttpCallbackWake => "http_callback_wake",
        MessageDeliverySurface::HttpControlPrompt => "http_control_prompt",
        MessageDeliverySurface::RemoteOperatorTransport => "remote_operator_transport",
        MessageDeliverySurface::TimerScheduler => "timer_scheduler",
        MessageDeliverySurface::RuntimeSystem => "runtime_system",
        MessageDeliverySurface::TaskRejoin => "task_rejoin",
    }
}

fn admission_context_label(context: AdmissionContext) -> &'static str {
    match context {
        AdmissionContext::PublicUnauthenticated => "public_unauthenticated",
        AdmissionContext::ControlAuthenticated => "control_authenticated",
        AdmissionContext::OperatorTransportAuthenticated => "operator_transport_authenticated",
        AdmissionContext::ExternalTriggerCapability => "external_trigger_capability",
        AdmissionContext::LocalProcess => "local_process",
        AdmissionContext::RuntimeOwned => "runtime_owned",
    }
}

fn scope_label(scope: &crate::types::SkillScope) -> &'static str {
    match scope {
        crate::types::SkillScope::User => "user",
        crate::types::SkillScope::Agent => "agent",
        crate::types::SkillScope::Workspace => "workspace",
    }
}

fn activation_source_label(source: crate::types::SkillActivationSource) -> &'static str {
    match source {
        crate::types::SkillActivationSource::Explicit => "explicit",
        crate::types::SkillActivationSource::ImplicitFromCatalog => "implicit_from_catalog",
        crate::types::SkillActivationSource::Restored => "restored",
        crate::types::SkillActivationSource::Inherited => "inherited",
    }
}

fn activation_state_label(state: crate::types::SkillActivationState) -> &'static str {
    match state {
        crate::types::SkillActivationState::TurnActive => "turn_active",
        crate::types::SkillActivationState::SessionActive => "session_active",
    }
}

fn body_preview(body: &MessageBody) -> String {
    let text = message_body_text(body);
    if text.chars().count() <= 160 {
        text
    } else {
        format!("{}...", text.chars().take(160).collect::<String>())
    }
}

fn message_body_text(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => value.to_string(),
        MessageBody::Brief { text, .. } => text.clone(),
    }
}

fn render_recent_turn_input_line(message: &MessageEnvelope) -> String {
    if is_trusted_operator_input(message) {
        format!(
            "  - operator input: {}",
            sanitize_inline(&message_body_text(&message.body))
        )
    } else {
        format!(
            "  - input: {}",
            sanitize_inline(&body_preview(&message.body))
        )
    }
}

fn render_recent_turn_brief_line(brief: &BriefRecord) -> String {
    format!(
        "    - {:?}: {} brief_ref=brief:{}",
        brief.kind,
        sanitize_inline(&truncate_text(&brief.text, 160)),
        sanitize_inline(&brief.id)
    )
}

fn render_current_input_body_with_budget(
    body: &MessageBody,
    budget: usize,
    wake_hint_fallback: Option<&str>,
) -> String {
    let mut rendered = match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        MessageBody::Brief { text, .. } => text.clone(),
    };
    if let Some(wake_hint) = wake_hint_fallback {
        rendered.push_str("\nwake_hint_context:\n");
        rendered.push_str(wake_hint);
    }
    truncate_section_content(
        "",
        &rendered,
        budget.max(64),
        Some("\n[truncated current input body]"),
    )
}

fn render_continuation_anchor(
    messages: &[MessageEnvelope],
    current_message: &MessageEnvelope,
    current_work_item: Option<&WorkItemRecord>,
    budget: usize,
) -> Option<String> {
    let latest_operator = latest_trusted_operator_input(messages, current_message);
    let relation = current_input_relation(current_message, latest_operator);
    if latest_operator.is_none() && relation.is_none() {
        return None;
    }

    let mut lines = vec!["Continuation anchor:".to_string()];
    if let Some(operator) = latest_operator {
        if same_message_identity(current_message, operator) {
            lines.push("Latest trusted operator input: current_input.".to_string());
        } else if current_work_item.is_some() {
            lines.push(format!(
                "Latest trusted operator input: {}.",
                operator
                    .message_seq
                    .map(|seq| format!("message_seq {seq}"))
                    .unwrap_or_else(|| operator.id.clone())
            ));
        } else {
            let prefix = "Latest trusted operator input:\n";
            let reserved = estimate_text_tokens("Continuation anchor:\nCurrent input relation:");
            let body_budget = budget.saturating_sub(reserved).max(32);
            lines.push(truncate_section_content(
                prefix,
                &render_message_body_for_anchor(&operator.body),
                body_budget,
                Some("\n[truncated trusted operator input]"),
            ));
        }
    }

    if let Some(relation) = relation {
        lines.push(format!("Current input relation: {relation}"));
    }

    Some(lines.join("\n"))
}

fn latest_trusted_operator_input<'a>(
    messages: &'a [MessageEnvelope],
    current_message: &'a MessageEnvelope,
) -> Option<&'a MessageEnvelope> {
    std::iter::once(current_message)
        .chain(messages.iter().rev())
        .find(|message| is_trusted_operator_input(message))
}

fn is_trusted_operator_input(message: &MessageEnvelope) -> bool {
    message.authority_class == AuthorityClass::OperatorInstruction
        && matches!(message.origin, MessageOrigin::Operator { .. })
}

fn current_input_relation(
    current_message: &MessageEnvelope,
    latest_operator: Option<&MessageEnvelope>,
) -> Option<String> {
    if is_trusted_operator_input(current_message) {
        return Some(
            if latest_operator
                .is_some_and(|operator| same_message_identity(current_message, operator))
            {
                "current_input is the latest trusted operator input.".to_string()
            } else {
                "current_input is a trusted operator override newer than previous state."
                    .to_string()
            },
        );
    }

    if latest_operator.is_some() {
        Some(format!(
            "current_input is {}, not a new operator request. Continue the latest trusted operator input above unless the current WorkItem projection is more specific.",
            runtime_continuation_label(current_message)
        ))
    } else {
        Some(format!(
            "current_input is {}, not a trusted operator request.",
            runtime_continuation_label(current_message)
        ))
    }
}

fn runtime_continuation_label(message: &MessageEnvelope) -> &'static str {
    match message.trigger_kind {
        Some(ContinuationTriggerKind::TaskResult) => "a task-result continuation",
        Some(ContinuationTriggerKind::ExternalEvent) => "an external-event continuation",
        Some(ContinuationTriggerKind::TimerFire) => "a timer continuation",
        Some(ContinuationTriggerKind::InternalFollowup) => "an internal-followup continuation",
        Some(ContinuationTriggerKind::SystemTick) => "a runtime system-tick continuation",
        Some(ContinuationTriggerKind::OperatorInput) => "operator-triggered input",
        None => match message.kind {
            MessageKind::TaskResult | MessageKind::TaskStatus => "a task-result continuation",
            MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent => {
                "an external-event continuation"
            }
            MessageKind::TimerTick => "a timer continuation",
            MessageKind::InternalFollowup => "an internal-followup continuation",
            MessageKind::SystemTick => "a runtime system-tick continuation",
            _ => "runtime-originated input",
        },
    }
}

fn render_message_body_for_anchor(body: &MessageBody) -> String {
    match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        MessageBody::Brief { text, .. } => text.clone(),
    }
}

fn same_message_identity(left: &MessageEnvelope, right: &MessageEnvelope) -> bool {
    left.id == right.id
        || (left.message_seq.is_some()
            && right.message_seq.is_some()
            && left.message_seq == right.message_seq)
}

fn continuation_context(
    message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
) -> Option<String> {
    let continuation = continuation?;
    if !continuation.model_reentry {
        return None;
    }

    let mut lines = vec![
        "Continuation context:".to_string(),
        format!(
            " - Trigger kind: {}",
            enum_label(&continuation.trigger_kind)
        ),
        format!(" - Continuation class: {}", enum_label(&continuation.class)),
        format!(
            " - Prior closure outcome: {}",
            enum_label(&continuation.prior_closure_outcome)
        ),
        format!(
            " - Prior waiting reason: {}",
            continuation
                .prior_waiting_reason
                .map(|reason| enum_label(&reason))
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            " - Waiting reason matched: {}",
            continuation.matched_waiting_reason
        ),
    ];

    if let Some(wake_hint) = render_wake_hint_context(message) {
        lines.push(wake_hint);
    }

    if let Some(work_queue) = render_work_queue_tick_context(message) {
        lines.push(work_queue);
    }

    if continuation.class == ContinuationClass::ResumeOverride {
        lines.push(" - This continuation overrides the prior wait.".to_string());
    }

    Some(lines.join("\n"))
}

fn enum_label<T: serde::Serialize + std::fmt::Debug>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| format!("{value:?}"))
}

fn render_wake_hint_context(message: &MessageEnvelope) -> Option<String> {
    if message.kind != MessageKind::SystemTick {
        return None;
    }
    let wake_hint = message.metadata.as_ref()?.get("wake_hint")?;
    let reason = wake_hint
        .get("reason")
        .and_then(serde_json::Value::as_str)?;
    let source = wake_hint
        .get("source")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let resource = wake_hint
        .get("resource")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none");
    let content_type = wake_hint
        .get("content_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none");
    let payload = wake_hint
        .get("body")
        .and_then(|value| serde_json::from_value::<MessageBody>(value.clone()).ok())
        .map(render_continuation_body)
        .unwrap_or_else(|| "none".to_string());
    let mut lines = vec![" - Wake hint:".to_string(), format!("- Source: {source}")];
    if let Some(scope) = wake_hint.get("scope").and_then(serde_json::Value::as_str) {
        lines.push(format!("- Scope: {scope}"));
    }
    if let Some(external_trigger_id) = wake_hint
        .get("external_trigger_id")
        .and_then(serde_json::Value::as_str)
    {
        lines.push(format!("- External trigger id: {external_trigger_id}"));
    }
    if let Some(waiting_intent_id) = wake_hint
        .get("waiting_intent_id")
        .and_then(serde_json::Value::as_str)
    {
        lines.push(format!("- Waiting intent id: {waiting_intent_id}"));
    }
    if let Some(work_item_id) = wake_hint
        .get("work_item_id")
        .and_then(serde_json::Value::as_str)
    {
        lines.push(format!("- Work item id: {work_item_id}"));
    }
    if let Some(description) = wake_hint
        .get("description")
        .and_then(serde_json::Value::as_str)
    {
        lines.push(format!("- Description: {description}"));
    }
    lines.extend([
        format!("- Resource: {resource}"),
        format!("- Reason: {reason}"),
        format!("- Content-Type: {content_type}"),
        format!("- Payload:\n{payload}"),
    ]);
    Some(lines.join("\n"))
}

fn render_work_queue_tick_context(message: &MessageEnvelope) -> Option<String> {
    if message.kind != MessageKind::SystemTick {
        return None;
    }
    let work_queue = message.metadata.as_ref()?.get("work_queue")?;
    let reason = work_queue
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let work_item_id = work_queue
        .get("work_item_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let objective = work_queue
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let runtime_switched_current = work_queue
        .get("runtime_switched_current_item")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Some(format!(
        " - Work queue:\n\
         - Reason: {reason}\n\
         - Work item id: {work_item_id}\n\
         - Objective: {objective}\n\
         - Runtime switched current item: {runtime_switched_current}"
    ))
}

fn render_continuation_body(body: MessageBody) -> String {
    let rendered = match body {
        MessageBody::Text { text } => text,
        MessageBody::Json { value } => {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        }
        MessageBody::Brief { text, .. } => text,
    };
    truncate_continuation_body(&rendered)
}

fn truncate_continuation_body(text: &str) -> String {
    const MAX_CHARS: usize = 4000;
    if text.chars().count() <= MAX_CHARS {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(MAX_CHARS).collect::<String>())
    }
}

fn estimate_section_tokens(section: &PromptSection) -> usize {
    estimate_text_tokens(&format!("[{}]\n{}", section.name, section.content))
}

fn estimate_message_tokens(message: &MessageEnvelope) -> usize {
    estimate_text_tokens(&render_message(message))
}

fn estimate_text_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    chars.div_ceil(4).max(1)
}

fn truncate_section_content(
    prefix: &str,
    text: &str,
    budget: usize,
    truncation_notice: Option<&str>,
) -> String {
    let full = format!("{prefix}{text}");
    if estimate_text_tokens(&full) <= budget {
        return full;
    }

    let suffix = format!("...{}", truncation_notice.unwrap_or(""));
    let prefix_only = prefix.trim_end().to_string();
    if estimate_text_tokens(&(prefix.to_string() + &suffix)) > budget {
        return prefix_only;
    }

    let chars = text.chars().collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high).div_ceil(2);
        let candidate = format!(
            "{prefix}{}{}",
            chars[..mid].iter().collect::<String>(),
            suffix
        );
        if estimate_text_tokens(&candidate) <= budget {
            low = mid;
        } else {
            high = mid.saturating_sub(1);
        }
    }

    format!(
        "{prefix}{}{}",
        chars[..low].iter().collect::<String>(),
        suffix
    )
}

fn render_recent_turns_with_budget(
    turn_records: &[TurnRecord],
    messages: &[MessageEnvelope],
    briefs: &[BriefRecord],
    tools: &[ToolExecutionRecord],
    current_message: &MessageEnvelope,
    current_work_item: Option<&WorkItemRecord>,
    budget: usize,
) -> Option<String> {
    render_turn_records_with_budget(
        turn_records,
        messages,
        briefs,
        tools,
        current_message,
        current_work_item,
        budget,
    )
}

fn render_turn_records_with_budget(
    turn_records: &[TurnRecord],
    messages: &[MessageEnvelope],
    briefs: &[BriefRecord],
    tools: &[ToolExecutionRecord],
    current_message: &MessageEnvelope,
    current_work_item: Option<&WorkItemRecord>,
    budget: usize,
) -> Option<String> {
    if turn_records.is_empty() {
        return None;
    }

    let latest_operator_for_continuation = (!is_trusted_operator_input(current_message))
        .then(|| latest_trusted_operator_input(messages, current_message))
        .flatten();
    let continuation_turn_id = latest_operator_for_continuation
        .and_then(|operator| {
            turn_records
                .iter()
                .find(|record| turn_record_matches_message(record, operator))
        })
        .map(|record| record.turn_id.as_str());

    let mut rendered_turns = turn_records
        .iter()
        .filter(|record| continuation_turn_id.is_none_or(|turn_id| record.turn_id != turn_id))
        .filter_map(|record| render_turn_record_projection(record, messages, briefs, tools, None))
        .collect::<Vec<_>>();

    if let Some(operator) = latest_operator_for_continuation {
        if let Some(record) = turn_records
            .iter()
            .find(|record| turn_record_matches_message(record, operator))
        {
            if let Some(rendered) = render_current_continuation_turn_record_projection(
                record,
                current_message,
                operator,
                messages,
                briefs,
                tools,
                current_work_item,
            ) {
                rendered_turns.push(rendered);
            }
        }
    }

    if rendered_turns.is_empty() {
        return None;
    }

    render_budgeted_lines("Recent turns:", rendered_turns, budget)
}

fn render_turn_record_projection(
    record: &TurnRecord,
    messages: &[MessageEnvelope],
    briefs: &[BriefRecord],
    tools: &[ToolExecutionRecord],
    continuation: Option<&MessageEnvelope>,
) -> Option<String> {
    let is_legacy_synthetic_record = record.turn_id.starts_with("legacy-");
    let trigger_message = record
        .input_message_ids
        .iter()
        .find_map(|id| messages.iter().find(|message| message.id == *id))
        .or_else(|| {
            record
                .trigger
                .as_ref()
                .and_then(|trigger| trigger.message_id.as_ref())
                .and_then(|id| messages.iter().find(|message| message.id == *id))
        });
    let mut lines = vec![format!(
        "- Turn {}:",
        if is_legacy_synthetic_record && record.turn_index != 0 {
            format!("message_seq {}", record.turn_index)
        } else if record.turn_index != 0 {
            format!("turn_index {}", record.turn_index)
        } else {
            record.turn_id.clone()
        }
    )];
    if !is_legacy_synthetic_record {
        lines.push(format!("  - turn_id: {}", sanitize_inline(&record.turn_id)));
    }
    if let Some(trigger_message) = trigger_message {
        lines.push(format!(
            "  - trigger: {}",
            turn_trigger_label(trigger_message)
        ));
    } else if let Some(trigger) = &record.trigger {
        lines.push(format!(
            "  - trigger: {}",
            turn_trigger_summary_label(trigger)
        ));
    } else {
        lines.push("  - trigger: unavailable".to_string());
    }
    if let Some(continuation) = continuation {
        lines.push(format!(
            "  - continues input: {}",
            trigger_message
                .and_then(|message| message.message_seq.map(|seq| format!("message_seq {seq}")))
                .or_else(|| {
                    trigger_message
                        .map(|message| message.id.clone())
                        .or_else(|| {
                            record
                                .trigger
                                .as_ref()
                                .and_then(|trigger| trigger.message_id.clone())
                        })
                })
                .unwrap_or_else(|| record.turn_id.clone())
        ));
        lines.push(format!(
            "  - continuation trigger: {}",
            turn_trigger_label(continuation)
        ));
    }
    if let Some(trigger_message) = trigger_message {
        lines.push(render_recent_turn_input_line(trigger_message));
    }

    let related_briefs = record
        .produced_brief_ids
        .iter()
        .filter_map(|id| briefs.iter().find(|brief| brief.id == *id))
        .map(render_recent_turn_brief_line)
        .collect::<Vec<_>>();
    if !related_briefs.is_empty() {
        lines.push("  - produced briefs:".to_string());
        lines.extend(related_briefs);
    }

    let related_tools = record
        .tool_execution_ids
        .iter()
        .filter_map(|id| tools.iter().find(|tool| tool.id == *id))
        .map(render_recent_tool_execution)
        .collect::<Vec<_>>();
    if !related_tools.is_empty() {
        lines.push("  - tool executions:".to_string());
        lines.extend(related_tools.into_iter().map(|tool| format!("    {tool}")));
    }

    Some(lines.join("\n"))
}

fn render_current_continuation_turn_record_projection(
    record: &TurnRecord,
    current_message: &MessageEnvelope,
    operator: &MessageEnvelope,
    messages: &[MessageEnvelope],
    briefs: &[BriefRecord],
    tools: &[ToolExecutionRecord],
    current_work_item: Option<&WorkItemRecord>,
) -> Option<String> {
    let mut rendered =
        render_turn_record_projection(record, messages, briefs, tools, Some(current_message))?;
    let mut lines = vec![
        format!(
            "  - current relation: {}",
            runtime_continuation_label(current_message)
        ),
        format!(
            "  - current input: {}",
            sanitize_inline(&body_preview(&current_message.body))
        ),
    ];
    if let Some(work_item) = current_work_item {
        lines.push(format!(
            "  - current work item: {} :: {}",
            sanitize_inline(&work_item.id),
            sanitize_inline(&truncate_text(&work_item.objective, 160))
        ));
    }
    if !turn_record_matches_message(record, operator) {
        lines.push(format!(
            "  - continues input id: {}",
            sanitize_inline(&operator.id)
        ));
    }
    rendered.push('\n');
    rendered.push_str(&lines.join("\n"));
    Some(rendered)
}

fn turn_record_matches_message(record: &TurnRecord, message: &MessageEnvelope) -> bool {
    if let Some(turn_id) = message.turn_id.as_deref().map(str::trim) {
        if !turn_id.is_empty() {
            if turn_id == record.turn_id.trim() {
                return true;
            }
            if !record.turn_id.starts_with("legacy-") {
                return false;
            }
        }
    }

    record.input_message_ids.iter().any(|id| id == &message.id)
        || record
            .trigger
            .as_ref()
            .and_then(|trigger| trigger.message_id.as_ref())
            .is_some_and(|id| id == &message.id)
        || record.turn_index != 0 && message.message_seq == Some(record.turn_index)
}

fn turn_trigger_label(message: &MessageEnvelope) -> &'static str {
    if is_trusted_operator_input(message) {
        return "trusted operator input";
    }
    runtime_continuation_label(message)
}

fn turn_trigger_summary_label(trigger: &crate::types::TurnTriggerSummary) -> &'static str {
    if matches!(trigger.authority_class, AuthorityClass::OperatorInstruction) {
        return "trusted operator input";
    }
    match trigger.trigger_kind {
        Some(ContinuationTriggerKind::TaskResult) => "a task-result continuation",
        Some(ContinuationTriggerKind::ExternalEvent) => "an external-event continuation",
        Some(ContinuationTriggerKind::TimerFire) => "a timer continuation",
        Some(ContinuationTriggerKind::InternalFollowup) => "an internal-followup continuation",
        Some(ContinuationTriggerKind::SystemTick) => "a runtime system-tick continuation",
        Some(ContinuationTriggerKind::OperatorInput) => "operator-triggered input",
        None => match trigger.kind {
            MessageKind::TaskResult | MessageKind::TaskStatus => "a task-result continuation",
            MessageKind::CallbackEvent | MessageKind::WebhookEvent | MessageKind::ChannelEvent => {
                "an external-event continuation"
            }
            MessageKind::TimerTick => "a timer continuation",
            MessageKind::InternalFollowup => "an internal-followup continuation",
            MessageKind::SystemTick => "a runtime system-tick continuation",
            _ => "runtime-originated input",
        },
    }
}

fn sanitize_inline(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            pending_space = !sanitized.is_empty();
        } else {
            if pending_space {
                sanitized.push(' ');
                pending_space = false;
            }
            sanitized.push(ch);
        }
    }
    sanitized
}

fn command_output_refs(record: &ToolExecutionRecord, batch_item_index: Option<usize>) -> String {
    let output = match (record.tool_name.as_str(), batch_item_index) {
        ("ExecCommand", None) => record
            .output
            .get("result")
            .or_else(|| {
                record
                    .output
                    .get("envelope")
                    .and_then(|value| value.get("result"))
            })
            .unwrap_or(&record.output),
        ("ExecCommandBatch", Some(index)) => match record
            .output
            .get("result")
            .or_else(|| {
                record
                    .output
                    .get("envelope")
                    .and_then(|value| value.get("result"))
            })
            .unwrap_or(&record.output)
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| items.get(index.saturating_sub(1)))
        {
            Some(item) => item.get("result").unwrap_or(item),
            None => return String::new(),
        },
        _ => return String::new(),
    };
    let has_output_evidence = [
        "stdout_preview",
        "stderr_preview",
        "initial_output_preview",
        "stdout_artifact",
        "stderr_artifact",
    ]
    .iter()
    .any(|key| output.get(key).is_some());
    if !has_output_evidence {
        return String::new();
    }
    format!(
        " stdout_ref={} stderr_ref={} output_ref={}",
        crate::tool::helpers::command_output_source_ref(&record.id, batch_item_index, "stdout"),
        crate::tool::helpers::command_output_source_ref(&record.id, batch_item_index, "stderr"),
        crate::tool::helpers::command_output_source_ref(&record.id, batch_item_index, "output")
    )
}

fn render_recent_tool_execution(record: &ToolExecutionRecord) -> String {
    let prefix = format!(
        "- [{}][{:?}] {}",
        trust_label(&record.authority_class),
        record.status,
        record.summary
    );
    match record.tool_name.as_str() {
        "ExecCommand" => record
            .input
            .get("cmd")
            .and_then(Value::as_str)
            .map(|cmd| {
                format!(
                    "{prefix} tool_execution_id={} cmd_digest={} cmd_ref={}{} cmd_preview={}",
                    record.id,
                    crate::tool::helpers::command_digest(cmd),
                    crate::tool::helpers::command_receipt_source_ref(&record.id, None),
                    command_output_refs(record, None),
                    crate::tool::helpers::command_preview(cmd)
                )
            })
            .unwrap_or(prefix),
        "ExecCommandBatch" => {
            let Some(items) = record.input.get("items").and_then(Value::as_array) else {
                return prefix;
            };
            let refs = items
                .iter()
                .enumerate()
                .filter_map(|(offset, item)| {
                    let cmd = item.get("cmd").and_then(Value::as_str)?;
                    let index = offset + 1;
                    Some(format!(
                        "{{index={index}, cmd_digest={}, cmd_ref={},{} cmd_preview={}}}",
                        crate::tool::helpers::command_digest(cmd),
                        crate::tool::helpers::command_receipt_source_ref(&record.id, Some(index)),
                        command_output_refs(record, Some(index)),
                        crate::tool::helpers::command_preview(cmd)
                    ))
                })
                .collect::<Vec<_>>();
            if refs.is_empty() {
                prefix
            } else {
                format!(
                    "{prefix} tool_execution_id={} batch_cmds=[{}]",
                    record.id,
                    refs.join(", ")
                )
            }
        }
        _ => prefix,
    }
}

fn render_budgeted_lines(heading: &str, lines: Vec<String>, budget: usize) -> Option<String> {
    if lines.is_empty() {
        return None;
    }

    let mut selected = Vec::new();
    let mut used = estimate_text_tokens(heading) + 1;
    for line in lines.into_iter().rev() {
        let cost = estimate_text_tokens(&line);
        if used + cost > budget {
            break;
        }
        used += cost;
        selected.push(line);
    }

    if selected.is_empty() {
        return None;
    }

    selected.reverse();
    Some(format!("{heading}\n{}", selected.join("\n")))
}

#[derive(Debug, Clone)]
struct EpisodeSelectionAnchor<'a> {
    current_work_item_id: Option<&'a str>,
    objective: Option<&'a str>,
    work_summary: Option<&'a str>,
    working_set_files: &'a [String],
    pending_followups: &'a [String],
    waiting_on: &'a [String],
    query_text: String,
}

fn build_relevant_episode_memory_section(
    episodes: &[ContextEpisodeRecord],
    agent: &AgentState,
    current_work_item: Option<&WorkItemRecord>,
    current_message: &MessageEnvelope,
    config: &ContextConfig,
    budget: usize,
    recent_turn_window_start: Option<u64>,
) -> Option<PromptSection> {
    if episodes.is_empty() || config.max_relevant_episodes == 0 || budget == 0 {
        return None;
    }

    let working_memory = &agent.working_memory.current_working_memory;
    let query_text = format!(
        "{}\n{}\n{}",
        body_preview(&current_message.body),
        working_memory.work_summary.as_deref().unwrap_or_default(),
        current_work_item
            .map(|item| item.objective.as_str())
            .unwrap_or_default()
    );
    let anchor = EpisodeSelectionAnchor {
        current_work_item_id: working_memory
            .current_work_item_id
            .as_deref()
            .or_else(|| current_work_item.map(|item| item.id.as_str())),
        objective: working_memory
            .objective
            .as_deref()
            .or_else(|| current_work_item.map(|item| item.objective.as_str())),
        work_summary: working_memory.work_summary.as_deref(),
        working_set_files: &working_memory.working_set_files,
        pending_followups: &working_memory.pending_followups,
        waiting_on: &working_memory.waiting_on,
        query_text,
    };

    let episode_count = episodes.len();
    let mut ranked = episodes
        .iter()
        .enumerate()
        .filter(|(_, episode)| {
            episode_is_before_recent_turn_window(episode, recent_turn_window_start)
        })
        .map(|(index, episode)| {
            let recency_index = episode_count.saturating_sub(index + 1);
            (
                episode_relevance_score(episode, &anchor, recency_index),
                index,
                episode,
            )
        })
        .filter(|(score, _, _)| *score > 0)
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.2.finalized_at.cmp(&left.2.finalized_at))
            .then_with(|| right.1.cmp(&left.1))
    });

    let section_overhead = estimate_text_tokens("Relevant episode memory:");
    let mut remaining = budget.saturating_sub(section_overhead);
    let mut blocks = Vec::new();
    for (_, _, episode) in ranked.into_iter().take(config.max_relevant_episodes) {
        let block = render_episode_block(episode);
        let cost = estimate_text_tokens(&block);
        if cost > remaining && !blocks.is_empty() {
            break;
        }
        remaining = remaining.saturating_sub(cost);
        blocks.push(block);
    }

    if blocks.is_empty() {
        return None;
    }

    Some(section(
        "relevant_episode_memory",
        format!("Relevant episode memory:\n{}", blocks.join("\n")),
    ))
}

fn recent_turn_window_start(turn_records: &[crate::types::TurnRecord]) -> Option<u64> {
    turn_records
        .iter()
        .filter_map(|turn| (turn.turn_index != 0).then_some(turn.turn_index))
        .min()
}

fn episode_is_before_recent_turn_window(
    episode: &ContextEpisodeRecord,
    recent_turn_window_start: Option<u64>,
) -> bool {
    recent_turn_window_start
        .map(|start| episode.end_turn_index < start)
        .unwrap_or(true)
}

fn render_episode_block(episode: &ContextEpisodeRecord) -> String {
    let mut lines = vec![format!(
        "- [episode {}][turns {}-{}][boundary {}]",
        episode.id,
        episode.start_turn_index,
        episode.end_turn_index,
        enum_label(&episode.boundary_reason)
    )];
    if let Some(objective) = episode.objective.as_deref() {
        lines.push(format!("  - Objective: {objective}"));
    }
    if let Some(work_summary) = episode.work_summary.as_deref() {
        lines.push(format!("  - Work summary: {work_summary}"));
    }
    if !episode.scope_hints.is_empty() {
        lines.push(format!(
            "  - Scope hints: {}",
            episode
                .scope_hints
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !episode.source_turn_ids.is_empty() || !episode.source_refs.is_empty() {
        let mut source_parts = Vec::new();
        if !episode.source_turn_ids.is_empty() {
            source_parts.push(format!(
                "turns [{}]",
                episode
                    .source_turn_ids
                    .iter()
                    .take(8)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !episode.source_refs.is_empty() {
            source_parts.push(format!(
                "refs [{}]",
                episode
                    .source_refs
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        lines.push(format!("  - Source refs: {}", source_parts.join("; ")));
    }
    if let Some(generated_by) = episode.generated_by.as_ref() {
        lines.push(format!(
            "  - Generated by: {} for {}",
            generated_by.component,
            enum_label(&generated_by.reason)
        ));
    }
    if !episode.operator_intents.is_empty() {
        lines.push(format!(
            "  - Operator intent (authoritative only from sources): {}",
            episode
                .operator_intents
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !episode.runtime_facts.is_empty() {
        lines.push(format!(
            "  - Runtime facts: {}",
            episode
                .runtime_facts
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !episode.task_results.is_empty() {
        lines.push(format!(
            "  - Task results: {}",
            episode
                .task_results
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !episode.unresolved_items.is_empty() {
        lines.push(format!(
            "  - Unresolved items: {}",
            episode
                .unresolved_items
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    lines.push(format!("  - Summary: {}", episode.summary));
    if !episode.model_inferences.is_empty() {
        lines.push(format!(
            "  - Model inference (non-authoritative evidence): {}",
            episode
                .model_inferences
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !episode.working_set_files.is_empty() {
        lines.push(format!(
            "  - Files: {}",
            episode
                .working_set_files
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !episode.verification.is_empty() {
        lines.push(format!(
            "  - Verification: {}",
            episode
                .verification
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !episode.carry_forward.is_empty() {
        lines.push(format!(
            "  - Carry forward: {}",
            episode
                .carry_forward
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    lines.join("\n")
}

fn episode_relevance_score(
    episode: &ContextEpisodeRecord,
    anchor: &EpisodeSelectionAnchor<'_>,
    recency_index: usize,
) -> usize {
    let mut score = anchor
        .current_work_item_id
        .filter(|id| episode.current_work_item_id.as_deref() == Some(*id))
        .map(|_| 120)
        .unwrap_or(0);

    if normalized_option_eq(episode.objective.as_deref(), anchor.objective) {
        score += 80;
    }
    if normalized_option_eq(episode.work_summary.as_deref(), anchor.work_summary) {
        score += 50;
    }

    score += overlap_count(&episode.working_set_files, anchor.working_set_files) * 20;
    score += overlap_count(&episode.carry_forward, anchor.pending_followups) * 16;
    score += overlap_count(&episode.waiting_on, anchor.waiting_on) * 16;

    if text_matches_query(&episode.summary, &anchor.query_text) {
        score += 24;
    }
    if episode
        .work_summary
        .as_deref()
        .is_some_and(|summary| text_matches_query(summary, &anchor.query_text))
    {
        score += 18;
    }
    score + config_recent_bonus(recency_index)
}

fn config_recent_bonus(recency_index: usize) -> usize {
    16usize.saturating_sub(recency_index)
}

fn reserve_current_input_budget(total_budget: usize) -> usize {
    total_budget.min(256)
}

fn normalized_option_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => normalize_text(left) == normalize_text(right),
        _ => false,
    }
}

fn overlap_count(left: &[String], right: &[String]) -> usize {
    left.iter()
        .filter(|value| {
            let normalized = normalize_text(value);
            right
                .iter()
                .any(|candidate| normalize_text(candidate) == normalized)
        })
        .count()
}

fn text_matches_query(text: &str, query: &str) -> bool {
    let query_terms = tokenize_significant_terms(query);
    if query_terms.is_empty() {
        return false;
    }
    let haystack = normalize_text(text);
    query_terms
        .iter()
        .any(|term| haystack.contains(term.as_str()))
}

fn tokenize_significant_terms(text: &str) -> Vec<String> {
    normalize_text(text)
        .split_whitespace()
        .filter(|term| term.len() >= 4)
        .map(ToString::to_string)
        .collect()
}

fn normalize_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
}

fn indent_block(text: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    text.lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn include_in_prompt_context(message: &MessageEnvelope) -> bool {
    !matches!(message.kind, MessageKind::SystemTick)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::tempdir;

    use crate::{
        prompt::build_effective_prompt,
        runtime_db::RuntimeDb,
        storage::AppStorage,
        types::{
            AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
            AgentVisibility, AuthorityClass, BriefKind, BriefRecord, CallbackDeliveryMode,
            ContextEpisodeRecord, ContinuationTriggerKind, EpisodeBoundaryReason,
            ExternalTriggerScope, LoadedAgentsMd, MessageKind, MessageOrigin, Priority, TodoItem,
            TodoItemState, ToolExecutionRecord, ToolExecutionStatus, TranscriptEntry,
            TranscriptEntryKind, WaitingIntentRecord, WaitingIntentScope, WaitingIntentStatus,
            WorkItemState,
        },
    };

    use super::*;

    fn append_turn_for_message(
        storage: &AppStorage,
        message: &MessageEnvelope,
        turn_id: &str,
        turn_index: usize,
    ) -> TurnRecord {
        let mut turn = TurnRecord::new(&message.agent_id, turn_id, turn_index as u64);
        turn.input_message_ids = vec![message.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(message));
        storage.append_turn(&turn).unwrap();
        turn
    }

    #[test]
    fn turn_projection_budget_applies_ratio_floor_and_ceiling() {
        let ratio = ContextConfig {
            prompt_budget_estimated_tokens: 20_000,
            turn_projection_budget_ratio: 0.25,
            turn_projection_min_budget: 4_096,
            turn_projection_max_budget: 64_000,
            ..ContextConfig::default()
        };
        assert_eq!(ratio.turn_projection_budget(), 5_000);

        let floor = ContextConfig {
            prompt_budget_estimated_tokens: 2_000,
            turn_projection_budget_ratio: 0.30,
            turn_projection_min_budget: 4_096,
            turn_projection_max_budget: 64_000,
            ..ContextConfig::default()
        };
        assert_eq!(floor.turn_projection_budget(), 4_096);

        let ceiling = ContextConfig {
            prompt_budget_estimated_tokens: 258_400,
            turn_projection_budget_ratio: 0.30,
            turn_projection_min_budget: 4_096,
            turn_projection_max_budget: 64_000,
            ..ContextConfig::default()
        };
        assert_eq!(ceiling.turn_projection_budget(), 64_000);
    }

    #[test]
    fn recent_turns_prefers_db_turn_records_when_available() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db)
            .unwrap();

        let mut operator = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Use the database turn spine.".into(),
            },
        );
        operator.turn_id = Some("turn-db-context".into());
        storage.append_message(&operator).unwrap();
        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Rendered from DB turn record.",
            Some(operator.id.clone()),
            None,
        );
        brief.id = "brief-db-context".into();
        brief.turn_id = Some("turn-db-context".into());
        storage.append_brief(&brief).unwrap();
        let mut turn = TurnRecord::new("default", "turn-db-context", 1);
        turn.input_message_ids = vec![operator.id.clone()];
        turn.produced_brief_ids = vec![brief.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&operator));
        storage.append_turn(&turn).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue.".into(),
            },
        );
        let context = build_context(
            &storage,
            &AgentState::new("default"),
            &execution_snapshot_for(&AgentState::new("default")),
            &SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig::default(),
        )
        .unwrap();
        let recent_turns = context
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section")
            .content
            .clone();
        assert!(recent_turns.contains("- Turn turn_index 1:"));
        assert!(recent_turns.contains("  - turn_id: turn-db-context"));
        assert!(recent_turns.contains("  - operator input: Use the database turn spine."));
        assert!(recent_turns.contains("Rendered from DB turn record."));
        assert!(recent_turns.contains("brief_ref=brief:brief-db-context"));
    }

    #[test]
    fn recent_turns_keeps_db_turn_when_trigger_message_is_outside_window() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db)
            .unwrap();

        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Rendered without trigger hydration.",
            Some("missing-trigger-message".into()),
            None,
        );
        brief.id = "brief-missing-trigger".into();
        brief.turn_id = Some("turn-missing-trigger".into());
        storage.append_brief(&brief).unwrap();

        let mut turn = TurnRecord::new("default", "turn-missing-trigger", 2);
        turn.input_message_ids = vec!["missing-trigger-message".into()];
        turn.produced_brief_ids = vec![brief.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary {
            message_id: Some("missing-trigger-message".into()),
            kind: MessageKind::OperatorPrompt,
            origin: MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            authority_class: AuthorityClass::OperatorInstruction,
            priority: Priority::Normal,
            trigger_kind: None,
            task_id: None,
        });
        storage.append_turn(&turn).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue.".into(),
            },
        );
        let context = build_context(
            &storage,
            &AgentState::new("default"),
            &execution_snapshot_for(&AgentState::new("default")),
            &SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig::default(),
        )
        .unwrap();
        let recent_turns = context
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section")
            .content
            .clone();
        assert!(recent_turns.contains("- Turn turn_index 2:"));
        assert!(recent_turns.contains("  - trigger: trusted operator input"));
        assert!(recent_turns.contains("Rendered without trigger hydration."));
        assert!(recent_turns.contains("brief_ref=brief:brief-missing-trigger"));
    }

    #[test]
    fn recent_turns_hydrates_turn_record_references_outside_recent_windows() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage.write_agent(&AgentState::new("default")).unwrap();
        let runtime_db = RuntimeDb::open_and_migrate(
            storage.runtime_dir().join("state/runtime.sqlite"),
            storage.runtime_dir().join("state/runtime.lock"),
        )
        .unwrap();
        storage
            .enable_scheduler_control_plane_db(runtime_db)
            .unwrap();

        let mut operator = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Hydrate this older turn input by id.".into(),
            },
        );
        operator.turn_id = Some("turn-hydrated".into());
        storage.append_message(&operator).unwrap();

        for idx in 0..4 {
            storage
                .append_message(&MessageEnvelope::new(
                    "default",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: Some("operator:test".into()),
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: format!("newer message {idx}"),
                    },
                ))
                .unwrap();
        }

        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Hydrated brief by turn record id.",
            Some(operator.id.clone()),
            None,
        );
        brief.id = "brief-hydrated".into();
        brief.turn_id = Some("turn-hydrated".into());
        storage.append_brief(&brief).unwrap();

        let tool = ToolExecutionRecord {
            id: "tool-hydrated".to_string(),
            agent_id: "default".to_string(),
            work_item_id: None,
            turn_index: 3,
            turn_id: Some("turn-hydrated".to_string()),
            tool_name: "ExecCommand".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 123,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": "echo hydrated"}),
            output: json!({"exit_code": 0}),
            summary: "hydrated tool execution by turn record id".to_string(),
            invocation_surface: None,
        };
        storage.append_tool_execution(&tool).unwrap();

        for idx in 0..4 {
            storage
                .append_brief(&BriefRecord::new(
                    "default",
                    BriefKind::Result,
                    &format!("newer brief {idx}"),
                    None,
                    None,
                ))
                .unwrap();
        }

        let mut turn = TurnRecord::new("default", "turn-hydrated", 3);
        turn.input_message_ids = vec![operator.id.clone()];
        turn.produced_brief_ids = vec![brief.id.clone()];
        turn.tool_execution_ids = vec![tool.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&operator));
        storage.append_turn(&turn).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue.".into(),
            },
        );
        let context = build_context(
            &storage,
            &AgentState::new("default"),
            &execution_snapshot_for(&AgentState::new("default")),
            &SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 2,
                recent_briefs: 2,
                prompt_budget_estimated_tokens: 8192,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let recent_turns = context
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section")
            .content
            .clone();
        assert!(recent_turns.contains("- Turn turn_index 3:"));
        assert!(recent_turns.contains("Hydrate this older turn input by id."));
        assert!(recent_turns.contains("Hydrated brief by turn record id."));
        assert!(recent_turns.contains("hydrated tool execution by turn record id"));
    }

    #[test]
    fn recent_turns_renders_full_operator_input_with_brief_ref() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let long_tail = "final-token-visible-after-old-preview-limit";
        let operator_text = format!(
            "{} {long_tail}",
            "Please preserve this operator input in recent turns.".repeat(8)
        );

        let operator = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: operator_text.clone(),
            },
        );
        storage.append_message(&operator).unwrap();
        let mut brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Rendered full operator input.",
            Some(operator.id.clone()),
            None,
        );
        brief.id = "brief-full-operator-input".into();
        storage.append_brief(&brief).unwrap();
        let mut turn = TurnRecord::new("default", "turn-full-operator-input", 1);
        turn.input_message_ids = vec![operator.id.clone()];
        turn.produced_brief_ids = vec![brief.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(&operator));
        storage.append_turn(&turn).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:test".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue.".into(),
            },
        );
        let context = build_context(
            &storage,
            &AgentState::new("default"),
            &execution_snapshot_for(&AgentState::new("default")),
            &SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                prompt_budget_estimated_tokens: 8192,
                ..ContextConfig::default()
            },
        )
        .unwrap();
        let recent_turns = context
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section")
            .content
            .clone();

        assert!(recent_turns.contains("  - operator input: "));
        assert!(recent_turns.contains(long_tail));
        assert!(!recent_turns.contains("  - operator asked: "));
        assert!(recent_turns.contains("brief_ref=brief:brief-full-operator-input"));
    }

    #[test]
    fn compaction_adds_summary_when_message_count_exceeds_threshold() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        for idx in 0..6 {
            let msg = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("message-{idx}"),
                },
            );
            storage.append_message(&msg).unwrap();
        }

        let mut session = AgentState::new("default");
        let changed = maybe_compact_agent(
            &storage,
            &mut session,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 4,
                compaction_keep_recent_messages: 2,
                compaction_trigger_estimated_tokens: 4,
                compaction_keep_recent_estimated_tokens: 2,
                ..ContextConfig::default()
            },
        )
        .unwrap();
        assert!(changed);
        assert!(session.context_summary.unwrap().contains("message-0"));
        assert_eq!(session.working_memory.compression_epoch, 1);
    }

    #[test]
    fn structured_working_memory_keeps_compression_epoch_stable_during_legacy_compaction() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        for idx in 0..6 {
            let msg = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("message-{idx}"),
                },
            );
            storage.append_message(&msg).unwrap();
        }

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            objective: Some("ship working memory".into()),
            plan: Some(vec!["[InProgress] keep cache identity stable"].join("\n")),
            ..WorkingMemorySnapshot::default()
        };
        session.working_memory.compression_epoch = 7;

        let changed = maybe_compact_agent(
            &storage,
            &mut session,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 4,
                compaction_keep_recent_messages: 2,
                compaction_trigger_estimated_tokens: 4,
                compaction_keep_recent_estimated_tokens: 2,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        assert!(changed);
        assert_eq!(session.context_summary, None);
        assert_eq!(session.compacted_message_count, 4);
        assert_eq!(session.working_memory.compression_epoch, 7);
    }

    #[test]
    fn context_fingerprint_changes_when_projected_context_lineage_changes() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        for idx in 0..6 {
            let msg = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("message-{idx}"),
                },
            );
            storage.append_message(&msg).unwrap();
        }

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            objective: Some("ship working memory".into()),
            plan: Some(vec!["[InProgress] keep cache identity stable"].join("\n")),
            ..WorkingMemorySnapshot::default()
        };
        session.working_memory.working_memory_revision = 3;
        session.working_memory.compression_epoch = 7;

        let config = ContextConfig {
            recent_messages: 4,
            recent_briefs: 4,
            compaction_trigger_messages: 4,
            compaction_keep_recent_messages: 2,
            compaction_trigger_estimated_tokens: 4,
            compaction_keep_recent_estimated_tokens: 2,
            ..ContextConfig::default()
        };
        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".into(),
            },
        );
        let identity = AgentIdentityView {
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
        };

        maybe_compact_agent(&storage, &mut session, &config).unwrap();
        let first_prompt = build_effective_prompt(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &current_message,
            &config,
            PathBuf::from("/workspace").as_path(),
            PathBuf::from("/tmp/agent-home").as_path(),
            &identity,
            LoadedAgentsMd::default(),
            &crate::types::SkillsRuntimeView::default(),
            &[],
            None,
        )
        .unwrap();

        storage
            .append_message(&MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "message-6".into(),
                },
            ))
            .unwrap();

        let previous_compacted_message_count = session.compacted_message_count;
        maybe_compact_agent(&storage, &mut session, &config).unwrap();
        let second_prompt = build_effective_prompt(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &current_message,
            &config,
            PathBuf::from("/workspace").as_path(),
            PathBuf::from("/tmp/agent-home").as_path(),
            &identity,
            LoadedAgentsMd::default(),
            &crate::types::SkillsRuntimeView::default(),
            &[],
            None,
        )
        .unwrap();

        assert_eq!(session.context_summary, None);
        assert_ne!(
            session.compacted_message_count,
            previous_compacted_message_count
        );
        assert_eq!(session.compacted_message_count, 5);
        assert_eq!(
            first_prompt.cache_identity.agent_id,
            second_prompt.cache_identity.agent_id
        );
        assert_ne!(
            first_prompt.cache_identity.context_fingerprint,
            second_prompt.cache_identity.context_fingerprint
        );
        assert_eq!(
            first_prompt.cache_identity.prompt_cache_key,
            second_prompt.cache_identity.prompt_cache_key
        );
        assert_eq!(second_prompt.cache_identity.compression_epoch, 7);
    }

    #[test]
    fn build_context_folds_latest_result_into_recent_turns_and_keeps_generic_context_contract() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut prior_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "fix the failing benchmark".to_string(),
            },
        );
        prior_message.turn_id = Some("turn-benchmark".to_string());
        storage.append_message(&prior_message).unwrap();

        let result_brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Updated benchmark summary reporting and verified cargo test.",
            Some(prior_message.id.clone()),
            None,
        );
        storage.append_brief(&result_brief).unwrap();

        let tool_record = ToolExecutionRecord {
            id: "tool-1".to_string(),
            agent_id: "default".to_string(),
            work_item_id: Some("work_123".to_string()),
            turn_index: 0,
            turn_id: Some("turn-benchmark".to_string()),
            tool_name: "ExecCommand".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 123,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": "cargo test"}),
            output: json!({"exit_code": 0}),
            summary: "Verified with cargo test".to_string(),
            invocation_surface: None,
        };
        storage.append_tool_execution(&tool_record).unwrap();

        let mut turn = TurnRecord::new("default", "turn-benchmark", 1);
        turn.input_message_ids = vec![prior_message.id.clone()];
        turn.produced_brief_ids = vec![result_brief.id.clone()];
        turn.tool_execution_ids = vec![tool_record.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
            &prior_message,
        ));
        storage.append_turn(&turn).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "What changed and how did you verify it?".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.total_model_rounds = 1;
        session.total_message_count = 2;
        session.last_brief_at = Some(chrono::Utc::now());

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                prompt_budget_estimated_tokens: 8192,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let recent_turns = built
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section should be present");
        assert!(recent_turns
            .content
            .contains("Updated benchmark summary reporting"));
        assert!(recent_turns.content.contains("Verified with cargo test"));
        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "latest_result"));

        let context_contract = built
            .sections
            .iter()
            .find(|section| section.name == "context_contract")
            .expect("context contract section should be present");
        assert!(context_contract
            .content
            .contains("Use prior briefs and recent tool results"));
        assert!(context_contract
            .content
            .contains("current work item objective first"));
    }

    #[test]
    fn build_context_does_not_render_recent_turns_from_orphan_recent_evidence() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        storage
            .append_message(&MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "Patch the prompt context fallback.".to_string(),
                },
            ))
            .unwrap();

        storage
            .append_brief(&BriefRecord::new(
                "default",
                BriefKind::Result,
                "Orphan result brief should still be visible.",
                None,
                None,
            ))
            .unwrap();

        let tool_record = ToolExecutionRecord {
            id: "tool-orphan".to_string(),
            agent_id: "default".to_string(),
            work_item_id: Some("work_orphan".to_string()),
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommand".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 123,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": "cargo test context::tests::orphan_recent_evidence"}),
            output: json!({"exit_code": 0}),
            summary: "verified orphan recent evidence fallback".to_string(),
            invocation_surface: None,
        };
        storage.append_tool_execution(&tool_record).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".to_string(),
            },
        );

        let session = AgentState::new("default");
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                prompt_budget_estimated_tokens: 8192,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "recent_turns"));
        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "latest_result"));
        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "recent_tool_executions"));
    }

    #[test]
    fn recent_tool_context_includes_recoverable_command_receipt_ref() {
        let command = "python - <<'PY'\nprint('context_receipt_middle_1246')\nPY";
        let record = ToolExecutionRecord {
            id: "tool-context-1246".to_string(),
            agent_id: "default".to_string(),
            work_item_id: Some("work_123".to_string()),
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommand".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 123,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": command}),
            output: json!({
                "exit_code": 0,
                "stdout_preview": "context_stdout_1246\n",
                "stderr_preview": "",
                "artifacts": []
            }),
            summary: "command exited with status 0".to_string(),
            invocation_surface: None,
        };

        let rendered = render_recent_tool_execution(&record);

        assert!(rendered.contains("tool_execution_id=tool-context-1246"));
        assert!(rendered.contains("cmd_ref=tool_execution:tool-context-1246:cmd"));
        assert!(rendered.contains("stdout_ref=tool_execution:tool-context-1246:stdout"));
        assert!(rendered.contains("stderr_ref=tool_execution:tool-context-1246:stderr"));
        assert!(rendered.contains("output_ref=tool_execution:tool-context-1246:output"));
        assert!(rendered.contains("cmd_digest="));
        assert!(rendered.contains("[omitted: command contains heredoc or inline script]"));
        assert!(!rendered.contains("context_receipt_middle_1246"));
    }

    #[test]
    fn recent_tool_context_includes_batch_item_receipt_refs() {
        let record = ToolExecutionRecord {
            id: "tool-context-batch-1246".to_string(),
            agent_id: "default".to_string(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommandBatch".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 123,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({
                "items": [
                    {"cmd": "rg -n \"foo\" src"},
                    {"cmd": "node - <<'NODE'\nconsole.log('hidden_batch_1246')\nNODE"}
                ]
            }),
            output: json!({
                "result": {
                    "items": [
                        {"result": {"stdout_preview": "foo\n", "stderr_preview": "", "artifacts": []}},
                        {"result": {"stdout_preview": "hidden_batch_1246\n", "stderr_preview": "", "artifacts": []}}
                    ]
                }
            }),
            summary: "ExecCommandBatch completed 2/2 items".to_string(),
            invocation_surface: None,
        };

        let rendered = render_recent_tool_execution(&record);

        assert!(rendered.contains("tool_execution_id=tool-context-batch-1246"));
        assert!(
            rendered.contains("cmd_ref=tool_execution:tool-context-batch-1246:batch_item:1:cmd")
        );
        assert!(
            rendered.contains("cmd_ref=tool_execution:tool-context-batch-1246:batch_item:2:cmd")
        );
        assert!(rendered
            .contains("stdout_ref=tool_execution:tool-context-batch-1246:batch_item:1:stdout"));
        assert!(rendered
            .contains("stderr_ref=tool_execution:tool-context-batch-1246:batch_item:2:stderr"));
        assert!(rendered
            .contains("output_ref=tool_execution:tool-context-batch-1246:batch_item:2:output"));
        assert!(rendered.contains("cmd_digest="));
        assert!(!rendered.contains("hidden_batch_1246"));
    }

    #[test]
    fn recent_tool_context_omits_output_refs_without_output_evidence() {
        let record = ToolExecutionRecord {
            id: "tool-context-old-1246".to_string(),
            agent_id: "default".to_string(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommand".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 123,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": "echo old_context_1246"}),
            output: json!({"exit_code": 0}),
            summary: "command exited with status 0".to_string(),
            invocation_surface: None,
        };

        let rendered = render_recent_tool_execution(&record);

        assert!(rendered.contains("cmd_ref=tool_execution:tool-context-old-1246:cmd"));
        assert!(!rendered.contains("stdout_ref="));
        assert!(!rendered.contains("stderr_ref="));
        assert!(!rendered.contains("output_ref="));
    }

    #[test]
    fn build_context_folds_briefs_into_recent_turns_without_parallel_brief_section() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let prior_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "previous request".to_string(),
            },
        );
        storage.append_message(&prior_message).unwrap();
        let ack = BriefRecord::new(
            "default",
            BriefKind::Ack,
            "Acknowledged the request.",
            Some(prior_message.id.clone()),
            None,
        );
        storage.append_brief(&ack).unwrap();
        let result = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Unique latest result content.",
            Some(prior_message.id.clone()),
            None,
        );
        storage.append_brief(&result).unwrap();
        let mut turn = TurnRecord::new("default", "turn-previous", 1);
        turn.input_message_ids = vec![prior_message.id.clone()];
        turn.produced_brief_ids = vec![ack.id.clone(), result.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
            &prior_message,
        ));
        storage.append_turn(&turn).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".to_string(),
            },
        );

        let session = AgentState::new("default");
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                prompt_budget_estimated_tokens: 8192,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let recent_turns = built
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section should be present");
        assert!(recent_turns
            .content
            .contains("Unique latest result content."));
        assert!(recent_turns.content.contains("Acknowledged the request."));
        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "recent_briefs"));
        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "latest_result"));
    }

    #[test]
    fn build_context_skips_messages_covered_by_compacted_summary() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        for idx in 0..20 {
            let message = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("message-{idx}"),
                },
            );
            storage.append_message(&message).unwrap();
            if idx >= 12 {
                append_turn_for_message(
                    &storage,
                    &message,
                    &format!("turn-message-{idx}"),
                    idx + 1,
                );
            }
        }

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".to_string(),
            },
        );
        let mut session = AgentState::new("default");
        session.compacted_message_count = 12;
        session.context_summary =
            Some("Compacted messages include message-8 and message-11".into());

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 12,
                recent_briefs: 4,
                prompt_budget_estimated_tokens: 8192,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let recent_turns = built
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section should be present");
        assert!(!recent_turns.content.contains("message-8"));
        assert!(!recent_turns.content.contains("message-11"));
        assert!(recent_turns.content.contains("message-12"));
        assert!(recent_turns.content.contains("message-19"));
    }

    #[test]
    fn maybe_compact_agent_persists_message_count_without_compaction() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_message(&MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "short".into(),
                },
            ))
            .unwrap();

        let mut session = AgentState::new("default");
        let changed = maybe_compact_agent(
            &storage,
            &mut session,
            &ContextConfig {
                compaction_trigger_estimated_tokens: 10_000,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        assert!(changed);
        assert_eq!(session.total_message_count, 1);
        assert_eq!(session.context_summary, None);
        assert_eq!(session.compacted_message_count, 0);
    }

    #[test]
    fn maybe_compact_agent_clears_legacy_summary_when_working_memory_is_active() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        for idx in 0..6 {
            storage
                .append_message(&MessageEnvelope::new(
                    "default",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator { actor_id: None },
                    AuthorityClass::OperatorInstruction,
                    Priority::Normal,
                    MessageBody::Text {
                        text: format!("message-{idx}"),
                    },
                ))
                .unwrap();
        }

        let mut session = AgentState::new("default");
        session.context_summary = Some("stale compacted summary".into());
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            objective: Some("active work".into()),
            ..WorkingMemorySnapshot::default()
        };
        let changed = maybe_compact_agent(
            &storage,
            &mut session,
            &ContextConfig {
                compaction_trigger_estimated_tokens: 4,
                compaction_keep_recent_messages: 2,
                compaction_keep_recent_estimated_tokens: 2,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        assert!(changed);
        assert_eq!(session.context_summary, None);
        assert!(session.compacted_message_count > 0);
    }

    #[test]
    fn build_context_prefers_structured_working_memory_and_turn_delta() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.context_summary = Some("legacy summary".into());
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            objective: Some("ship working memory".into()),
            plan: Some(vec!["[InProgress] wire post-turn refresh"].join("\n")),
            ..WorkingMemorySnapshot::default()
        };
        session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
            from_revision: 0,
            to_revision: 1,
            created_at_turn: 1,
            reason: crate::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
            changed_fields: vec!["plan".into()],
            summary_lines: vec!["updated plan: [InProgress] wire post-turn refresh".into()],
        });

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        assert!(built
            .sections
            .iter()
            .any(|section| section.name == "working_memory"));
        assert!(built
            .sections
            .iter()
            .any(|section| section.name == "working_memory_delta"));
        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "compacted_summary"));
    }

    #[test]
    fn build_context_keeps_verification_as_raw_evidence_without_promoting_session_fact() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut prior_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "check whether the wake path is stable".to_string(),
            },
        );
        prior_message.turn_id = Some("turn-wake-path".to_string());
        storage.append_message(&prior_message).unwrap();

        let brief = BriefRecord::new(
            "default",
            BriefKind::Failure,
            "Tried cargo test wake_path, but the result is still inconclusive.",
            Some(prior_message.id.clone()),
            None,
        );
        storage.append_brief(&brief).unwrap();
        let tool = ToolExecutionRecord {
            id: "tool-raw-evidence".to_string(),
            agent_id: "default".to_string(),
            work_item_id: None,
            turn_index: 0,
            turn_id: Some("turn-wake-path".to_string()),
            tool_name: "ExecCommand".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 42,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": "cargo test wake_path"}),
            output: json!({"exit_code": 1}),
            summary: "cargo test wake_path still flakes under load".to_string(),
            invocation_surface: None,
        };
        storage.append_tool_execution(&tool).unwrap();
        let mut turn = TurnRecord::new("default", "turn-wake-path", 1);
        turn.input_message_ids = vec![prior_message.id.clone()];
        turn.produced_brief_ids = vec![brief.id.clone()];
        turn.tool_execution_ids = vec![tool.id.clone()];
        turn.trigger = Some(crate::types::TurnTriggerSummary::from_message(
            &prior_message,
        ));
        storage.append_turn(&turn).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Summarize the current state.".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            objective: Some("stabilize wake path".into()),
            recent_decisions: vec!["leave flaky verification as raw evidence".into()],
            ..WorkingMemorySnapshot::default()
        };

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let working_memory = built
            .sections
            .iter()
            .find(|section| section.name == "working_memory")
            .expect("working memory section should be present");
        assert!(!working_memory
            .content
            .contains("leave flaky verification as raw evidence"));
        assert!(!working_memory.content.contains("Latest verified result"));
        assert!(!working_memory.content.contains("cargo test wake_path"));

        let recent_turns = built
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section should be present");
        assert!(recent_turns.content.contains("still inconclusive"));
        assert!(recent_turns.content.contains("cargo test wake_path"));
        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "recent_briefs"));
        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "recent_tool_executions"));
    }

    #[test]
    fn build_context_does_not_emit_follow_up_specific_contracts() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Analyze the runtime architecture and suggest next steps.".to_string(),
            },
        );

        let session = AgentState::new("default");
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        assert!(built
            .sections
            .iter()
            .all(|section| section.name != "follow_up_context_contract"));

        let context_contract = built
            .sections
            .iter()
            .find(|section| section.name == "context_contract")
            .expect("context contract section should be present");
        assert!(context_contract
            .content
            .contains("Use prior briefs and recent tool results"));
        let context_contract_index = built
            .sections
            .iter()
            .position(|section| section.name == "context_contract")
            .expect("context contract index should be present");
        let current_input_index = built
            .sections
            .iter()
            .position(|section| section.name == "current_input")
            .expect("current input index should be present");
        assert!(context_contract_index < current_input_index);
    }

    #[test]
    fn build_context_lists_skill_metadata_without_skill_body() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Use the demo skill".to_string(),
            },
        );

        let session = AgentState::new("default");
        let skills = SkillsRuntimeView {
            agent_templates_catalog: Vec::new(),
            discovered_roots: Vec::new(),
            discoverable_skills: vec![crate::types::SkillCatalogEntry {
                skill_id: "workspace:demo".into(),
                name: "demo".into(),
                description: "demo skill summary".into(),
                path: PathBuf::from("/tmp/workspace/.agents/skills/demo/SKILL.md"),
                scope: crate::types::SkillScope::Workspace,
            }],
            attached_skills: Vec::new(),
            active_skills: vec![crate::types::ActiveSkillRecord {
                skill_id: "workspace:demo".into(),
                name: "demo".into(),
                path: PathBuf::from("/tmp/workspace/.agents/skills/demo/SKILL.md"),
                scope: crate::types::SkillScope::Workspace,
                agent_id: "default".into(),
                activation_source: crate::types::SkillActivationSource::ImplicitFromCatalog,
                activation_state: crate::types::SkillActivationState::SessionActive,
                activated_at_turn: 2,
            }],
        };

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &skills,
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let catalog = built
            .sections
            .iter()
            .find(|section| section.name == "skills_catalog")
            .expect("skills_catalog section should be present");
        assert!(catalog.content.contains("demo skill summary"));
        assert!(catalog
            .content
            .contains("/tmp/workspace/.agents/skills/demo/SKILL.md"));
        assert!(!catalog.content.contains("Follow the demo workflow."));

        let active = built
            .sections
            .iter()
            .find(|section| section.name == "active_skills")
            .expect("active_skills section should be present");
        assert!(active.content.contains("workspace:demo"));
        assert!(active.content.contains("session_active"));
    }

    #[test]
    fn build_context_lists_agent_template_catalog_without_template_body() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Use a helper template".to_string(),
            },
        );

        let session = AgentState::new("default");
        let skills = SkillsRuntimeView {
            agent_templates_catalog: vec![crate::types::AgentTemplateCatalogEntry {
                catalog_id: "builtin:demo".into(),
                template: "demo".into(),
                template_id: "demo".into(),
                source: crate::types::AgentTemplateSourceKind::Builtin,
                path: None,
                description: "demo template summary".into(),
                included_skills: vec!["ghx".into()],
            }],
            ..SkillsRuntimeView::default()
        };

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &skills,
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let catalog = built
            .sections
            .iter()
            .find(|section| section.name == "agent_templates_catalog")
            .expect("agent_templates_catalog section should be present");
        assert!(catalog.content.contains("builtin:demo"));
        assert!(catalog.content.contains("template=demo"));
        assert!(catalog.content.contains("demo template summary"));
        assert!(catalog.content.contains("skills=ghx"));
    }

    #[test]
    fn build_context_includes_worktree_session_when_active() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Make changes in the worktree".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.worktree_session = Some(crate::types::WorktreeSession {
            original_cwd: PathBuf::from("/original/repo"),
            original_branch: "main".to_string(),
            worktree_path: PathBuf::from("/tmp/worktree-feature-branch"),
            worktree_branch: "feature-branch".to_string(),
        });

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let worktree_section = built
            .sections
            .iter()
            .find(|section| section.name == "worktree_session")
            .expect("worktree_session section should be present when active");

        assert!(worktree_section.content.contains("Managed worktree active"));
        assert!(worktree_section.content.contains("/original/repo"));
        assert!(worktree_section.content.contains("main"));
        assert!(worktree_section
            .content
            .contains("/tmp/worktree-feature-branch"));
        assert!(worktree_section.content.contains("feature-branch"));
    }

    #[test]
    fn build_context_includes_current_work_item_and_plan_sections() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue the implementation".to_string(),
            },
        );

        let mut active = crate::types::WorkItemRecord::new(
            "default",
            "storage and recovery foundation",
            crate::types::WorkItemState::Open,
        );
        active.plan_artifact = Some(
            crate::work_item_plan::ensure_plan_artifact(
                dir.path(),
                &active,
                Some("Persist the work item store and project it into prompts."),
            )
            .unwrap(),
        );
        active.todo_list = vec![
            TodoItem {
                text: "Persist work-item store".into(),
                state: TodoItemState::Completed,
            },
            TodoItem {
                text: "Project active item into prompt".into(),
                state: TodoItemState::InProgress,
            },
        ];
        storage.append_work_item(&active).unwrap();
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: "wait-current".into(),
                agent_id: "default".into(),
                scope: WaitingIntentScope::WorkItem,
                work_item_id: Some(active.id.clone()),
                description: "wait for CI webhook".into(),
                source: "github".into(),
                resource: Some("pull/1099".into()),
                condition: Some("ci completed".into()),
                delivery_mode: CallbackDeliveryMode::EnqueueMessage,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb-current".into(),
                created_at: chrono::Utc::now(),
                cancelled_at: None,
                last_triggered_at: Some(chrono::Utc::now()),
                trigger_count: 1,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: "wait-other-agent".into(),
                agent_id: "other-agent".into(),
                scope: WaitingIntentScope::WorkItem,
                work_item_id: Some(active.id.clone()),
                description: "other agent wait must not leak".into(),
                source: "github".into(),
                resource: Some("pull/other".into()),
                condition: Some("ci completed".into()),
                delivery_mode: CallbackDeliveryMode::EnqueueMessage,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb-other".into(),
                created_at: chrono::Utc::now(),
                cancelled_at: None,
                last_triggered_at: Some(chrono::Utc::now()),
                trigger_count: 1,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();
        storage
            .append_brief(&BriefRecord {
                work_item_id: Some(active.id.clone()),
                ..BriefRecord::new(
                    "default",
                    BriefKind::Result,
                    "Bound brief captured current WorkItem evidence.",
                    None,
                    None,
                )
            })
            .unwrap();
        storage
            .append_brief(&BriefRecord {
                agent_id: "other-agent".into(),
                work_item_id: Some(active.id.clone()),
                ..BriefRecord::new(
                    "default",
                    BriefKind::Result,
                    "Other agent brief must not leak.",
                    None,
                    None,
                )
            })
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-current".into(),
                agent_id: "default".into(),
                work_item_id: Some(active.id.clone()),
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommand".into(),
                created_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                duration_ms: 10,
                authority_class: AuthorityClass::OperatorInstruction,
                status: ToolExecutionStatus::Success,
                input: json!({"cmd": "cargo test -p holon context"}),
                output: json!({}),
                summary: "Verified current WorkItem projection".into(),
                invocation_surface: None,
            })
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-other-agent".into(),
                agent_id: "other-agent".into(),
                work_item_id: Some(active.id.clone()),
                turn_index: 0,
                turn_id: None,
                tool_name: "ExecCommand".into(),
                created_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                duration_ms: 10,
                authority_class: AuthorityClass::OperatorInstruction,
                status: ToolExecutionStatus::Success,
                input: json!({"cmd": "echo other"}),
                output: json!({}),
                summary: "Other agent tool must not leak".into(),
                invocation_surface: None,
            })
            .unwrap();
        storage
            .append_transcript_entry(&TranscriptEntry::new(
                "default",
                TranscriptEntryKind::AssistantRound,
                Some(1),
                None,
                json!({
                    "work_item_id": active.id.clone(),
                    "blocks": [
                        {"type": "thinking", "text": "Provider thinking must not leak."},
                        {"type": "text", "text": "Assistant summarized current WorkItem progress."}
                    ]
                }),
            ))
            .unwrap();
        storage
            .append_transcript_entry(&TranscriptEntry::new(
                "other-agent",
                TranscriptEntryKind::AssistantRound,
                Some(1),
                None,
                json!({
                    "work_item_id": active.id.clone(),
                    "blocks": [{"type": "text", "text": "Other agent transcript must not leak."}]
                }),
            ))
            .unwrap();

        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some(active.id.clone());
        storage.write_agent(&agent).unwrap();
        let built = build_context(
            &storage,
            &agent,
            &execution_snapshot_for(&agent),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let active_section = built
            .sections
            .iter()
            .find(|section| section.name == "current_work_item")
            .expect("current_work_item section should be present");
        assert!(active_section
            .content
            .contains("storage and recovery foundation"));
        assert!(active_section.content.contains("Plan artifact:"));
        assert!(active_section.content.contains("work-items"));
        assert!(active_section
            .content
            .contains("Plan preview complete: true"));
        assert!(active_section
            .content
            .contains("Persist the work item store and project it into prompts."));
        assert!(active_section
            .content
            .contains("Project active item into prompt"));
        assert!(active_section.content.contains("Active waits:"));
        assert!(active_section.content.contains("wait for CI webhook"));
        assert!(!active_section
            .content
            .contains("other agent wait must not leak"));

        let trace_section = built
            .sections
            .iter()
            .find(|section| section.name == "current_work_item_process_trace")
            .expect("current_work_item_process_trace section should be present");
        assert!(trace_section
            .content
            .contains("Bound brief captured current WorkItem evidence"));
        assert!(trace_section
            .content
            .contains("Verified current WorkItem projection"));
        assert!(trace_section
            .content
            .contains("Assistant summarized current WorkItem progress"));
        assert!(!trace_section
            .content
            .contains("Provider thinking must not leak"));
        assert!(!trace_section
            .content
            .contains("Other agent brief must not leak"));
        assert!(!trace_section
            .content
            .contains("Other agent tool must not leak"));
        assert!(!trace_section
            .content
            .contains("Other agent transcript must not leak"));
    }

    #[test]
    fn build_context_includes_ranked_work_item_candidates_and_completion_reports() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue the implementation".to_string(),
            },
        );

        let mut triggered = crate::types::WorkItemRecord::new(
            "default",
            "Resume triggered CI follow-up",
            crate::types::WorkItemState::Open,
        );
        triggered.blocked_by = Some("waiting for CI".into());
        triggered.plan_artifact = Some(
            crate::work_item_plan::ensure_plan_artifact(
                dir.path(),
                &triggered,
                Some("Handle the triggered CI result."),
            )
            .unwrap(),
        );
        let mut queued = crate::types::WorkItemRecord::new(
            "default",
            "Queue follow-up verification",
            crate::types::WorkItemState::Open,
        );
        queued.plan_artifact = Some(
            crate::work_item_plan::ensure_plan_artifact(
                dir.path(),
                &queued,
                Some(&format!(
                    "Verify the queued path.\n{}",
                    "queued detail ".repeat(200)
                )),
            )
            .unwrap(),
        );
        let mut waiting = crate::types::WorkItemRecord::new(
            "default",
            "Wait for operator confirmation",
            crate::types::WorkItemState::Open,
        );
        waiting.plan_status = crate::types::WorkItemPlanStatus::NeedsInput;
        waiting.plan_artifact = Some(
            crate::work_item_plan::ensure_plan_artifact(
                dir.path(),
                &waiting,
                Some("Wait for the operator answer before retrying."),
            )
            .unwrap(),
        );
        let mut completed = crate::types::WorkItemRecord::new(
            "default",
            "Already finished item",
            crate::types::WorkItemState::Completed,
        );
        completed.result_summary = Some("Promoted completion report only.".into());
        storage.append_work_item(&triggered).unwrap();
        storage.append_work_item(&queued).unwrap();
        storage.append_work_item(&waiting).unwrap();
        storage.append_work_item(&completed).unwrap();
        storage
            .append_waiting_intent(&WaitingIntentRecord {
                id: "wait-triggered".into(),
                agent_id: "default".into(),
                scope: WaitingIntentScope::WorkItem,
                work_item_id: Some(triggered.id.clone()),
                description: "CI completed".into(),
                source: "github".into(),
                resource: Some("pull/1099".into()),
                condition: Some("ci completed".into()),
                delivery_mode: CallbackDeliveryMode::EnqueueMessage,
                status: WaitingIntentStatus::Active,
                external_trigger_id: "cb-triggered".into(),
                created_at: chrono::Utc::now(),
                cancelled_at: None,
                last_triggered_at: Some(chrono::Utc::now()),
                trigger_count: 1,
                correlation_id: None,
                causation_id: None,
            })
            .unwrap();

        let built = build_context(
            &storage,
            &AgentState::new("default"),
            &execution_snapshot_for(&AgentState::new("default")),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let summary = built
            .sections
            .iter()
            .find(|section| section.name == "queued_blocked_work_items")
            .expect("queued_blocked_work_items section should be present");
        assert!(summary
            .content
            .contains("Work item candidates by scheduler ranking:"));
        assert!(summary.content.contains("Triggered work items:"));
        assert!(summary.content.contains("Resume triggered CI follow-up"));
        assert!(summary.content.contains("[triggered_blocked]"));
        assert!(summary.content.contains("Queued runnable work items:"));
        assert!(summary.content.contains("Queue follow-up verification"));
        assert!(summary.content.contains("[queued_runnable]"));
        assert!(summary.content.contains("Waiting for operator:"));
        assert!(summary.content.contains("Wait for operator confirmation"));
        assert!(summary.content.contains("[waiting_for_operator]"));
        assert!(summary.content.contains("Recently completed work items:"));
        assert!(summary.content.contains("Already finished item"));
        assert!(summary
            .content
            .contains("Completion report: Promoted completion report only."));
        assert!(summary.content.contains("Plan artifact:"));
        assert!(summary.content.contains("Plan preview:"));
        assert!(summary.content.contains("Verify the queued path."));
        assert!(summary.content.contains("Plan preview complete: false"));
        assert!(summary.content.contains("Plan preview complete: true"));
    }

    #[test]
    fn build_context_omits_worktree_session_when_not_active() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Make changes".to_string(),
            },
        );

        let session = AgentState::new("default");

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        assert!(built
            .sections
            .iter()
            .all(|section| section.name != "worktree_session"));
    }

    #[test]
    fn build_context_includes_execution_environment() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Inspect the environment".to_string(),
            },
        );
        let session = AgentState::new("default");

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let execution_section = built
            .sections
            .iter()
            .find(|section| section.name == "execution_environment")
            .expect("execution_environment section should be present");
        assert!(execution_section
            .content
            .contains("policy snapshot; host-local is not a strong sandbox guarantee"));
        assert!(execution_section
            .content
            .contains("Process execution exposed: true"));
        assert!(execution_section
            .content
            .contains("Background tasks supported: true"));
        assert!(execution_section
            .content
            .contains("Managed worktrees supported: true"));
        assert!(execution_section
            .content
            .contains("path_confinement: not_enforced"));
        assert!(execution_section
            .content
            .contains("write_confinement: not_enforced"));
        assert!(execution_section
            .content
            .contains("network_confinement: not_enforced"));
        assert!(execution_section
            .content
            .contains("secret_isolation: not_enforced"));
        assert!(execution_section
            .content
            .contains("child_process_containment: not_enforced"));
    }

    #[test]
    fn build_context_includes_default_external_wake_ingress() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        storage
            .append_external_trigger(&ExternalTriggerRecord {
                external_trigger_id: "wake-default".into(),
                target_agent_id: "default".into(),
                waiting_intent_id: None,
                scope: ExternalTriggerScope::Agent,
                delivery_mode: CallbackDeliveryMode::WakeHint,
                trigger_url: Some("http://127.0.0.1:7878/callbacks/wake/token".into()),
                token_hash: "redacted-token-hash".into(),
                status: ExternalTriggerStatus::Active,
                created_at: chrono::Utc::now(),
                revoked_at: None,
                last_delivered_at: None,
                delivery_count: 0,
            })
            .unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Inspect ingress".to_string(),
            },
        );
        let session = AgentState::new("default");

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let ingress = built
            .sections
            .iter()
            .find(|section| section.name == "default_external_ingress")
            .expect("default external ingress section should be present");
        assert!(ingress
            .content
            .contains("http://127.0.0.1:7878/callbacks/wake/token"));
        assert!(ingress.content.contains("- mode: wake_hint"));
        assert!(ingress.content.contains("- status: active"));
        assert!(ingress.content.contains("capability_secret: true"));
    }

    #[test]
    fn build_context_uses_latest_default_external_wake_ingress() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let now = chrono::Utc::now();
        storage
            .append_external_trigger(&ExternalTriggerRecord {
                external_trigger_id: "aaa-old-wake".into(),
                target_agent_id: "default".into(),
                waiting_intent_id: None,
                scope: ExternalTriggerScope::Agent,
                delivery_mode: CallbackDeliveryMode::WakeHint,
                trigger_url: Some("http://127.0.0.1:7878/callbacks/wake/old".into()),
                token_hash: "redacted-old-token-hash".into(),
                status: ExternalTriggerStatus::Active,
                created_at: now - chrono::Duration::seconds(60),
                revoked_at: None,
                last_delivered_at: None,
                delivery_count: 0,
            })
            .unwrap();
        storage
            .append_external_trigger(&ExternalTriggerRecord {
                external_trigger_id: "zzz-new-wake".into(),
                target_agent_id: "default".into(),
                waiting_intent_id: None,
                scope: ExternalTriggerScope::Agent,
                delivery_mode: CallbackDeliveryMode::WakeHint,
                trigger_url: Some("http://127.0.0.1:7878/callbacks/wake/new".into()),
                token_hash: "redacted-new-token-hash".into(),
                status: ExternalTriggerStatus::Active,
                created_at: now,
                revoked_at: None,
                last_delivered_at: None,
                delivery_count: 0,
            })
            .unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Inspect ingress".to_string(),
            },
        );
        let session = AgentState::new("default");

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let ingress = built
            .sections
            .iter()
            .find(|section| section.name == "default_external_ingress")
            .expect("default external ingress section should be present");
        assert!(ingress
            .content
            .contains("http://127.0.0.1:7878/callbacks/wake/new"));
        assert!(!ingress
            .content
            .contains("http://127.0.0.1:7878/callbacks/wake/old"));
    }

    #[test]
    fn build_context_omits_system_tick_from_recent_turns() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let operator_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "hello".to_string(),
            },
        );
        storage.append_message(&operator_message).unwrap();
        append_turn_for_message(&storage, &operator_message, "turn-operator-hello", 1);

        let system_tick = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "wake_hint".to_string(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "wake hint: changed".to_string(),
            },
        );
        storage.append_message(&system_tick).unwrap();

        let session = AgentState::new("default");
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &operator_message,
            None,
            &ContextConfig {
                recent_messages: 10,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let recent_turns = built
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section should be present");
        assert!(recent_turns.content.contains("hello"));
        assert!(!recent_turns.content.contains("SystemTick"));
        assert!(!recent_turns.content.contains("wake hint: changed"));
    }

    #[test]
    fn build_context_includes_continuation_context_for_system_tick() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut system_tick = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "wake_hint".to_string(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "wake hint: github inbox updated".to_string(),
            },
        );
        system_tick.metadata = Some(json!({
            "wake_hint": {
                "reason": "github inbox updated",
                "source": "agentinbox",
                "resource": "interest/pr-reviews",
                "content_type": "application/json",
                "body": {
                    "type": "json",
                    "value": {
                        "notification_type": "pr_review_requested",
                        "pr": 123
                    }
                }
            }
        }));

        let session = AgentState::new("default");
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &system_tick,
            Some(&ContinuationResolution {
                trigger_kind: crate::types::ContinuationTriggerKind::SystemTick,
                class: ContinuationClass::ResumeExpectedWait,
                model_reentry: true,
                prior_closure_outcome: crate::types::ClosureOutcome::Waiting,
                prior_waiting_reason: Some(crate::types::WaitingReason::AwaitingExternalChange),
                matched_waiting_reason: true,
                evidence: vec![],
            }),
            &ContextConfig {
                recent_messages: 10,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let activation = built
            .sections
            .iter()
            .find(|section| section.name == "continuation_context")
            .expect("continuation_context section should be present");
        assert!(activation.content.contains("agentinbox"));
        assert!(activation.content.contains("interest/pr-reviews"));
        assert!(activation.content.contains("pr_review_requested"));
        assert!(activation.content.contains("\"pr\": 123"));
    }

    #[test]
    fn build_context_includes_continuation_context_for_work_queue_system_tick() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut system_tick = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "work_queue".to_string(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue current work item: fix stale pid handling".to_string(),
            },
        );
        system_tick.metadata = Some(json!({
            "work_queue": {
                "reason": "continue_active",
                "work_item_id": "work_123",
                "objective": "fix stale pid handling",
                "runtime_switched_current_item": false
            }
        }));

        let session = AgentState::new("default");
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &system_tick,
            Some(&ContinuationResolution {
                trigger_kind: crate::types::ContinuationTriggerKind::SystemTick,
                class: ContinuationClass::LocalContinuation,
                model_reentry: true,
                prior_closure_outcome: crate::types::ClosureOutcome::Completed,
                prior_waiting_reason: None,
                matched_waiting_reason: false,
                evidence: vec![],
            }),
            &ContextConfig {
                recent_messages: 10,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let activation = built
            .sections
            .iter()
            .find(|section| section.name == "continuation_context")
            .expect("continuation_context section should be present");
        assert!(activation.content.contains("continue_active"));
        assert!(activation.content.contains("work_123"));
        assert!(activation.content.contains("fix stale pid handling"));
    }

    #[test]
    fn build_context_anchors_latest_operator_input_for_runtime_continuation_without_work_item() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let operator_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "请实现 continuation anchor，并保持中文回复。".to_string(),
            },
        );
        storage.append_message(&operator_message).unwrap();

        let mut system_tick = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "recovery".to_string(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "runtime recovery after provider fallback".to_string(),
            },
        );
        system_tick.trigger_kind = Some(ContinuationTriggerKind::SystemTick);

        let session = AgentState::new("default");
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &system_tick,
            Some(&ContinuationResolution {
                trigger_kind: ContinuationTriggerKind::SystemTick,
                class: ContinuationClass::LocalContinuation,
                model_reentry: true,
                prior_closure_outcome: crate::types::ClosureOutcome::Continuable,
                prior_waiting_reason: None,
                matched_waiting_reason: false,
                evidence: vec![],
            }),
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let anchor = built
            .sections
            .iter()
            .find(|section| section.name == "continuation_anchor")
            .expect("continuation_anchor section should be present");
        assert!(anchor
            .content
            .contains("请实现 continuation anchor，并保持中文回复。"));
        assert!(anchor.content.contains("runtime system-tick continuation"));
        assert!(anchor.content.contains("not a new operator request"));
    }

    #[test]
    fn build_context_anchor_uses_operator_input_beyond_recent_window() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let operator_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Preserve this trimmed operator intent.".to_string(),
            },
        );
        storage.append_message(&operator_message).unwrap();

        for idx in 0..6 {
            let runtime_message = MessageEnvelope::new(
                "default",
                MessageKind::TaskStatus,
                MessageOrigin::System {
                    subsystem: "task".to_string(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("runtime status {idx}"),
                },
            );
            storage.append_message(&runtime_message).unwrap();
            append_turn_for_message(
                &storage,
                &runtime_message,
                &format!("turn-runtime-status-{idx}"),
                idx + 2,
            );
        }

        let mut recovery = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "recovery".to_string(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "runtime recovery after context trimming".to_string(),
            },
        );
        recovery.trigger_kind = Some(ContinuationTriggerKind::SystemTick);

        let mut session = AgentState::new("default");
        session.compacted_message_count = 4;
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &recovery,
            None,
            &ContextConfig {
                recent_messages: 2,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 2,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let anchor = built
            .sections
            .iter()
            .find(|section| section.name == "continuation_anchor")
            .expect("continuation_anchor section should be present");
        assert!(anchor
            .content
            .contains("Preserve this trimmed operator intent."));

        let recent_turns = built
            .sections
            .iter()
            .find(|section| section.name == "recent_turns")
            .expect("recent_turns section should be present");
        assert!(!recent_turns
            .content
            .contains("Preserve this trimmed operator intent."));
    }

    #[test]
    fn build_context_work_item_anchor_does_not_duplicate_work_item_projection() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let operator_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue the durable work item instead of starting over.".to_string(),
            },
        );
        storage.append_message(&operator_message).unwrap();

        let active = crate::types::WorkItemRecord::new(
            "default",
            "Implement continuation anchor runtime behavior",
            crate::types::WorkItemState::Open,
        );
        storage.append_work_item(&active).unwrap();
        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some(active.id.clone());
        storage.write_agent(&agent).unwrap();

        let mut system_tick = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "work_queue".to_string(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue current work item".to_string(),
            },
        );
        system_tick.trigger_kind = Some(ContinuationTriggerKind::SystemTick);

        let built = build_context(
            &storage,
            &agent,
            &execution_snapshot_for(&agent),
            &crate::types::SkillsRuntimeView::default(),
            &system_tick,
            Some(&ContinuationResolution {
                trigger_kind: ContinuationTriggerKind::SystemTick,
                class: ContinuationClass::LocalContinuation,
                model_reentry: true,
                prior_closure_outcome: crate::types::ClosureOutcome::Completed,
                prior_waiting_reason: None,
                matched_waiting_reason: false,
                evidence: vec![],
            }),
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let current_work_item = built
            .sections
            .iter()
            .find(|section| section.name == "current_work_item")
            .expect("current_work_item projection should be present");
        assert!(current_work_item
            .content
            .contains("Implement continuation anchor runtime behavior"));

        let anchor = built
            .sections
            .iter()
            .find(|section| section.name == "continuation_anchor")
            .expect("continuation_anchor section should be present");
        assert!(anchor.content.contains("Latest trusted operator input:"));
        assert!(anchor.content.contains("runtime system-tick continuation"));
        assert!(!anchor
            .content
            .contains("Implement continuation anchor runtime behavior"));
        assert!(!anchor.content.contains("Current WorkItem"));
    }

    #[test]
    fn build_context_keeps_full_current_input_body_for_long_operator_prompt() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let long_path =
            "/very/long/path/to/a/workspace/projects/example/agentinbox/operator-guides/agentinbox-cli-quickstart-for-review-workflow-2026-04.md";
        let long_prompt = format!(
            "Read this file first and follow it exactly: {long_path}\n\nDo not invent APIs."
        );
        let operator_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: long_prompt.clone(),
            },
        );

        let session = AgentState::new("default");
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &operator_message,
            None,
            &ContextConfig {
                recent_messages: 10,
                recent_briefs: 4,
                compaction_trigger_messages: 10,
                compaction_keep_recent_messages: 4,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let current_input = built
            .sections
            .iter()
            .find(|section| section.name == "current_input")
            .expect("current_input section should be present");
        assert!(current_input.content.contains(long_path));
        assert!(current_input.content.contains("Do not invent APIs."));
        assert!(current_input
            .content
            .contains("[operator][operator_instruction][OperatorPrompt]"));
        assert!(current_input.content.contains("\n  Read this file first"));
    }

    #[test]
    fn build_context_renders_provenance_admission_and_authority_labels() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        let session = AgentState::new("default");
        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("operator:jolestar".into()),
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue from the operator surface.".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::CliPrompt,
            AdmissionContext::LocalProcess,
        );

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig::default(),
        )
        .unwrap();

        let current_input = built
            .sections
            .iter()
            .find(|section| section.name == "current_input")
            .expect("current_input section should be present");
        assert!(current_input.content.contains(
            "[operator][cli_prompt][local_process][operator_instruction][OperatorPrompt]"
        ));
    }

    #[test]
    fn build_context_uses_budgeted_hot_tail_and_preserves_section_order() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        for text in [
            "Investigate the flaky runtime wake path and patch it.",
            "Validate the intermediate wake-path hypothesis before editing state order.",
            "Newest wake-path update: patch the resume path and summarize what remains.",
        ] {
            let message = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: text.to_string(),
                },
            );
            storage.append_message(&message).unwrap();
        }

        let result_brief = BriefRecord::new(
            "default",
            BriefKind::Result,
            "Verified wake-path fix with cargo test and focused regression coverage.",
            None,
            None,
        );
        storage.append_brief(&result_brief).unwrap();

        let tool_record = ToolExecutionRecord {
            id: "tool-budgeted".into(),
            agent_id: "default".into(),
            work_item_id: None,
            turn_index: 0,
            turn_id: None,
            tool_name: "ExecCommand".into(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 10,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": "cargo test"}),
            output: json!({"exit_code": 0}),
            summary: "ran cargo test for the wake-path fix".into(),
            invocation_surface: None,
        };
        storage.append_tool_execution(&tool_record).unwrap();

        let active =
            crate::types::WorkItemRecord::new("default", "wake path patching", WorkItemState::Open);
        storage.append_work_item(&active).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue and report what is still pending.".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            current_work_item_id: Some(active.id.clone()),
            objective: Some(active.objective.clone()),
            work_summary: Some("wake path patching".into()),
            plan: Some(vec!["finish wake-path regression"].join("\n")),
            ..WorkingMemorySnapshot::default()
        };
        session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
            from_revision: 1,
            to_revision: 2,
            created_at_turn: 2,
            reason: crate::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
            changed_fields: vec!["plan".into()],
            summary_lines: vec!["updated plan: finish wake-path regression".into()],
        });

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 8,
                recent_briefs: 8,
                prompt_budget_estimated_tokens: 420,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let section_names = built
            .sections
            .iter()
            .map(|section| section.name.as_str())
            .collect::<Vec<_>>();
        let current_input_index = section_names
            .iter()
            .position(|name| *name == "current_input")
            .unwrap();

        if let Some(working_memory_index) = section_names
            .iter()
            .position(|name| *name == "working_memory")
        {
            assert!(working_memory_index < current_input_index);
        }
        if let Some(delta_index) = section_names
            .iter()
            .position(|name| *name == "working_memory_delta")
        {
            assert!(delta_index < current_input_index);
        }
        if let Some(active_work_index) = section_names
            .iter()
            .position(|name| *name == "current_work_item")
        {
            assert!(active_work_index < current_input_index);
        }
        if let (Some(working_memory_index), Some(delta_index)) = (
            section_names
                .iter()
                .position(|name| *name == "working_memory"),
            section_names
                .iter()
                .position(|name| *name == "working_memory_delta"),
        ) {
            assert!(working_memory_index < delta_index);
        }
        if let (Some(delta_index), Some(active_work_index)) = (
            section_names
                .iter()
                .position(|name| *name == "working_memory_delta"),
            section_names
                .iter()
                .position(|name| *name == "current_work_item"),
        ) {
            assert!(delta_index < active_work_index);
        }

        let trimmed_hot_tail = render_budgeted_lines(
            "Recent messages:",
            vec![
                "- older wake-path probe".into(),
                "- newest wake-path update".into(),
            ],
            estimate_text_tokens("Recent messages:")
                + estimate_text_tokens("- newest wake-path update")
                + 1,
        )
        .expect("budgeted line rendering should keep the newest line that fits");
        assert!(trimmed_hot_tail.contains("newest wake-path update"));
        assert!(!trimmed_hot_tail.contains("older wake-path probe"));
    }

    #[test]
    fn build_context_omits_working_memory_delta_when_budget_is_exhausted() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue the runtime memory work and report the latest status.".into(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            objective: Some("ship the prompt delta gating fix".into()),
            plan: Some(vec!["[InProgress] wire prompt render acknowledgement"].join("\n")),
            ..WorkingMemorySnapshot::default()
        };
        session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
            from_revision: 4,
            to_revision: 5,
            created_at_turn: 7,
            reason: crate::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
            changed_fields: vec!["plan".into()],
            summary_lines: vec![
                "updated the plan with a long-form explanation of why prompt rendering acknowledgement must happen after budgeted assembly rather than before prompt construction".into(),
                "recorded the continuity decision that pending deltas stay durable across turns until the model actually sees the delta section in a rendered prompt".into(),
                "captured low-budget prompt coverage for the interactive runtime path that previously cleared the delta too early".into(),
            ],
        });

        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                prompt_budget_estimated_tokens: 140,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        assert!(!built
            .sections
            .iter()
            .any(|section| section.name == "working_memory_delta"));
    }

    #[test]
    fn build_context_selects_only_relevant_archived_episodes_that_fit_budget() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let old_episode = ContextEpisodeRecord {
            id: "ep_old".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            created_at: chrono::Utc::now(),
            finalized_at: chrono::Utc::now(),
            start_turn_index: 1,
            end_turn_index: 3,
            start_message_count: 1,
            end_message_count: 6,
            boundary_reason: EpisodeBoundaryReason::HardTurnCap,
            current_work_item_id: Some("work_old".into()),
            objective: Some("Refactor parser".into()),
            work_summary: Some("parser cleanup".into()),
            scope_hints: vec!["keep unrelated runtime behavior unchanged".into()],
            source_turn_ids: vec![],
            source_refs: vec![],
            generated_by: None,
            operator_intents: vec![],
            runtime_facts: vec![],
            task_results: vec![],
            unresolved_items: vec![],
            model_inferences: vec![],
            summary: "Completed parser refactor and removed dead branches.".into(),
            working_set_files: vec!["src/parser.rs".into()],
            commands: vec!["cargo test parser".into()],
            verification: vec!["cargo test parser".into()],
            decisions: vec![],
            carry_forward: vec![],
            waiting_on: vec![],
        };
        storage.append_context_episode(&old_episode).unwrap();

        let relevant_episode = ContextEpisodeRecord {
            id: "ep_relevant".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            created_at: chrono::Utc::now(),
            finalized_at: chrono::Utc::now(),
            start_turn_index: 4,
            end_turn_index: 7,
            start_message_count: 7,
            end_message_count: 14,
            boundary_reason: EpisodeBoundaryReason::WaitBoundary,
            current_work_item_id: Some("work_runtime".into()),
            objective: Some("Fix wake path".into()),
            work_summary: Some("wake path patching".into()),
            scope_hints: vec!["keep behavior unchanged outside the wake path".into()],
            source_turn_ids: vec![
                "turn-stable-4".into(),
                "turn-stable-5".into(),
                "turn-stable-6".into(),
                "turn-stable-7".into(),
            ],
            source_refs: vec!["turn:turn-stable-4".into(), "turn:turn-stable-5".into()],
            generated_by: Some(crate::types::ContextEpisodeGeneratedBy {
                component: "runtime_episode_memory".into(),
                reason: EpisodeBoundaryReason::WaitBoundary,
                model: None,
                prompt_ref: None,
            }),
            operator_intents: vec!["objective: Fix wake path".into()],
            runtime_facts: vec!["decision: prefer explicit wake transition ordering".into()],
            task_results: vec![],
            unresolved_items: vec!["carry_forward: confirm remaining wake edge case".into()],
            model_inferences: vec![
                "non_authoritative_summary: wake path remains under review".into()
            ],
            summary:
                "Patched wake path, updated state transitions, and left one runtime follow-up."
                    .into(),
            working_set_files: vec!["src/runtime.rs".into(), "src/context.rs".into()],
            commands: vec!["cargo test runtime".into()],
            verification: vec!["cargo test runtime".into()],
            decisions: vec!["prefer explicit wake transition ordering".into()],
            carry_forward: vec!["confirm remaining wake edge case".into()],
            waiting_on: vec![],
        };
        storage.append_context_episode(&relevant_episode).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue fixing the wake path in src/runtime.rs.".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            current_work_item_id: Some("work_runtime".into()),
            objective: Some("Fix wake path".into()),
            work_summary: Some("wake path patching".into()),
            working_set_files: vec!["src/runtime.rs".into()],
            pending_followups: vec!["confirm remaining wake edge case".into()],
            ..WorkingMemorySnapshot::default()
        };

        let episodes = storage.read_recent_context_episodes(4).unwrap();
        let episode_section = build_relevant_episode_memory_section(
            &episodes,
            &session,
            None,
            &current_message,
            &ContextConfig {
                prompt_budget_estimated_tokens: 280,
                max_relevant_episodes: 1,
                ..ContextConfig::default()
            },
            120,
            None,
        )
        .expect("relevant episode memory section should be present");

        assert!(episode_section.content.contains("ep_relevant"));
        assert!(episode_section.content.contains(
            "Source refs: turns [turn-stable-4, turn-stable-5, turn-stable-6, turn-stable-7]"
        ));
        assert!(episode_section
            .content
            .contains("Operator intent (authoritative only from sources)"));
        assert!(episode_section
            .content
            .contains("Runtime facts: decision: prefer explicit"));
        assert!(episode_section
            .content
            .contains("Model inference (non-authoritative evidence)"));
        assert!(episode_section.content.contains("src/runtime.rs"));
        assert!(episode_section
            .content
            .contains("keep behavior unchanged outside the wake path"));
        assert!(!episode_section.content.contains("ep_old"));
    }

    #[test]
    fn build_context_excludes_episode_overlapping_recent_turn_window() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let archived_episode = ContextEpisodeRecord {
            id: "ep_archived".into(),
            agent_id: "default".into(),
            workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
            created_at: chrono::Utc::now(),
            finalized_at: chrono::Utc::now(),
            start_turn_index: 1,
            end_turn_index: 2,
            start_message_count: 1,
            end_message_count: 4,
            boundary_reason: EpisodeBoundaryReason::HardTurnCap,
            current_work_item_id: Some("work_wake".into()),
            objective: Some("Fix wake path".into()),
            work_summary: Some("wake path patching".into()),
            scope_hints: vec![],
            source_turn_ids: vec![],
            source_refs: vec![],
            generated_by: None,
            operator_intents: vec![],
            runtime_facts: vec![],
            task_results: vec![],
            unresolved_items: vec![],
            model_inferences: vec![],
            summary: "Archived wake path evidence from older turns.".into(),
            working_set_files: vec!["src/runtime.rs".into()],
            commands: vec![],
            verification: vec![],
            decisions: vec![],
            carry_forward: vec![],
            waiting_on: vec![],
        };
        storage.append_context_episode(&archived_episode).unwrap();

        let overlapping_episode = ContextEpisodeRecord {
            id: "ep_recent_overlap".into(),
            start_turn_index: 4,
            end_turn_index: 5,
            summary: "Recent wake path evidence already covered by recent_turns.".into(),
            ..archived_episode.clone()
        };
        storage
            .append_context_episode(&overlapping_episode)
            .unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue fixing the wake path in src/runtime.rs.".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            current_work_item_id: Some("work_wake".into()),
            objective: Some("Fix wake path".into()),
            work_summary: Some("wake path patching".into()),
            working_set_files: vec!["src/runtime.rs".into()],
            ..WorkingMemorySnapshot::default()
        };

        let episodes = storage.read_recent_context_episodes(4).unwrap();
        let section = build_relevant_episode_memory_section(
            &episodes,
            &session,
            None,
            &current_message,
            &ContextConfig {
                max_relevant_episodes: 3,
                ..ContextConfig::default()
            },
            240,
            Some(4),
        )
        .expect("archived episode should still be recalled");

        assert!(section.content.contains("ep_archived"));
        assert!(!section.content.contains("ep_recent_overlap"));
    }

    #[test]
    fn recent_turn_window_start_ignores_zero_sentinel_turn_index() {
        let sentinel = crate::types::TurnRecord::new("default", "turn-zero", 0);
        let recent = crate::types::TurnRecord::new("default", "turn-recent", 6);

        assert_eq!(recent_turn_window_start(&[sentinel, recent]), Some(6));
    }

    #[test]
    fn build_context_orders_work_item_before_relevant_episode_memory() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let mut work_item =
            crate::types::WorkItemRecord::new("default", "Fix wake path", WorkItemState::Open);
        work_item.id = "work_wake".into();
        work_item.plan_status = crate::types::WorkItemPlanStatus::Ready;
        storage.append_work_item(&work_item).unwrap();

        let mut agent = AgentState::new("default");
        agent.current_work_item_id = Some(work_item.id.clone());
        agent.working_memory.current_working_memory = WorkingMemorySnapshot {
            current_work_item_id: Some(work_item.id.clone()),
            objective: Some("Fix wake path".into()),
            work_summary: Some("wake path patching".into()),
            working_set_files: vec!["src/runtime.rs".into()],
            ..WorkingMemorySnapshot::default()
        };
        storage.write_agent(&agent).unwrap();

        storage
            .append_context_episode(&ContextEpisodeRecord {
                id: "ep_archived".into(),
                agent_id: "default".into(),
                workspace_id: crate::types::AGENT_HOME_WORKSPACE_ID.into(),
                created_at: chrono::Utc::now(),
                finalized_at: chrono::Utc::now(),
                start_turn_index: 1,
                end_turn_index: 2,
                start_message_count: 1,
                end_message_count: 4,
                boundary_reason: EpisodeBoundaryReason::HardTurnCap,
                current_work_item_id: Some(work_item.id.clone()),
                objective: Some("Fix wake path".into()),
                work_summary: Some("wake path patching".into()),
                scope_hints: vec![],
                source_turn_ids: vec![],
                source_refs: vec![],
                generated_by: None,
                operator_intents: vec![],
                runtime_facts: vec![],
                task_results: vec![],
                unresolved_items: vec![],
                model_inferences: vec![],
                summary: "Archived wake path evidence from older turns.".into(),
                working_set_files: vec!["src/runtime.rs".into()],
                commands: vec![],
                verification: vec![],
                decisions: vec![],
                carry_forward: vec![],
                waiting_on: vec![],
            })
            .unwrap();

        storage
            .append_turn(&TurnRecord::new("default", "turn-4", 4))
            .unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue fixing the wake path in src/runtime.rs.".to_string(),
            },
        );

        let built = build_context(
            &storage,
            &agent,
            &execution_snapshot_for(&agent),
            &crate::types::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_episode_candidates: 4,
                max_relevant_episodes: 2,
                prompt_budget_estimated_tokens: 2048,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let names = built
            .sections
            .iter()
            .map(|section| section.name.as_str())
            .collect::<Vec<_>>();
        let work_item_index = names
            .iter()
            .position(|name| *name == "current_work_item")
            .expect("current_work_item section");
        let episode_index = names
            .iter()
            .position(|name| *name == "relevant_episode_memory")
            .expect("relevant_episode_memory section");

        assert!(work_item_index < episode_index);
    }

    #[test]
    fn build_context_enforces_total_prompt_budget_across_large_sections() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "Keep the current input visible while trimming oversized context sections."
                    .to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            objective: Some("Stabilize prompt budgeting".into()),
            work_summary: Some("Trim oversized context sections before append".into()),
            scope_hints: vec![
                "scope hint one repeats enough text to force truncation".repeat(8),
                "scope hint two repeats enough text to force truncation".repeat(8),
            ],
            plan: Some(
                vec![
                    "inspect pre-turn sections for hard budget compliance".repeat(8),
                    "retain current input after oversized section truncation".repeat(8),
                ]
                .join("\n"),
            ),
            working_set_files: vec![
                "src/context.rs".repeat(12),
                "src/runtime/message_dispatch.rs".repeat(12),
            ],
            recent_decisions: vec![
                "treat prompt budget as a hard cap, not a post-hoc estimate".repeat(8)
            ],
            pending_followups: vec![
                "reply on the PR after the hard-cap regression test passes".repeat(8)
            ],
            waiting_on: vec!["reviewer confirmation after the hard cap lands".repeat(8)],
            ..WorkingMemorySnapshot::default()
        };
        session.worktree_session = Some(crate::types::WorktreeSession {
            original_cwd: PathBuf::from("/canonical/repo"),
            original_branch: "main".into(),
            worktree_path: PathBuf::from("/worktrees/issue-264-budget-aware-prompt"),
            worktree_branch: "feature/issue-264-budget-aware-prompt".into(),
        });

        let current_work_item = crate::types::WorkItemRecord::new(
            "default",
            "current work item summary repeats to guarantee it needs truncation ".repeat(12),
            WorkItemState::Open,
        );
        storage.append_work_item(&current_work_item).unwrap();
        session
            .working_memory
            .current_working_memory
            .current_work_item_id = Some(current_work_item.id.clone());

        let skills = crate::types::SkillsRuntimeView {
            agent_templates_catalog: Vec::new(),
            discoverable_skills: vec![crate::types::SkillCatalogEntry {
                skill_id: "skill/large-catalog-entry".into(),
                name: "Large Catalog Entry".into(),
                description:
                    "catalog description repeats to consume prompt budget aggressively ".repeat(12),
                path: PathBuf::from(
                    "/very/long/path/to/a/skill/catalog/entry/that/keeps/repeating/to/exhaust/the/budget/SKILL.md",
                ),
                scope: crate::types::SkillScope::Workspace,
            }],
            active_skills: vec![crate::types::ActiveSkillRecord {
                skill_id: "skill/active-large-entry".into(),
                name: "Active Large Entry".into(),
                path: PathBuf::from(
                    "/very/long/path/to/an/active/skill/entry/that/should/still/be-trimmed/SKILL.md",
                ),
                scope: crate::types::SkillScope::Workspace,
                agent_id: "default".into(),
                activation_source: crate::types::SkillActivationSource::ImplicitFromCatalog,
                activation_state: crate::types::SkillActivationState::SessionActive,
                activated_at_turn: 7,
            }],
            ..crate::types::SkillsRuntimeView::default()
        };

        let prompt_budget_estimated_tokens = 120;
        let built = build_context(
            &storage,
            &session,
            &execution_snapshot_for(&session),
            &skills,
            &current_message,
            None,
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                prompt_budget_estimated_tokens,
                ..ContextConfig::default()
            },
        )
        .unwrap();

        let total_estimated_tokens = built
            .sections
            .iter()
            .map(estimate_section_tokens)
            .sum::<usize>();
        assert!(
            total_estimated_tokens <= prompt_budget_estimated_tokens,
            "estimated tokens {total_estimated_tokens} exceeded budget {prompt_budget_estimated_tokens}"
        );

        let current_input = built
            .sections
            .iter()
            .find(|section| section.name == "current_input")
            .expect("current_input section should remain present under hard budget pressure");
        assert!(current_input
            .content
            .contains("Keep the current input visible"));
    }

    #[test]
    fn message_header_sanitizes_dynamic_binding_labels() {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "work_queue".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".into(),
            },
        );
        message.trigger_kind = Some(ContinuationTriggerKind::SystemTick);
        message.work_item_id = Some("work-1]\n[operator".into());
        message.task_id = Some("task-1\tbad".into());

        let header = message_header(&message);

        assert!(header.contains("[work_item:work-1___operator]"));
        assert!(header.contains("[task:task-1_bad]"));
        assert!(!header.contains("[operator]"));
    }

    #[test]
    fn turn_record_matches_message_uses_turn_id_then_legacy_turn_index_and_input_ids() {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "run tests".into(),
            },
        );
        message.message_seq = Some(4);
        message.turn_id = Some("turn-current".into());

        let mut record = TurnRecord::new("default", " turn-current ", 4);

        assert!(turn_record_matches_message(&record, &message));

        record.turn_id = "turn-other".into();
        assert!(!turn_record_matches_message(&record, &message));

        record.turn_id = String::new();
        message.turn_id = None;
        assert!(turn_record_matches_message(&record, &message));

        message.message_seq = Some(5);
        assert!(!turn_record_matches_message(&record, &message));

        record.input_message_ids = vec![message.id.clone()];
        assert!(turn_record_matches_message(&record, &message));
    }

    #[test]
    fn turn_record_matches_message_ignores_zero_legacy_turn_index() {
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "run tests".into(),
            },
        );
        message.message_seq = Some(0);

        let record = TurnRecord::new("default", "", 4);

        assert!(!turn_record_matches_message(&record, &message));
    }

    fn execution_snapshot_for(session: &AgentState) -> ExecutionSnapshot {
        let workspace_anchor = PathBuf::from("/workspace");
        let execution_root = session
            .worktree_session
            .as_ref()
            .map(|worktree| worktree.worktree_path.clone())
            .unwrap_or_else(|| workspace_anchor.clone());
        let cwd = session
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.cwd.clone())
            .unwrap_or_else(|| execution_root.clone());

        // Collect attached workspaces from session.attached_workspaces.
        // For testing purposes, we create mock anchors since we don't have storage access.
        let attached_workspaces: Vec<(String, PathBuf)> = session
            .attached_workspaces
            .iter()
            .map(|ws_id| {
                let anchor = session
                    .active_workspace_entry
                    .as_ref()
                    .and_then(|entry| {
                        if entry.workspace_id == *ws_id {
                            Some(entry.workspace_anchor.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| PathBuf::from(format!("/workspace/{}", ws_id)));
                (ws_id.clone(), anchor)
            })
            .collect();

        ExecutionSnapshot {
            profile: session.execution_profile.clone(),
            policy: session.execution_profile.policy_snapshot(),
            workspace_id: None,
            attached_workspaces,
            workspace_anchor: workspace_anchor.clone(),
            execution_root: execution_root.clone(),
            cwd,
            execution_root_id: Some(if session.worktree_session.is_some() {
                "git_worktree_root:workspace".into()
            } else {
                "canonical_root:workspace".into()
            }),
            projection_kind: Some(if session.worktree_session.is_some() {
                crate::system::types::WorkspaceProjectionKind::GitWorktreeRoot
            } else {
                crate::system::types::WorkspaceProjectionKind::CanonicalRoot
            }),
            access_mode: Some(if session.worktree_session.is_some() {
                crate::system::types::WorkspaceAccessMode::ExclusiveWrite
            } else {
                crate::system::types::WorkspaceAccessMode::SharedRead
            }),
            worktree_root: session
                .worktree_session
                .as_ref()
                .map(|worktree| worktree.worktree_path.clone()),
        }
    }
}
