use anyhow::Result;

use crate::{
    prompt::{PromptSection, PromptStability},
    storage::AppStorage,
    system::{execution_policy_summary_lines, ExecutionSnapshot},
    types::{
        AdmissionContext, AgentState, AuthorityClass, BriefRecord, ContextEpisodeRecord,
        ContinuationClass, ContinuationResolution, MessageBody, MessageDeliverySurface,
        MessageEnvelope, MessageKind, MessageOrigin, SkillsRuntimeView, ToolExecutionRecord,
        TrustLevel, WorkItemRecord, WorkPlanSnapshot, WorkingMemoryDelta, WorkingMemorySnapshot,
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
        }
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
    let messages =
        storage.read_messages_from(agent.compacted_message_count, config.recent_messages)?;
    let briefs = storage.read_recent_briefs(config.recent_briefs)?;
    let tools = storage.read_recent_tool_executions(config.recent_messages)?;
    let episodes = storage.read_recent_context_episodes(config.recent_episode_candidates)?;
    let work_queue_projection = storage.work_queue_prompt_projection()?;
    let active_work_item = work_queue_projection.active.as_ref();
    let active_work_plan = active_work_item
        .map(|item| storage.latest_work_plan(&item.id))
        .transpose()?
        .flatten();
    let queued_waiting_items = work_queue_projection
        .queued_waiting
        .iter()
        .collect::<Vec<_>>();

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

    if let Some(section) = build_relevant_episode_memory_section(
        &episodes,
        agent,
        active_work_item,
        current_message,
        config,
        remaining_budget,
    ) {
        push_budgeted_section(&mut sections, &mut remaining_budget, section);
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

    if let Some(delta) = &agent.working_memory.pending_working_memory_delta {
        if let Some(content) = render_working_memory_delta_with_budget(delta, remaining_budget) {
            push_budgeted_section(
                &mut sections,
                &mut remaining_budget,
                turn_section("working_memory_delta", content),
            );
        }
    }

    if let Some(work_item) = active_work_item {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section(
                "active_work_item",
                render_active_work_item(work_item, active_work_plan.as_ref()),
            ),
        );
    }

    if !queued_waiting_items.is_empty() {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section(
                "queued_waiting_work_items",
                render_queued_waiting_work_items(&queued_waiting_items),
            ),
        );
    }

    push_budgeted_section(
        &mut sections,
        &mut remaining_budget,
        section(
            "context_contract",
            "Interpret the memory block with this priority: active work item first for the committed delivery target and current runtime task, working memory delta next for the newest updates since the last prompt, and working memory after that for rolling agent context. This is an interpretation priority, not a guarantee about section ordering. Use prior briefs and recent tool results as the most reliable continuity evidence across turns. When these sources differ on task scope or delivery target, treat the active work item's `delivery_target` as the ground truth for the current committed task unless the current input explicitly changes it."
                .to_string(),
        ),
    );

    if let Some(content) = render_recent_messages_with_budget(&messages, remaining_budget) {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section("recent_messages", content),
        );
    }

    let latest_result_id = briefs
        .iter()
        .rev()
        .find(|brief| matches!(brief.kind, crate::types::BriefKind::Result))
        .map(|brief| brief.id.clone());
    if let Some(last_result) = latest_result_id
        .as_deref()
        .and_then(|id| briefs.iter().find(|brief| brief.id == id))
    {
        let latest_result_budget = remaining_budget;
        let latest_result_content = truncate_section_content(
            "Latest completed result:\n",
            &last_result.text,
            latest_result_budget,
            Some("\n[truncated latest result]"),
        );
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section("latest_result", latest_result_content),
        );
    }

    let recent_briefs = briefs
        .iter()
        .filter(|brief| latest_result_id.as_deref() != Some(brief.id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(content) = render_recent_briefs_with_budget(&recent_briefs, remaining_budget) {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section("recent_briefs", content),
        );
    }

    if let Some(content) = render_recent_tools_with_budget(&tools, remaining_budget) {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section("recent_tool_executions", content),
        );
    }

    if let Some(content) = continuation_context(current_message, continuation) {
        push_budgeted_section(
            &mut sections,
            &mut remaining_budget,
            turn_section("continuation_context", content),
        );
    }

    let mut current_input_budget = current_input_reserved_budget.saturating_add(remaining_budget);
    let current_input_body =
        render_current_input_body_with_budget(&current_message.body, current_input_budget);
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

fn message_header(message: &MessageEnvelope) -> String {
    let mut labels = vec![origin_label(&message.origin).to_string()];
    if let Some(surface) = message.delivery_surface {
        labels.push(delivery_surface_label(surface).to_string());
    }
    if let Some(context) = message.admission_context {
        labels.push(admission_context_label(context).to_string());
    }
    labels.push(authority_class_label(message.authority_class).to_string());
    labels.push(kind_label(message));
    format!("[{}]", labels.join("]["))
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

fn render_brief(brief: &BriefRecord) -> String {
    format!("- [{:?}] {}", brief.kind, brief.text)
}

fn render_active_work_item(work_item: &WorkItemRecord, plan: Option<&WorkPlanSnapshot>) -> String {
    let mut lines = vec![
        "Active work item:".to_string(),
        format!("- Id: {}", work_item.id),
        format!("- Status: {:?}", work_item.status),
        format!("- Delivery target: {}", work_item.delivery_target),
    ];
    if let Some(parent_id) = work_item.parent_id.as_deref() {
        lines.push(format!("- Parent id: {parent_id}"));
    }
    if let Some(summary) = work_item.summary.as_deref() {
        lines.push(format!("- Summary: {summary}"));
    }
    if let Some(progress_note) = work_item.progress_note.as_deref() {
        lines.push(format!("- Progress note: {progress_note}"));
    }
    if let Some(plan) = plan {
        lines.push("- Current work plan:".to_string());
        lines.extend(
            plan.items
                .iter()
                .map(|item| format!("  - [{:?}] {}", item.status, item.step)),
        );
    }
    lines.join("\n")
}

fn render_queued_waiting_work_items(items: &[&WorkItemRecord]) -> String {
    let mut lines = vec!["Queued and waiting work items:".to_string()];
    lines.extend(items.iter().map(|item| {
        let mut summary = format!(
            "- [{:?}] {} :: {}",
            item.status, item.id, item.delivery_target
        );
        if let Some(progress_note) = item.progress_note.as_deref() {
            summary.push_str(&format!(" :: {progress_note}"));
        } else if let Some(summary_text) = item.summary.as_deref() {
            summary.push_str(&format!(" :: {summary_text}"));
        }
        summary
    }));
    lines.join("\n")
}

fn working_memory_is_empty(snapshot: &WorkingMemorySnapshot) -> bool {
    snapshot == &WorkingMemorySnapshot::default()
}

fn render_working_memory(snapshot: &WorkingMemorySnapshot) -> String {
    let mut lines = vec!["Working memory:".to_string()];
    if let Some(active_work_item_id) = snapshot.active_work_item_id.as_deref() {
        lines.push(format!("- Active work item id: {active_work_item_id}"));
    }
    if let Some(delivery_target) = snapshot.delivery_target.as_deref() {
        lines.push(format!("- Delivery target: {delivery_target}"));
    }
    if let Some(work_summary) = snapshot.work_summary.as_deref() {
        lines.push(format!("- Work summary: {work_summary}"));
    }
    if !snapshot.current_plan.is_empty() {
        lines.push("- Current plan:".to_string());
        lines.extend(
            snapshot
                .current_plan
                .iter()
                .map(|step| format!("  - {step}")),
        );
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

fn trust_label(trust: &TrustLevel) -> &'static str {
    match trust {
        TrustLevel::TrustedOperator => "trusted_operator",
        TrustLevel::TrustedSystem => "trusted_system",
        TrustLevel::TrustedIntegration => "trusted_integration",
        TrustLevel::UntrustedExternal => "untrusted_external",
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
    let text = match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => value.to_string(),
        MessageBody::Brief { text, .. } => text.clone(),
    };
    if text.chars().count() <= 160 {
        text
    } else {
        format!("{}...", text.chars().take(160).collect::<String>())
    }
}

fn render_current_input_body_with_budget(body: &MessageBody, budget: usize) -> String {
    let rendered = match body {
        MessageBody::Text { text } => text.clone(),
        MessageBody::Json { value } => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        MessageBody::Brief { text, .. } => text.clone(),
    };
    truncate_section_content(
        "",
        &rendered,
        budget.max(64),
        Some("\n[truncated current input body]"),
    )
}

fn continuation_context(
    message: &MessageEnvelope,
    continuation: Option<&ContinuationResolution>,
) -> Option<String> {
    let continuation = continuation?;
    if !continuation.model_visible {
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
    Some(format!(
        " - Wake hint:\n\
         - Source: {source}\n\
         - Resource: {resource}\n\
         - Reason: {reason}\n\
         - Content-Type: {content_type}\n\
         - Payload:\n{payload}"
    ))
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
    let delivery_target = work_queue
        .get("delivery_target")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let activated_from_queue = work_queue
        .get("activated_from_queue")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Some(format!(
        " - Work queue:\n\
         - Reason: {reason}\n\
         - Work item id: {work_item_id}\n\
         - Delivery target: {delivery_target}\n\
         - Activated from queue: {activated_from_queue}"
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

fn render_recent_messages_with_budget(
    messages: &[MessageEnvelope],
    budget: usize,
) -> Option<String> {
    render_budgeted_lines(
        "Recent messages:",
        messages
            .iter()
            .filter(|message| include_in_prompt_context(message))
            .map(render_message)
            .collect(),
        budget,
    )
}

fn render_recent_briefs_with_budget(briefs: &[BriefRecord], budget: usize) -> Option<String> {
    render_budgeted_lines(
        "Recent briefs:",
        briefs.iter().map(render_brief).collect(),
        budget,
    )
}

fn render_recent_tools_with_budget(tools: &[ToolExecutionRecord], budget: usize) -> Option<String> {
    render_budgeted_lines(
        "Recent tool executions:",
        tools
            .iter()
            .map(|record| {
                format!(
                    "- [{}][{:?}] {}",
                    trust_label(&record.trust),
                    record.status,
                    record.summary
                )
            })
            .collect(),
        budget,
    )
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
    active_work_item_id: Option<&'a str>,
    delivery_target: Option<&'a str>,
    work_summary: Option<&'a str>,
    working_set_files: &'a [String],
    pending_followups: &'a [String],
    waiting_on: &'a [String],
    query_text: String,
}

fn build_relevant_episode_memory_section(
    episodes: &[ContextEpisodeRecord],
    agent: &AgentState,
    active_work_item: Option<&WorkItemRecord>,
    current_message: &MessageEnvelope,
    config: &ContextConfig,
    budget: usize,
) -> Option<PromptSection> {
    if episodes.is_empty() || config.max_relevant_episodes == 0 || budget == 0 {
        return None;
    }

    let working_memory = &agent.working_memory.current_working_memory;
    let query_text = format!(
        "{}\n{}\n{}",
        body_preview(&current_message.body),
        working_memory.work_summary.as_deref().unwrap_or_default(),
        active_work_item
            .and_then(|item| item.summary.as_deref())
            .unwrap_or_default()
    );
    let anchor = EpisodeSelectionAnchor {
        active_work_item_id: working_memory
            .active_work_item_id
            .as_deref()
            .or_else(|| active_work_item.map(|item| item.id.as_str())),
        delivery_target: working_memory
            .delivery_target
            .as_deref()
            .or_else(|| active_work_item.map(|item| item.delivery_target.as_str())),
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

fn render_episode_block(episode: &ContextEpisodeRecord) -> String {
    let mut lines = vec![format!(
        "- [episode {}][turns {}-{}][boundary {}]",
        episode.id,
        episode.start_turn_index,
        episode.end_turn_index,
        enum_label(&episode.boundary_reason)
    )];
    if let Some(delivery_target) = episode.delivery_target.as_deref() {
        lines.push(format!("  - Delivery target: {delivery_target}"));
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
    lines.push(format!("  - Summary: {}", episode.summary));
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
        .active_work_item_id
        .filter(|id| episode.active_work_item_id.as_deref() == Some(*id))
        .map(|_| 120)
        .unwrap_or(0);

    if normalized_option_eq(episode.delivery_target.as_deref(), anchor.delivery_target) {
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
        storage::AppStorage,
        types::{
            AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
            AgentVisibility, BriefKind, BriefRecord, ContextEpisodeRecord, EpisodeBoundaryReason,
            LoadedAgentsMd, MessageKind, MessageOrigin, Priority, ToolExecutionRecord,
            ToolExecutionStatus, TrustLevel, WorkItemStatus,
        },
    };

    use super::*;

    #[test]
    fn compaction_adds_summary_when_message_count_exceeds_threshold() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        for idx in 0..6 {
            let msg = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                TrustLevel::TrustedOperator,
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
                TrustLevel::TrustedOperator,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("message-{idx}"),
                },
            );
            storage.append_message(&msg).unwrap();
        }

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            delivery_target: Some("ship working memory".into()),
            current_plan: vec!["[InProgress] keep cache identity stable".into()],
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
    fn prompt_cache_identity_stays_stable_when_only_legacy_compaction_changes() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        for idx in 0..6 {
            let msg = MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator { actor_id: None },
                TrustLevel::TrustedOperator,
                Priority::Normal,
                MessageBody::Text {
                    text: format!("message-{idx}"),
                },
            );
            storage.append_message(&msg).unwrap();
        }

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            delivery_target: Some("ship working memory".into()),
            current_plan: vec!["[InProgress] keep cache identity stable".into()],
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
            TrustLevel::TrustedOperator,
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
                TrustLevel::TrustedOperator,
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
        assert_eq!(first_prompt.cache_identity, second_prompt.cache_identity);
        assert_eq!(second_prompt.cache_identity.compression_epoch, 7);
    }

    #[test]
    fn build_context_includes_latest_result_and_generic_context_contract() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let prior_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "fix the failing benchmark".to_string(),
            },
        );
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
            turn_index: 1,
            tool_name: "ExecCommand".to_string(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 123,
            trust: TrustLevel::TrustedOperator,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": "cargo test"}),
            output: json!({"exit_code": 0}),
            summary: "Verified with cargo test".to_string(),
            invocation_surface: None,
        };
        storage.append_tool_execution(&tool_record).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
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

        let latest_result = built
            .sections
            .iter()
            .find(|section| section.name == "latest_result")
            .expect("latest result section should be present");
        assert!(latest_result
            .content
            .contains("Updated benchmark summary reporting"));

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
            .contains("active work item first for the committed delivery target"));
    }

    #[test]
    fn build_context_does_not_repeat_latest_result_in_recent_briefs() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".to_string(),
            },
        );
        storage
            .append_brief(&BriefRecord::new(
                "default",
                BriefKind::Ack,
                "Acknowledged the request.",
                Some(current_message.id.clone()),
                None,
            ))
            .unwrap();
        storage
            .append_brief(&BriefRecord::new(
                "default",
                BriefKind::Result,
                "Unique latest result content.",
                Some(current_message.id.clone()),
                None,
            ))
            .unwrap();

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

        let latest_result = built
            .sections
            .iter()
            .find(|section| section.name == "latest_result")
            .expect("latest result section should be present");
        assert!(latest_result
            .content
            .contains("Unique latest result content."));
        let recent_briefs = built
            .sections
            .iter()
            .find(|section| section.name == "recent_briefs")
            .expect("non-result brief should still be present");
        assert!(!recent_briefs
            .content
            .contains("Unique latest result content."));
        assert!(recent_briefs.content.contains("Acknowledged the request."));
    }

    #[test]
    fn build_context_skips_messages_covered_by_compacted_summary() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();
        for idx in 0..20 {
            storage
                .append_message(&MessageEnvelope::new(
                    "default",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator { actor_id: None },
                    TrustLevel::TrustedOperator,
                    Priority::Normal,
                    MessageBody::Text {
                        text: format!("message-{idx}"),
                    },
                ))
                .unwrap();
        }

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
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

        let recent_messages = built
            .sections
            .iter()
            .find(|section| section.name == "recent_messages")
            .expect("recent messages section should be present");
        assert!(!recent_messages.content.contains("message-8"));
        assert!(!recent_messages.content.contains("message-11"));
        assert!(recent_messages.content.contains("message-12"));
        assert!(recent_messages.content.contains("message-19"));
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
                TrustLevel::TrustedOperator,
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
                    TrustLevel::TrustedOperator,
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
            delivery_target: Some("active work".into()),
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
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "continue".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.context_summary = Some("legacy summary".into());
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            delivery_target: Some("ship working memory".into()),
            current_plan: vec!["[InProgress] wire post-turn refresh".into()],
            ..WorkingMemorySnapshot::default()
        };
        session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
            from_revision: 0,
            to_revision: 1,
            created_at_turn: 1,
            reason: crate::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
            changed_fields: vec!["current_plan".into()],
            summary_lines: vec!["updated current plan: [InProgress] wire post-turn refresh".into()],
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

        let prior_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "check whether the wake path is stable".to_string(),
            },
        );
        storage.append_message(&prior_message).unwrap();

        storage
            .append_brief(&BriefRecord::new(
                "default",
                BriefKind::Failure,
                "Tried cargo test wake_path, but the result is still inconclusive.",
                Some(prior_message.id.clone()),
                None,
            ))
            .unwrap();
        storage
            .append_tool_execution(&ToolExecutionRecord {
                id: "tool-raw-evidence".to_string(),
                agent_id: "default".to_string(),
                work_item_id: None,
                turn_index: 1,
                tool_name: "ExecCommand".to_string(),
                created_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
                duration_ms: 42,
                trust: TrustLevel::TrustedOperator,
                status: ToolExecutionStatus::Success,
                input: json!({"cmd": "cargo test wake_path"}),
                output: json!({"exit_code": 1}),
                summary: "cargo test wake_path still flakes under load".to_string(),
                invocation_surface: None,
            })
            .unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Summarize the current state.".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            delivery_target: Some("stabilize wake path".into()),
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

        let recent_briefs = built
            .sections
            .iter()
            .find(|section| section.name == "recent_briefs")
            .expect("recent briefs section should be present");
        assert!(recent_briefs.content.contains("still inconclusive"));

        let recent_tools = built
            .sections
            .iter()
            .find(|section| section.name == "recent_tool_executions")
            .expect("recent tool executions section should be present");
        assert!(recent_tools.content.contains("cargo test wake_path"));
    }

    #[test]
    fn build_context_does_not_emit_follow_up_specific_contracts() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
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
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Use the demo skill".to_string(),
            },
        );

        let session = AgentState::new("default");
        let skills = SkillsRuntimeView {
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
    fn build_context_includes_worktree_session_when_active() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
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
    fn build_context_includes_active_work_item_and_plan_sections() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue the implementation".to_string(),
            },
        );

        let mut active = crate::types::WorkItemRecord::new(
            "default",
            "Persist work-item state",
            crate::types::WorkItemStatus::Active,
        );
        active.summary = Some("storage and recovery foundation".into());
        active.progress_note = Some("storage API added; prompt projection next".into());
        storage.append_work_item(&active).unwrap();
        storage
            .append_work_plan(&crate::types::WorkPlanSnapshot::new(
                "default",
                active.id.clone(),
                vec![
                    crate::types::WorkPlanItem {
                        step: "Persist work-item store".into(),
                        status: crate::types::WorkPlanStepStatus::Completed,
                    },
                    crate::types::WorkPlanItem {
                        step: "Project active item into prompt".into(),
                        status: crate::types::WorkPlanStepStatus::InProgress,
                    },
                ],
            ))
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

        let active_section = built
            .sections
            .iter()
            .find(|section| section.name == "active_work_item")
            .expect("active_work_item section should be present");
        assert!(active_section.content.contains("Persist work-item state"));
        assert!(active_section
            .content
            .contains("storage and recovery foundation"));
        assert!(active_section
            .content
            .contains("storage API added; prompt projection next"));
        assert!(active_section
            .content
            .contains("Project active item into prompt"));
    }

    #[test]
    fn build_context_includes_compact_queued_waiting_work_item_summary_and_omits_completed() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue the implementation".to_string(),
            },
        );

        let queued = crate::types::WorkItemRecord::new(
            "default",
            "Queue follow-up verification",
            crate::types::WorkItemStatus::Queued,
        );
        let mut waiting = crate::types::WorkItemRecord::new(
            "default",
            "Wait for operator confirmation",
            crate::types::WorkItemStatus::Waiting,
        );
        waiting.progress_note = Some("needs explicit approval before completion".into());
        let completed = crate::types::WorkItemRecord::new(
            "default",
            "Already finished item",
            crate::types::WorkItemStatus::Completed,
        );
        storage.append_work_item(&queued).unwrap();
        storage.append_work_item(&waiting).unwrap();
        storage.append_work_item(&completed).unwrap();

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
            .find(|section| section.name == "queued_waiting_work_items")
            .expect("queued_waiting_work_items section should be present");
        assert!(summary.content.contains("Queue follow-up verification"));
        assert!(summary.content.contains("Wait for operator confirmation"));
        assert!(summary
            .content
            .contains("needs explicit approval before completion"));
        assert!(!summary.content.contains("Already finished item"));
    }

    #[test]
    fn build_context_omits_worktree_session_when_not_active() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
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
            TrustLevel::TrustedOperator,
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
    }

    #[test]
    fn build_context_omits_system_tick_from_recent_messages() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let operator_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "hello".to_string(),
            },
        );
        storage.append_message(&operator_message).unwrap();

        let system_tick = MessageEnvelope::new(
            "default",
            MessageKind::SystemTick,
            MessageOrigin::System {
                subsystem: "wake_hint".to_string(),
            },
            TrustLevel::TrustedSystem,
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

        let recent_messages = built
            .sections
            .iter()
            .find(|section| section.name == "recent_messages")
            .expect("recent_messages section should be present");
        assert!(recent_messages.content.contains("hello"));
        assert!(!recent_messages.content.contains("SystemTick"));
        assert!(!recent_messages.content.contains("wake hint: changed"));
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
            TrustLevel::TrustedSystem,
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
                model_visible: true,
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
            TrustLevel::TrustedSystem,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue active work item: fix stale pid handling".to_string(),
            },
        );
        system_tick.metadata = Some(json!({
            "work_queue": {
                "reason": "continue_active",
                "work_item_id": "work_123",
                "delivery_target": "fix stale pid handling",
                "activated_from_queue": false
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
                model_visible: true,
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
            TrustLevel::TrustedOperator,
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
            TrustLevel::TrustedOperator,
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
                TrustLevel::TrustedOperator,
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
            turn_index: 1,
            tool_name: "ExecCommand".into(),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: 10,
            trust: TrustLevel::TrustedOperator,
            status: ToolExecutionStatus::Success,
            input: json!({"cmd": "cargo test"}),
            output: json!({"exit_code": 0}),
            summary: "ran cargo test for the wake-path fix".into(),
            invocation_surface: None,
        };
        storage.append_tool_execution(&tool_record).unwrap();

        let mut active = crate::types::WorkItemRecord::new(
            "default",
            "Fix flaky wake path",
            WorkItemStatus::Active,
        );
        active.summary = Some("wake path patching".into());
        storage.append_work_item(&active).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue and report what is still pending.".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            active_work_item_id: Some(active.id.clone()),
            delivery_target: Some(active.delivery_target.clone()),
            work_summary: Some("wake path patching".into()),
            current_plan: vec!["finish wake-path regression".into()],
            ..WorkingMemorySnapshot::default()
        };
        session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
            from_revision: 1,
            to_revision: 2,
            created_at_turn: 2,
            reason: crate::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
            changed_fields: vec!["current_plan".into()],
            summary_lines: vec!["updated current plan: finish wake-path regression".into()],
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
            .position(|name| *name == "active_work_item")
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
                .position(|name| *name == "active_work_item"),
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
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue the runtime memory work and report the latest status.".into(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            delivery_target: Some("ship the prompt delta gating fix".into()),
            current_plan: vec!["[InProgress] wire prompt render acknowledgement".into()],
            ..WorkingMemorySnapshot::default()
        };
        session.working_memory.pending_working_memory_delta = Some(WorkingMemoryDelta {
            from_revision: 4,
            to_revision: 5,
            created_at_turn: 7,
            reason: crate::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
            changed_fields: vec!["current_plan".into()],
            summary_lines: vec![
                "updated the current plan with a long-form explanation of why prompt rendering acknowledgement must happen after budgeted assembly rather than before prompt construction".into(),
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
            active_work_item_id: Some("work_old".into()),
            delivery_target: Some("Refactor parser".into()),
            work_summary: Some("parser cleanup".into()),
            scope_hints: vec!["keep unrelated runtime behavior unchanged".into()],
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
            active_work_item_id: Some("work_runtime".into()),
            delivery_target: Some("Fix wake path".into()),
            work_summary: Some("wake path patching".into()),
            scope_hints: vec!["keep behavior unchanged outside the wake path".into()],
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
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Continue fixing the wake path in src/runtime.rs.".to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            active_work_item_id: Some("work_runtime".into()),
            delivery_target: Some("Fix wake path".into()),
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
        )
        .expect("relevant episode memory section should be present");

        assert!(episode_section.content.contains("ep_relevant"));
        assert!(episode_section.content.contains("src/runtime.rs"));
        assert!(episode_section
            .content
            .contains("keep behavior unchanged outside the wake path"));
        assert!(!episode_section.content.contains("ep_old"));
    }

    #[test]
    fn build_context_enforces_total_prompt_budget_across_large_sections() {
        let dir = tempdir().unwrap();
        let storage = AppStorage::new(dir.path()).unwrap();

        let current_message = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator { actor_id: None },
            TrustLevel::TrustedOperator,
            Priority::Normal,
            MessageBody::Text {
                text: "Keep the current input visible while trimming oversized context sections."
                    .to_string(),
            },
        );

        let mut session = AgentState::new("default");
        session.working_memory.current_working_memory = WorkingMemorySnapshot {
            delivery_target: Some("Stabilize prompt budgeting".into()),
            work_summary: Some("Trim oversized context sections before append".into()),
            scope_hints: vec![
                "scope hint one repeats enough text to force truncation".repeat(8),
                "scope hint two repeats enough text to force truncation".repeat(8),
            ],
            current_plan: vec![
                "inspect pre-turn sections for hard budget compliance".repeat(8),
                "retain current input after oversized section truncation".repeat(8),
            ],
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

        let mut active_work_item = crate::types::WorkItemRecord::new(
            "default",
            "Stabilize prompt budgeting",
            WorkItemStatus::Active,
        );
        active_work_item.summary =
            Some("current work item summary repeats to guarantee it needs truncation ".repeat(12));
        active_work_item.progress_note = Some(
            "progress note repeats to guarantee it competes with other large sections ".repeat(12),
        );
        storage.append_work_item(&active_work_item).unwrap();
        session
            .working_memory
            .current_working_memory
            .active_work_item_id = Some(active_work_item.id.clone());

        let skills = crate::types::SkillsRuntimeView {
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
