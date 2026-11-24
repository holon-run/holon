//! Prompt assembly over an already qualified, captured view.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use holon_domain::{
    execution_protocol::ExecutionAttempt, execution_types::ExecutionSnapshot,
    work_item_scheduling::WorkItemSchedulingProjection, AgentState, CallbackDeliveryMode,
    ContextConfig, ContextEpisodeRecord, ContinuationResolution, ExternalTriggerRecord,
    IdleInputBatchItem, LiveInputBatchRecord, MessageEnvelope, SkillsRuntimeView, TaskRecord,
    TurnRecord, WaitConditionRecord, WorkItemRecord, WorkItemRefStatus,
};
use holon_system::execution_policy_summary_lines;

use crate::{
    budget::estimate_section_tokens,
    episodes::{build_relevant_episode_memory_section, recent_turn_window_start},
    history::{
        compare_message_recency, latest_trusted_operator_input, render_captured_turns,
        CapturedHistory,
    },
    planner::{
        plan_context, ContextCandidate, ContextCandidatePolicy, ContextPlan, ContextPlanOutcome,
        DropTier, RetentionPriority,
    },
    projection_eval::{
        ProjectionEvidenceIndex, ProjectionEvidenceRef, ProjectionEvidenceRole, ProjectionOwner,
    },
    records::CapturedRecords,
    render::{
        continuation::{
            continuation_context, render_continuation_anchor, render_message_body_for_anchor,
            render_wake_hint_context,
        },
        input_batches::{render_batch, BATCH_HEADER},
        message_body_text, message_header, render_current_input_section, section,
        tasks::render_active_tasks,
        turn_section,
        work_items::{
            render_compact_current_work_item, render_current_work_item,
            render_current_work_item_process_trace, render_current_work_refs,
            render_empty_current_work_item, render_work_item_candidates, CompletionReport,
            ContextWorkQueue, PlanArtifactLookup,
        },
    },
    PromptSection,
};

type OperatorInputLookup<'a> = dyn Fn(&str) -> CapturedRecords<'a, MessageEnvelope> + 'a;

/// The renderer can only borrow captured data. The callbacks adapt already
/// captured views or format values using the caller's existing policies.
pub struct ContextAssemblyInput<'a> {
    pub admission_planning: bool,
    pub config: &'a ContextConfig,
    pub agent: &'a AgentState,
    pub execution: &'a ExecutionSnapshot,
    pub skills: &'a SkillsRuntimeView,
    pub current_message: &'a MessageEnvelope,
    pub continuation: Option<&'a ContinuationResolution>,
    pub recovery_source: Option<RecoveryContext<'a>>,
    pub continuation_anchor_messages: &'a [MessageEnvelope],
    pub history: CapturedHistory<'a>,
    pub active_wait_conditions: &'a [WaitConditionRecord],
    pub episodes: &'a [std::sync::Arc<ContextEpisodeRecord>],
    pub work_queue: &'a ContextWorkQueue,
    pub current_work_item: Option<&'a WorkItemRecord>,
    pub default_ingress: Option<&'a ExternalTriggerRecord>,
    pub active_task_count: usize,
    pub active_tasks: &'a [TaskRecord],
    pub plan_observations: &'a dyn PlanArtifactLookup,
    pub notes: Option<&'a str>,
    pub current_turn: Option<&'a TurnRecord>,
    pub current_input_projection: Option<&'a IdleInputBatchItem>,
    pub completion_report: &'a dyn Fn(&'a WorkItemRecord) -> Option<CompletionReport<'a>>,
    pub render_pending_task_results: &'a dyn Fn() -> Option<String>,
    pub operator_inputs: &'a OperatorInputLookup<'a>,
    pub command_preview: fn(&str) -> String,
    pub callback_url: fn(&str, &CallbackDeliveryMode, &str) -> String,
    pub current_work_scheduling:
        fn(&WorkItemRecord, &[WaitConditionRecord]) -> WorkItemSchedulingProjection,
    pub max_active_work_refs: usize,
}

#[derive(Clone, Copy)]
pub struct RecoveryContext<'a> {
    pub root_message: &'a MessageEnvelope,
    pub predecessor_attempt_id: &'a str,
    pub predecessors: &'a [ExecutionAttempt],
    pub source_turns: &'a [TurnRecord],
}

impl RecoveryContext<'_> {
    fn source_turn(&self) -> &TurnRecord {
        self.source_turns
            .last()
            .expect("validated provider recovery lineage is nonempty")
    }
}

#[derive(Debug)]
pub struct ContextAssembly {
    pub plan: ContextPlan,
    pub projection_evidence: ProjectionEvidenceIndex,
    pub recent_turns_budget: Option<usize>,
    pub admission_tokens: usize,
}

/// Assemble a Prompt from the observations already captured by the runtime.
pub fn assemble_context(
    data: &ContextAssemblyInput<'_>,
    budget: usize,
    input_admission: bool,
) -> Result<ContextAssembly> {
    anyhow::ensure!(
        input_admission || !data.admission_planning,
        "admission planning snapshots cannot be used as an execution prompt"
    );
    anyhow::ensure!(
        budget <= data.config.prompt_budget_estimated_tokens,
        "context budget expansion requires a newly prepared snapshot"
    );
    let mut config = data.config.clone();
    config.prompt_budget_estimated_tokens = budget;
    let config = &config;
    let agent = data.agent;
    let execution = data.execution;
    let skills = data.skills;
    let current_message = data.current_message;
    let continuation = data.continuation;
    let recovery_source = data.recovery_source;
    let continuation_anchor_messages = data.continuation_anchor_messages;
    let continuation_anchor_operator =
        latest_trusted_operator_input(continuation_anchor_messages, current_message);
    let CapturedHistory {
        turn_records,
        messages,
        briefs,
        tools,
        transcript,
        observations,
    } = data.history;
    let active_wait_conditions = data.active_wait_conditions;
    let episodes = data.episodes;
    let work_queue_projection = data.work_queue;
    let current_work_item = data.current_work_item;
    let default_ingress = data.default_ingress;
    let active_task_count = data.active_task_count;
    let active_tasks = data.active_tasks;
    let plan_observations = data.plan_observations;
    let notes_content = data.notes;
    let current_turn = data.current_turn;
    let mut seen_messages: BTreeSet<_> = recovery_source
        .iter()
        .map(|source| source.root_message.id.clone())
        .collect();
    let mut candidates = Vec::new();
    candidates.push(context_candidate(
        section("agent", format!("Agent id: {}", agent.id)),
        None,
        false,
    ));
    if let Some(default_ingress) = default_ingress {
        candidates.push(context_candidate(
            section(
                "default_external_ingress",
                render_default_external_ingress(
                    &config.callback_base_url,
                    default_ingress,
                    data.callback_url,
                ),
            ),
            None,
            false,
        ));
    }
    let execution_summary = execution_policy_summary_lines(execution).join("\n");
    candidates.push(context_candidate(
        section(
            "execution_environment",
            format!(
                "Execution environment summary (policy snapshot; host-local is not a strong sandbox guarantee):\n\
                 {}",
                execution_summary,
            ),
        ),
        None,
        false,
    ));
    if active_task_count > 0 {
        candidates.push(context_candidate(
            section(
                "active_tasks",
                render_active_tasks(active_tasks, active_task_count, data.command_preview),
            ),
            None,
            false,
        ));
    }

    if let Some(work_item) = current_work_item {
        candidates.push(context_candidate(
            turn_section(
                "current_work_item",
                render_current_work_item(
                    work_item,
                    &(data.current_work_scheduling)(work_item, active_wait_conditions),
                    plan_observations.plan(&work_item.id),
                    active_wait_conditions,
                ),
            ),
            Some(turn_section(
                "current_work_item",
                render_compact_current_work_item(work_item),
            )),
            true,
        ));
        if let Some(content) = render_current_work_refs(work_item, data.max_active_work_refs) {
            candidates.push(context_candidate(
                turn_section("current_work_refs", content),
                None,
                false,
            ));
        }
    } else {
        candidates.push(context_candidate(
            turn_section("current_work_item", render_empty_current_work_item()),
            None,
            false,
        ));
    }

    let recent_turn_window_start = recent_turn_window_start(turn_records.iter());
    if let Some(section) = build_relevant_episode_memory_section(
        episodes,
        agent,
        current_work_item,
        current_message,
        config,
        config.prompt_budget_estimated_tokens,
        recent_turn_window_start,
    ) {
        candidates.push(context_candidate(section, None, false));
    }

    if let Some(work_item) = current_work_item {
        if let Some(content) =
            render_current_work_item_process_trace(work_item, briefs, tools, transcript)
        {
            candidates.push(context_candidate(
                turn_section("current_work_item_process_trace", content),
                None,
                false,
            ));
        }
    }

    if let Some(content) = render_work_item_candidates(
        work_queue_projection,
        current_work_item.map(|item| item.id.as_str()),
        data.completion_report,
        plan_observations,
    ) {
        candidates.push(context_candidate(
            turn_section("queued_blocked_work_items", content),
            None,
            false,
        ));
    }

    if let Some(worktree) = &agent.worktree_session {
        candidates.push(context_candidate(
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
            None,
            false,
        ));
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
        candidates.push(context_candidate(
            section(
                "skills_catalog",
                format!(
                    "Discovered skills catalog (same-name precedence: workspace > agent > user; lower-precedence duplicates are omitted):\n{rendered}"
                ),
            ),
            None,
            false,
        ));
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
        candidates.push(context_candidate(
            section(
                "active_skills",
                format!("Active skills (same-name precedence follows skills_catalog):\n{rendered}"),
            ),
            None,
            false,
        ));
    }

    if let Some(notes_content) = notes_content {
        candidates.push(context_candidate(
            section("agent_home_notes_catalog", notes_content.to_owned()),
            None,
            false,
        ));
    }

    candidates.push(context_candidate(
        section(
            "context_contract",
            "Interpret the memory block with this priority: current work item objective first, durable plan artifact second, todo_list third, and current work refs after that. This is an interpretation priority, not a guarantee about section ordering. Use prior briefs and recent tool results as continuity evidence across turns. When sources differ on task scope, treat the current work item's `objective` and plan artifact as the ground truth unless the current input explicitly changes it."
                .to_string(),
        ),
        None,
        false,
    ));

    if let Some(source) = recovery_source {
        let root = &source.root_message;
        let root_projection = source
            .predecessors
            .iter()
            .filter(|attempt| attempt.source_message_id.as_deref() == Some(root.id.as_str()))
            .find_map(|attempt| attempt.input_projection.as_ref())
            .or_else(|| {
                source
                    .source_turns
                    .iter()
                    .filter_map(|turn| turn.idle_input_batch.as_ref())
                    .flat_map(|batch| &batch.items)
                    .find(|item| item.message_id == root.id)
            });
        let root_content = root_projection
            .map(|item| item.rendered.clone())
            .unwrap_or_else(|| {
                format!(
                    "{}\n{}",
                    message_header(root),
                    render_message_body_for_anchor(&root.body),
                )
            });
        let anchor = format!(
            "Provider recovery: continue the task of source turn {}, predecessor attempt {}. The current focus is not the recovery authority.\nRoot task input {} (original provenance):\n{}",
            source.source_turn().turn_id,
            source.predecessor_attempt_id,
            root.id,
            root_content,
        );
        candidates.push(context_candidate(
            turn_section("continuation_anchor", anchor.clone()),
            Some(turn_section("continuation_anchor", anchor)),
            true,
        ));
    } else if let Some(anchor) = render_continuation_anchor(
        continuation_anchor_operator,
        current_message,
        continuation,
        current_work_item,
        config.prompt_budget_estimated_tokens,
    ) {
        let compact_anchor = render_continuation_anchor(
            continuation_anchor_operator,
            current_message,
            continuation,
            current_work_item,
            48,
        )
        .expect("full continuation anchor implies compact continuation anchor");
        candidates.push(context_candidate(
            turn_section("continuation_anchor", anchor),
            Some(turn_section("continuation_anchor", compact_anchor)),
            true,
        ));
    }

    let recent_turns_budget = config
        .prompt_budget_estimated_tokens
        .min(config.turn_projection_budget());
    let recent_turns_compact_budget = recent_turns_budget.min(128);
    let projection_evidence = build_projection_evidence(
        data.history,
        agent,
        current_message,
        current_work_item,
        data.max_active_work_refs,
    );
    if let Some(content) = render_captured_turns(
        observations,
        turn_records,
        messages,
        briefs,
        tools,
        transcript,
        current_message,
        current_work_item,
        recent_turns_budget,
    ) {
        let compact = render_captured_turns(
            observations,
            turn_records,
            messages,
            briefs,
            tools,
            transcript,
            current_message,
            current_work_item,
            recent_turns_compact_budget,
        )
        .map(|content| turn_section("recent_turns", content));
        candidates.push(context_candidate(
            turn_section("recent_turns", content),
            compact,
            false,
        ));
    }

    let continuation_present =
        if let Some(content) = continuation_context(current_message, continuation) {
            candidates.push(context_candidate(
                turn_section("continuation_context", content),
                None,
                false,
            ));
            true
        } else {
            false
        };

    let wake_hint_fallback = (!continuation_present)
        .then(|| render_wake_hint_context(current_message))
        .flatten();
    if let Some(content) = (data.render_pending_task_results)() {
        let section = turn_section("pending_task_results", content);
        candidates.push(context_candidate(section.clone(), Some(section), true));
    }
    let batch = current_turn
        .and_then(|turn| turn.idle_input_batch.as_ref())
        .filter(|batch| batch.anchor_message_id == current_message.id);
    let (current_input_full, current_input_compact) = if let Some(batch) = batch {
        let mut visible = batch.clone();
        visible
            .items
            .retain(|item| seen_messages.insert(item.message_id.clone()));
        let section = turn_section("current_input", render_batch(&visible));
        // Already admitted evidence cannot be dropped to make room for new input.
        (section.clone(), section)
    } else if let Some(item) = data.current_input_projection {
        seen_messages.insert(current_message.id.clone());
        let section = turn_section(
            "current_input",
            format!("{}{}", BATCH_HEADER, item.rendered),
        );
        (section.clone(), section)
    } else {
        seen_messages.insert(current_message.id.clone());
        (
            render_current_input_section(
                current_message,
                config.prompt_budget_estimated_tokens,
                wake_hint_fallback.as_deref(),
            ),
            render_current_input_section(current_message, 48, wake_hint_fallback.as_deref()),
        )
    };
    if !input_admission {
        candidates.push(context_candidate(
            current_input_full,
            Some(current_input_compact),
            true,
        ));
    }
    if let Some(source) = recovery_source {
        let mut parts = Vec::new();
        for turn in source.source_turns {
            if let Some(content) =
                render_recovery_input_batches(data.operator_inputs, turn, &mut seen_messages)?
            {
                parts.push(content);
            }
        }
        if !parts.is_empty() {
            let section = turn_section("recovery_input_batches", parts.join("\n"));
            candidates.push(context_candidate(section.clone(), Some(section), true));
        }
    }
    if let Some(turn) = current_turn {
        if let Some(content) =
            render_live_input_batches(&turn.live_input_batches, &mut seen_messages)
        {
            let section = turn_section("current_live_input_batches", content);
            candidates.push(context_candidate(section.clone(), Some(section), true));
        }
        if turn.replay.is_some() {
            if let Some(content) =
                render_operator_inputs(data.operator_inputs, turn, &mut seen_messages)?
            {
                let section = turn_section("recovered_operator_inputs", content);
                candidates.push(context_candidate(section.clone(), Some(section), true));
            }
        }
    }

    let pinned_tokens = candidates
        .iter()
        .filter(|candidate| candidate.policy.pinned)
        .map(|candidate| {
            estimate_section_tokens(candidate.compact.as_ref().unwrap_or(&candidate.full))
        })
        .sum::<usize>();
    let admission_tokens = config
        .prompt_budget_estimated_tokens
        .saturating_sub(pinned_tokens);
    let plan = plan_context(candidates, config.prompt_budget_estimated_tokens)?;
    let recent_turns_budget = if turn_records.is_empty() {
        None
    } else {
        plan.evidence
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == "recent_turns")
            .and_then(|decision| match decision.outcome {
                ContextPlanOutcome::Full => Some(recent_turns_budget),
                ContextPlanOutcome::Compact => Some(recent_turns_compact_budget),
                ContextPlanOutcome::Truncated => Some(decision.allocated_estimated_tokens),
                _ => None,
            })
    };
    Ok(ContextAssembly {
        plan,
        projection_evidence,
        recent_turns_budget,
        admission_tokens,
    })
}

/// Render already prepared history at the caller's selected budget.
pub fn reproject_recent_turns(
    history: CapturedHistory<'_>,
    current_message: &MessageEnvelope,
    current_work_item: Option<&WorkItemRecord>,
    budget: usize,
) -> Option<PromptSection> {
    render_captured_turns(
        history.observations,
        history.turn_records,
        history.messages,
        history.briefs,
        history.tools,
        history.transcript,
        current_message,
        current_work_item,
        budget,
    )
    .map(|content| turn_section("recent_turns", content))
}

pub struct ScopedHistoryProjection {
    pub section: PromptSection,
    pub evidence: Vec<ProjectionEvidenceRef>,
    pub turns_loaded: usize,
}

pub fn reproject_work_item_history(
    history: CapturedHistory<'_>,
    current_message: &MessageEnvelope,
    work_item: &WorkItemRecord,
    budget: usize,
) -> Option<ScopedHistoryProjection> {
    let section = reproject_recent_turns(history, current_message, Some(work_item), budget)?;
    let evidence = build_recent_turn_projection_evidence(
        history,
        ProjectionOwner::WorkItem {
            work_item_id: work_item.id.clone(),
        },
    );
    Some(ScopedHistoryProjection {
        section,
        evidence,
        turns_loaded: history.turn_records.iter().len(),
    })
}

fn context_candidate(
    full: PromptSection,
    compact: Option<PromptSection>,
    pinned: bool,
) -> ContextCandidate {
    let policy = context_candidate_policy(&full.id, pinned);
    ContextCandidate::new(full, compact, policy)
}

fn context_candidate_policy(id: &str, pinned: bool) -> ContextCandidatePolicy {
    let (priority, drop_tier, render_order) = match id {
        "agent" => (RetentionPriority::High, DropTier::Last, 10),
        "default_external_ingress" => (RetentionPriority::Normal, DropTier::Normal, 20),
        "execution_environment" => (RetentionPriority::High, DropTier::Last, 30),
        "active_tasks" => (RetentionPriority::High, DropTier::Late, 40),
        "current_work_item" => (RetentionPriority::Critical, DropTier::Last, 50),
        "current_work_refs" => (RetentionPriority::High, DropTier::Late, 60),
        "relevant_episode_memory" => (RetentionPriority::Normal, DropTier::Normal, 70),
        "current_work_item_process_trace" => (RetentionPriority::Normal, DropTier::Normal, 80),
        "queued_blocked_work_items" => (RetentionPriority::Normal, DropTier::Normal, 90),
        "worktree_session" => (RetentionPriority::Normal, DropTier::Late, 100),
        "skills_catalog" => (RetentionPriority::Low, DropTier::Early, 120),
        "active_skills" => (RetentionPriority::Normal, DropTier::Normal, 130),
        "agent_home_notes_catalog" => (RetentionPriority::Low, DropTier::Early, 140),
        "context_contract" => (RetentionPriority::High, DropTier::Last, 150),
        "continuation_anchor" => (RetentionPriority::Critical, DropTier::Last, 160),
        "recent_turns" => (RetentionPriority::High, DropTier::Late, 170),
        "continuation_context" => (RetentionPriority::High, DropTier::Last, 180),
        "pending_task_results" => (RetentionPriority::Critical, DropTier::Last, 185),
        "current_input" => (RetentionPriority::Critical, DropTier::Last, 190),
        "recovery_input_batches" => (RetentionPriority::Critical, DropTier::Last, 192),
        "current_live_input_batches" => (RetentionPriority::Critical, DropTier::Last, 195),
        _ => (RetentionPriority::Normal, DropTier::Normal, u16::MAX),
    };
    ContextCandidatePolicy {
        pinned,
        priority,
        drop_tier,
        render_order,
    }
}

fn build_projection_evidence(
    history: CapturedHistory<'_>,
    agent: &AgentState,
    current_message: &MessageEnvelope,
    current_work_item: Option<&WorkItemRecord>,
    max_active_work_refs: usize,
) -> ProjectionEvidenceIndex {
    let CapturedHistory {
        observations,
        turn_records,
        messages,
        briefs,
        tools,
        ..
    } = history;
    let mut evidence = ProjectionEvidenceIndex::new();
    let current_owner = agent
        .current_execution_binding
        .as_ref()
        .and_then(|binding| binding.owner.as_ref())
        .map(ProjectionOwner::from)
        .unwrap_or_else(|| projection_owner(current_message.work_item_id.as_deref(), &agent.id));
    evidence.insert(
        "current_input".into(),
        vec![ProjectionEvidenceRef::new(
            format!("message:{}", current_message.id),
            ProjectionEvidenceRole::CurrentInput,
            current_owner,
        )],
    );
    if let Some(work_item) = current_work_item {
        let owner = projection_owner(Some(&work_item.id), &agent.id);
        let work_item_ref = ProjectionEvidenceRef::new(
            format!("work_item:{}", work_item.id),
            ProjectionEvidenceRole::WorkItemState,
            owner.clone(),
        );
        evidence.insert("current_work_item".into(), vec![work_item_ref]);
        let work_refs = work_item
            .work_refs
            .iter()
            .filter(|work_ref| work_ref.status == WorkItemRefStatus::Active)
            .take(max_active_work_refs)
            .map(|work_ref| {
                ProjectionEvidenceRef::new(
                    work_ref.ref_id.clone(),
                    ProjectionEvidenceRole::Supporting,
                    owner.clone(),
                )
            })
            .collect::<Vec<_>>();
        if !work_refs.is_empty() {
            evidence.insert("current_work_refs".into(), work_refs);
        }
    }

    let turn_owners = turn_records
        .iter()
        .map(|turn| (turn.turn_id.as_str(), turn.effective_owner()))
        .collect::<BTreeMap<_, _>>();
    let mut recent_turn_refs = turn_records
        .iter()
        .map(|turn| {
            ProjectionEvidenceRef::new(
                format!("turn:{}", turn.turn_id),
                ProjectionEvidenceRole::Turn,
                ProjectionOwner::from(&turn.effective_owner()),
            )
        })
        .collect::<Vec<_>>();
    let direct_predecessor_id = messages
        .iter()
        .max_by(|left, right| compare_message_recency(left, right))
        .map(|message| message.id.as_str());
    for message in messages.iter() {
        let owner = observations
            .assigned_turn_for_message(&message.id)
            .and_then(|turn_id| turn_owners.get(turn_id))
            .map(ProjectionOwner::from)
            .unwrap_or_else(|| projection_owner(message.work_item_id.as_deref(), &agent.id));
        recent_turn_refs.push(ProjectionEvidenceRef::new(
            format!("message:{}", message.id),
            if direct_predecessor_id == Some(message.id.as_str()) {
                ProjectionEvidenceRole::DirectPredecessor
            } else {
                ProjectionEvidenceRole::Input
            },
            owner,
        ));
    }
    for brief in briefs.iter() {
        let owner = brief
            .turn_id
            .as_deref()
            .and_then(|turn_id| turn_owners.get(turn_id))
            .map(ProjectionOwner::from)
            .unwrap_or_else(|| projection_owner(brief.work_item_id.as_deref(), &agent.id));
        recent_turn_refs.push(ProjectionEvidenceRef::new(
            format!("brief:{}", brief.id),
            ProjectionEvidenceRole::Result,
            owner,
        ));
    }
    for tool in tools.iter() {
        let owner = tool
            .turn_id
            .as_deref()
            .and_then(|turn_id| turn_owners.get(turn_id))
            .map(ProjectionOwner::from)
            .unwrap_or_else(|| projection_owner(tool.work_item_id.as_deref(), &agent.id));
        recent_turn_refs.push(ProjectionEvidenceRef::new(
            format!("tool_execution:{}", tool.id),
            ProjectionEvidenceRole::ToolResult,
            owner,
        ));
    }
    recent_turn_refs.sort();
    if !recent_turn_refs.is_empty() {
        evidence.insert("recent_turns".into(), recent_turn_refs);
    }
    evidence
}

pub fn build_recent_turn_projection_evidence(
    history: CapturedHistory<'_>,
    fallback_owner: ProjectionOwner,
) -> Vec<ProjectionEvidenceRef> {
    let CapturedHistory {
        observations,
        turn_records,
        messages,
        briefs,
        tools,
        ..
    } = history;
    let turn_owners = turn_records
        .iter()
        .map(|turn| (turn.turn_id.as_str(), turn.effective_owner()))
        .collect::<BTreeMap<_, _>>();
    let owner_for_turn = |turn_id: Option<&str>| {
        turn_id
            .and_then(|turn_id| turn_owners.get(turn_id))
            .map(ProjectionOwner::from)
            .unwrap_or_else(|| fallback_owner.clone())
    };
    let mut evidence = turn_records
        .iter()
        .map(|turn| {
            ProjectionEvidenceRef::new(
                format!("turn:{}", turn.turn_id),
                ProjectionEvidenceRole::Turn,
                ProjectionOwner::from(&turn.effective_owner()),
            )
        })
        .collect::<Vec<_>>();
    let direct_predecessor_id = messages
        .iter()
        .max_by(|left, right| compare_message_recency(left, right))
        .map(|message| message.id.as_str());
    evidence.extend(messages.iter().map(|message| {
        ProjectionEvidenceRef::new(
            format!("message:{}", message.id),
            if direct_predecessor_id == Some(message.id.as_str()) {
                ProjectionEvidenceRole::DirectPredecessor
            } else {
                ProjectionEvidenceRole::Input
            },
            owner_for_turn(observations.assigned_turn_for_message(&message.id)),
        )
    }));
    evidence.extend(briefs.iter().map(|brief| {
        ProjectionEvidenceRef::new(
            format!("brief:{}", brief.id),
            ProjectionEvidenceRole::Result,
            owner_for_turn(brief.turn_id.as_deref()),
        )
    }));
    evidence.extend(tools.iter().map(|tool| {
        ProjectionEvidenceRef::new(
            format!("tool_execution:{}", tool.id),
            ProjectionEvidenceRole::ToolResult,
            owner_for_turn(tool.turn_id.as_deref()),
        )
    }));
    evidence.sort();
    evidence.dedup();
    evidence
}

fn projection_owner(work_item_id: Option<&str>, agent_id: &str) -> ProjectionOwner {
    work_item_id
        .map(|work_item_id| ProjectionOwner::WorkItem {
            work_item_id: work_item_id.to_string(),
        })
        .unwrap_or_else(|| ProjectionOwner::LegacyUnbound {
            agent_id: agent_id.to_string(),
        })
}

fn render_default_external_ingress(
    callback_base_url: &str,
    record: &ExternalTriggerRecord,
    callback_url: fn(&str, &CallbackDeliveryMode, &str) -> String,
) -> String {
    let url = record
        .token
        .as_ref()
        .map_or("<unavailable>".to_string(), |token| {
            callback_url(callback_base_url, &record.delivery_mode, token)
        });
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

fn render_recovery_input_batches(
    operator_inputs: &OperatorInputLookup<'_>,
    turn: &TurnRecord,
    seen_messages: &mut BTreeSet<String>,
) -> Result<Option<String>> {
    let mut parts = Vec::new();
    if let Some(batch) = turn.idle_input_batch.as_ref() {
        let mut visible = batch.clone();
        visible
            .items
            .retain(|item| seen_messages.insert(item.message_id.clone()));
        if !visible.items.is_empty() {
            parts.push(render_batch(&visible));
        }
    }
    if let Some(content) = render_live_input_batches(&turn.live_input_batches, seen_messages) {
        parts.push(content);
    }
    if let Some(content) = render_operator_inputs(operator_inputs, turn, seen_messages)? {
        parts.push(content);
    }
    Ok((!parts.is_empty()).then(|| {
        format!(
            "Provider recovery evidence from source turn {} (original provenance):\n{}",
            turn.turn_id,
            parts.join("\n")
        )
    }))
}

fn render_operator_inputs(
    operator_inputs: &OperatorInputLookup<'_>,
    turn: &TurnRecord,
    seen_messages: &mut BTreeSet<String>,
) -> Result<Option<String>> {
    let parts = operator_inputs(&turn.turn_id)
        .iter()
        .filter(|input| seen_messages.insert(input.id.clone()))
        .map(|message| {
            let mut provenance = serde_json::to_value(message)?;
            provenance.as_object_mut().expect("message envelope is an object").remove("body");
            Ok(format!(
                "Recovered operator interjection (original admission and provenance):\nsource_ref: message:{}\nprovenance: {}\nbody:\n{}\n",
                message.id,
                serde_json::to_string(&provenance)?,
                message_body_text(&message.body),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((!parts.is_empty()).then(|| parts.join("\n")))
}

fn render_live_input_batches(
    batches: &[LiveInputBatchRecord],
    seen_messages: &mut BTreeSet<String>,
) -> Option<String> {
    let mut seen_batches = BTreeSet::new();
    let mut text = String::new();
    for batch in batches {
        if !seen_batches.insert(&batch.safe_point) {
            continue;
        }
        let items: Vec<_> = batch
            .items
            .iter()
            .filter(|item| seen_messages.insert(item.message_id.clone()))
            .cloned()
            .collect();
        if items.is_empty() {
            continue;
        }
        let mut visible = batch.clone();
        visible.items = items;
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&visible.rendered_text());
    }
    (!text.is_empty()).then_some(text)
}

fn scope_label(scope: &holon_domain::SkillScope) -> &'static str {
    match scope {
        holon_domain::SkillScope::UserGlobal => "user_global",
        holon_domain::SkillScope::Agent => "agent",
        holon_domain::SkillScope::Workspace => "workspace",
    }
}

fn activation_source_label(source: holon_domain::SkillActivationSource) -> &'static str {
    match source {
        holon_domain::SkillActivationSource::Explicit => "explicit",
        holon_domain::SkillActivationSource::ImplicitFromCatalog => "implicit_from_catalog",
        holon_domain::SkillActivationSource::Restored => "restored",
        holon_domain::SkillActivationSource::Inherited => "inherited",
    }
}

fn activation_state_label(state: holon_domain::SkillActivationState) -> &'static str {
    match state {
        holon_domain::SkillActivationState::TurnActive => "turn_active",
        holon_domain::SkillActivationState::SessionActive => "session_active",
    }
}

#[cfg(test)]
mod tests;
