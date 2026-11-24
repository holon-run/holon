use super::*;
use crate::history::{ProjectionHistoryObservations, TurnHistoryObservations};
use holon_domain::{
    work_item_scheduling::{WorkItemCandidateClass, WorkItemFocus, WorkItemSchedulingReasonCode},
    AdmissionContext, AuthorityClass, BriefRecord, ContinuationClass, ContinuationTriggerKind,
    MessageBody, MessageDeliverySurface, MessageKind, MessageOrigin, Priority, WorkItemReadiness,
    WorkItemSchedulingState, WorkingMemorySnapshot,
};
use serde_json::json;
use std::path::PathBuf;

#[derive(Default)]
struct Fixture {
    work_queue: ContextWorkQueue,
    plans: BTreeMap<String, holon_domain::WorkItemPlanArtifact>,
    turn_records: Vec<TurnRecord>,
    messages: Vec<MessageEnvelope>,
}

impl TurnHistoryObservations for Fixture {
    fn message_belongs_to_turn(&self, turn: &TurnRecord, id: &str) -> bool {
        self.assigned_turn_for_message(id) == Some(turn.turn_id.as_str())
    }

    fn brief_text<'a>(&'a self, brief: &'a BriefRecord) -> &'a str {
        &brief.text
    }

    fn trigger_summary_valid(&self, _: &TurnRecord) -> bool {
        true
    }
}

impl ProjectionHistoryObservations for Fixture {
    fn assigned_turn_for_message(&self, message_id: &str) -> Option<&str> {
        self.messages
            .iter()
            .find(|message| message.id == message_id)?
            .turn_id
            .as_deref()
    }
}

impl Fixture {
    fn input<'a>(
        &'a self,
        agent: &'a AgentState,
        execution: &'a ExecutionSnapshot,
        skills: &'a SkillsRuntimeView,
        current_message: &'a MessageEnvelope,
        continuation: Option<&'a ContinuationResolution>,
        config: &'a ContextConfig,
    ) -> ContextAssemblyInput<'a> {
        ContextAssemblyInput {
            admission_planning: false,
            config,
            agent,
            execution,
            skills,
            current_message,
            continuation,
            recovery_source: None,
            continuation_anchor_messages: &self.messages,
            history: CapturedHistory {
                observations: self,
                turn_records: CapturedRecords::new(&self.turn_records),
                messages: CapturedRecords::new(&self.messages),
                briefs: CapturedRecords::empty(),
                tools: CapturedRecords::empty(),
                transcript: CapturedRecords::empty(),
            },
            active_wait_conditions: &[],
            episodes: &[],
            work_queue: &self.work_queue,
            current_work_item: self.work_queue.current.as_ref(),
            default_ingress: None,
            active_task_count: 0,
            active_tasks: &[],
            plan_observations: &self.plans,
            notes: None,
            current_turn: None,
            current_input_projection: None,
            completion_report: &|_| None,
            render_pending_task_results: &|| None,
            operator_inputs: &|_| CapturedRecords::empty(),
            command_preview: |_| panic!("unexpected command preview"),
            callback_url: |_, _, _| panic!("unexpected callback URL"),
            current_work_scheduling: |item, _| WorkItemSchedulingProjection {
                work_item: item.clone(),
                scheduling_state: WorkItemSchedulingState::Runnable,
                readiness: WorkItemReadiness::Runnable,
                candidate_class: WorkItemCandidateClass::CurrentRunnable,
                focus: WorkItemFocus::Current,
                is_current: true,
                is_runnable: true,
                has_active_waits: false,
                has_active_task_waits: false,
                has_triggered_waits: false,
                last_triggered_at: None,
                current_todo: None,
                active_wait_conditions: Vec::new(),
                reason_code: WorkItemSchedulingReasonCode::Runnable,
                diagnostics: Vec::new(),
            },
            max_active_work_refs: 12,
        }
    }

    fn build_context(
        &self,
        agent: &AgentState,
        execution: &ExecutionSnapshot,
        skills: &SkillsRuntimeView,
        message: &MessageEnvelope,
        continuation: Option<&ContinuationResolution>,
        config: &ContextConfig,
    ) -> Result<ContextAssembly> {
        assemble_context(
            &self.input(agent, execution, skills, message, continuation, config),
            config.prompt_budget_estimated_tokens,
            false,
        )
    }
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
            holon_domain::execution_types::WorkspaceProjectionKind::GitWorktreeRoot
        } else {
            holon_domain::execution_types::WorkspaceProjectionKind::CanonicalRoot
        }),
        access_mode: Some(if session.worktree_session.is_some() {
            holon_domain::execution_types::WorkspaceAccessMode::ExclusiveWrite
        } else {
            holon_domain::execution_types::WorkspaceAccessMode::SharedRead
        }),
        worktree_root: session
            .worktree_session
            .as_ref()
            .map(|worktree| worktree.worktree_path.clone()),
        execution_roots: Vec::new(),
    }
}

#[test]
fn build_context_omits_working_memory_sections_by_default() {
    let fixture = Fixture::default();

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "continue".to_string(),
        },
    );

    let mut session = AgentState::new("default");
    session.working_memory.current_working_memory = WorkingMemorySnapshot {
        objective: Some("ship working memory".into()),
        plan: Some(vec!["[InProgress] wire post-turn refresh"].join("\n")),
        ..WorkingMemorySnapshot::default()
    };

    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
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

    assert!(!built
        .plan
        .sections
        .iter()
        .any(|section| section.name == "working_memory"));
    assert!(!built
        .plan
        .sections
        .iter()
        .any(|section| section.name == "working_memory_delta"));
    assert!(!built
        .plan
        .sections
        .iter()
        .any(|section| section.name == "compacted_summary"));
}

#[test]
fn build_context_does_not_emit_follow_up_specific_contracts() {
    let fixture = Fixture::default();

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Analyze the runtime architecture and suggest next steps.".to_string(),
        },
    );

    let session = AgentState::new("default");
    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
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
        .plan
        .sections
        .iter()
        .all(|section| section.name != "follow_up_context_contract"));

    let context_contract = built
        .plan
        .sections
        .iter()
        .find(|section| section.name == "context_contract")
        .expect("context contract section should be present");
    assert!(context_contract
        .content
        .contains("Use prior briefs and recent tool results"));
    let context_contract_index = built
        .plan
        .sections
        .iter()
        .position(|section| section.name == "context_contract")
        .expect("context contract index should be present");
    let current_input_index = built
        .plan
        .sections
        .iter()
        .position(|section| section.name == "current_input")
        .expect("current input index should be present");
    assert!(context_contract_index < current_input_index);
}

#[test]
fn build_context_lists_skill_metadata_without_skill_body() {
    let fixture = Fixture::default();

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
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
        catalog_only_roots: Vec::new(),
        discoverable_skills: vec![
            holon_domain::SkillCatalogEntry {
                skill_id: "agent:demo".into(),
                root_id: "agent_home:test-root".into(),
                skill_dir: "demo".into(),
                name: "demo".into(),
                description: "agent demo skill summary".into(),
                path: PathBuf::from("/tmp/agent/skills/demo/SKILL.md"),
                scope: holon_domain::SkillScope::Agent,
            },
            holon_domain::SkillCatalogEntry {
                skill_id: "workspace:other".into(),
                root_id: "workspace:test-root".into(),
                skill_dir: "other".into(),
                name: "other".into(),
                description: "other skill summary".into(),
                path: PathBuf::from("/tmp/workspace/.agents/skills/other/SKILL.md"),
                scope: holon_domain::SkillScope::Workspace,
            },
        ]
        .into(),
        attached_skills: Default::default(),
        active_skills: vec![holon_domain::ActiveSkillRecord {
            skill_id: "agent:demo".into(),
            name: "demo".into(),
            path: PathBuf::from("/tmp/agent/skills/demo/SKILL.md"),
            scope: holon_domain::SkillScope::Agent,
            agent_id: "default".into(),
            activation_source: holon_domain::SkillActivationSource::ImplicitFromCatalog,
            activation_state: holon_domain::SkillActivationState::SessionActive,
            activated_at_turn: 2,
        }],
    };

    let built = fixture
        .build_context(
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
        .plan
        .sections
        .iter()
        .find(|section| section.name == "skills_catalog")
        .expect("skills_catalog section should be present");
    assert!(catalog
        .content
        .contains("same-name precedence: workspace > agent > user"));
    assert!(catalog.content.contains("agent demo skill summary"));
    assert!(catalog.content.contains("/tmp/agent/skills/demo/SKILL.md"));
    assert!(catalog.content.contains("other skill summary"));
    assert!(!catalog.content.contains("Follow the demo workflow."));

    let active = built
        .plan
        .sections
        .iter()
        .find(|section| section.name == "active_skills")
        .expect("active_skills section should be present");
    assert!(active
        .content
        .contains("same-name precedence follows skills_catalog"));
    assert!(active.content.contains("agent:demo"));
    assert!(active.content.contains("session_active"));
}

#[test]
fn build_context_omits_agent_template_catalog_section() {
    let fixture = Fixture::default();

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Use a helper template".to_string(),
        },
    );

    let session = AgentState::new("default");
    let skills = SkillsRuntimeView {
        agent_templates_catalog: vec![holon_domain::AgentTemplateCatalogEntry {
            catalog_id: "builtin:demo".into(),
            template: "demo".into(),
            template_id: "demo".into(),
            source: holon_domain::AgentTemplateSourceKind::Builtin,
            path: None,
            name: "Demo".into(),
            schema_version: Some("holon.agent_template.v1".into()),
            description: "demo template summary".into(),
            included_skills: vec!["ghx".into()],
            source_url: None,
            source_id: None,
            resolved_ref: None,
            resolved_revision: None,
        }],
        ..SkillsRuntimeView::default()
    };

    let built = fixture
        .build_context(
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

    // The template catalog is a low-frequency lookup. It is served by
    // `holon template catalog|show` (and Web/TUI/API), not injected into the
    // prompt, so the captured SkillsRuntimeView data must stay out of context.
    assert!(built
        .plan
        .sections
        .iter()
        .all(|section| section.name != "agent_templates_catalog"));
    assert!(skills
        .agent_templates_catalog
        .iter()
        .any(|entry| entry.catalog_id == "builtin:demo"));
}

#[test]
fn build_context_includes_worktree_session_when_active() {
    let fixture = Fixture::default();

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Make changes in the worktree".to_string(),
        },
    );

    let mut session = AgentState::new("default");
    session.worktree_session = Some(holon_domain::WorktreeSession {
        original_cwd: PathBuf::from("/original/repo"),
        original_branch: "main".to_string(),
        worktree_path: PathBuf::from("/tmp/worktree-feature-branch"),
        worktree_branch: "feature-branch".to_string(),
    });

    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
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
        .plan
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
fn build_context_omits_worktree_session_when_not_active() {
    let fixture = Fixture::default();

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Make changes".to_string(),
        },
    );

    let session = AgentState::new("default");

    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
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
        .plan
        .sections
        .iter()
        .all(|section| section.name != "worktree_session"));
}

#[test]
fn build_context_includes_execution_environment() {
    let fixture = Fixture::default();

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Inspect the environment".to_string(),
        },
    );
    let session = AgentState::new("default");

    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
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
        .plan
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
fn build_context_includes_continuation_context_for_system_tick() {
    let fixture = Fixture::default();

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
    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
            &system_tick,
            Some(&ContinuationResolution {
                trigger_kind: holon_domain::ContinuationTriggerKind::SystemTick,
                class: ContinuationClass::ResumeExpectedWait,
                model_reentry: true,
                prior_closure_outcome: holon_domain::ClosureOutcome::Waiting,
                prior_waiting_reason: Some(holon_domain::WaitingReason::AwaitingExternalChange),
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
        .plan
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
    let fixture = Fixture::default();

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
    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
            &system_tick,
            Some(&ContinuationResolution {
                trigger_kind: holon_domain::ContinuationTriggerKind::SystemTick,
                class: ContinuationClass::LocalContinuation,
                model_reentry: true,
                prior_closure_outcome: holon_domain::ClosureOutcome::Completed,
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
        .plan
        .sections
        .iter()
        .find(|section| section.name == "continuation_context")
        .expect("continuation_context section should be present");
    assert!(activation.content.contains("continue_active"));
    assert!(activation.content.contains("work_123"));
    assert!(activation.content.contains("fix stale pid handling"));
}

#[test]
fn build_context_marks_fresh_operator_input_as_the_only_new_task() {
    let mut fixture = Fixture::default();

    let mut prior_operator = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue the old database investigation.".to_string(),
        },
    );
    let mut prior_turn = TurnRecord::new("default", "turn-old-topic", 1);
    prior_operator.turn_id = Some(prior_turn.turn_id.clone());
    prior_operator.message_seq = Some(1);
    prior_turn.input_message_ids = vec![prior_operator.id.clone()];
    fixture.messages.push(prior_operator);
    fixture.turn_records.push(prior_turn);

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Interject,
        MessageBody::Text {
            text: "Push the documentation update.".to_string(),
        },
    );
    let session = AgentState::new("default");
    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig::default(),
        )
        .unwrap();

    let anchor = built
        .plan
        .sections
        .iter()
        .find(|section| section.name == "continuation_anchor")
        .expect("continuation_anchor section should be present");
    assert!(anchor.content.contains("the only new operator task"));
    assert!(anchor.content.contains("historical/background evidence"));
    assert!(anchor
        .content
        .contains("do not resume old tasks or waits unless current_input or the current WorkItem"));
}

#[test]
fn build_context_keeps_full_current_input_body_for_long_operator_prompt() {
    let fixture = Fixture::default();

    let long_path =
        "/very/long/path/to/a/workspace/projects/example/agentinbox/operator-guides/agentinbox-cli-quickstart-for-review-workflow-2026-04.md";
    let long_prompt =
        format!("Read this file first and follow it exactly: {long_path}\n\nDo not invent APIs.");
    let operator_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: long_prompt.clone(),
        },
    );

    let session = AgentState::new("default");
    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
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
        .plan
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
    let fixture = Fixture::default();
    let session = AgentState::new("default");
    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
            actor_display_name: None,
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

    let built = fixture
        .build_context(
            &session,
            &execution_snapshot_for(&session),
            &holon_domain::SkillsRuntimeView::default(),
            &current_message,
            None,
            &ContextConfig::default(),
        )
        .unwrap();

    let current_input = built
        .plan
        .sections
        .iter()
        .find(|section| section.name == "current_input")
        .expect("current_input section should be present");
    assert!(current_input
        .content
        .contains("[operator][cli_prompt][local_process][operator_instruction][OperatorPrompt]"));
}

#[test]
fn build_context_preserves_pinned_truth_at_exact_minimum_and_fails_below_it() {
    let mut fixture = Fixture::default();

    let mut operator_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Preserve this trusted operator intent for the active implementation.".repeat(8),
        },
    );
    operator_message.message_seq = Some(1);
    fixture.messages.push(operator_message);

    let active = holon_domain::WorkItemRecord::new(
        "default",
        "Implement the deterministic context budget planner without losing durable truth."
            .repeat(6),
        holon_domain::WorkItemState::Open,
    );
    let mut agent = AgentState::new("default");
    agent.current_work_item_id = Some(active.id.clone());
    fixture.work_queue.current = Some(active);

    let mut system_tick = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "work_queue".to_string(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue the focused work item after a runtime scheduling wake.".repeat(8),
        },
    );
    system_tick.trigger_kind = Some(ContinuationTriggerKind::SystemTick);
    let continuation = ContinuationResolution {
        trigger_kind: ContinuationTriggerKind::SystemTick,
        class: ContinuationClass::LocalContinuation,
        model_reentry: true,
        prior_closure_outcome: holon_domain::ClosureOutcome::Continuable,
        prior_waiting_reason: None,
        matched_waiting_reason: false,
        evidence: vec![],
    };

    let build = |budget| {
        fixture.build_context(
            &agent,
            &execution_snapshot_for(&agent),
            &holon_domain::SkillsRuntimeView::default(),
            &system_tick,
            Some(&continuation),
            &ContextConfig {
                recent_messages: 4,
                recent_briefs: 4,
                prompt_budget_estimated_tokens: budget,
                ..ContextConfig::default()
            },
        )
    };

    let initial = build(4096).unwrap();
    let pinned_ids = ["current_input", "current_work_item", "continuation_anchor"];
    let pinned_minimum = initial
        .plan
        .evidence
        .decisions
        .iter()
        .filter(|decision| pinned_ids.contains(&decision.candidate_id.as_str()))
        .map(|decision| decision.minimum_estimated_tokens)
        .sum::<usize>();
    assert!(pinned_minimum > 0);

    let exact = build(pinned_minimum).unwrap();
    for candidate_id in pinned_ids {
        assert!(
            exact
                .plan
                .sections
                .iter()
                .any(|section| section.id == candidate_id),
            "missing pinned candidate {candidate_id}"
        );
        let decision = exact
            .plan
            .evidence
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == candidate_id)
            .expect("pinned decision should be present");
        assert_ne!(decision.outcome, ContextPlanOutcome::Omitted);
        assert!(decision.allocated_estimated_tokens >= decision.minimum_estimated_tokens);
    }
    assert!(exact.plan.evidence.allocated_estimated_tokens <= pinned_minimum);

    let error = build(pinned_minimum - 1).unwrap_err();
    assert!(error.to_string().contains("pinned_minimum_over_budget"));
    assert!(error
        .to_string()
        .contains(&format!("pinned_minimum={pinned_minimum}")));
}

#[test]
fn build_context_bounds_recent_turn_reprojection_to_planned_compact_budget() {
    let mut fixture = Fixture::default();
    let mut historical_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "large historical context ".repeat(4_000),
        },
    );
    historical_message.turn_id = Some("turn-large-history".to_string());
    historical_message.message_seq = Some(1);
    let mut historical_turn = holon_domain::TurnRecord::new("default", "turn-large-history", 1);
    historical_turn.input_message_ids = vec![historical_message.id.clone()];
    historical_turn.trigger = Some(holon_domain::TurnTriggerSummary::from_message(
        &historical_message,
    ));
    fixture.messages.push(historical_message);
    fixture.turn_records.push(historical_turn);

    let current_message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue with the current task.".to_string(),
        },
    );
    let agent = AgentState::new("default");
    let config_for_budget = |budget| ContextConfig {
        prompt_budget_estimated_tokens: budget,
        turn_projection_budget_ratio: 1.0,
        turn_projection_min_budget: 0,
        turn_projection_max_budget: budget,
        ..ContextConfig::default()
    };
    let initial = fixture
        .build_context(
            &agent,
            &execution_snapshot_for(&agent),
            &holon_domain::SkillsRuntimeView::default(),
            &current_message,
            None,
            &config_for_budget(20_000),
        )
        .unwrap();
    let requested = |candidate_id: &str| {
        initial
            .plan
            .evidence
            .decisions
            .iter()
            .find(|decision| decision.candidate_id == candidate_id)
            .expect("candidate decision")
            .requested_estimated_tokens
    };
    let recent_turns_minimum = initial
        .plan
        .evidence
        .decisions
        .iter()
        .find(|decision| decision.candidate_id == "recent_turns")
        .expect("recent_turns decision")
        .minimum_estimated_tokens;
    let compact_plan_budget = [
        "current_input",
        "current_work_item",
        "agent",
        "execution_environment",
        "context_contract",
    ]
    .into_iter()
    .map(requested)
    .sum::<usize>()
        + recent_turns_minimum;

    let (compact_plan_budget, compact) = (compact_plan_budget..compact_plan_budget + 512)
        .find_map(|budget| {
            let built = fixture
                .build_context(
                    &agent,
                    &execution_snapshot_for(&agent),
                    &holon_domain::SkillsRuntimeView::default(),
                    &current_message,
                    None,
                    &config_for_budget(budget),
                )
                .unwrap();
            built
                .plan
                .evidence
                .decisions
                .iter()
                .any(|decision| {
                    decision.candidate_id == "recent_turns"
                        && decision.outcome == ContextPlanOutcome::Compact
                })
                .then_some((budget, built))
        })
        .expect("a nearby budget should select compact recent_turns");
    let decision = compact
        .plan
        .evidence
        .decisions
        .iter()
        .find(|decision| decision.candidate_id == "recent_turns")
        .expect("recent_turns decision");
    assert_eq!(decision.outcome, ContextPlanOutcome::Compact);
    assert_eq!(
        compact
            .recent_turns_budget
            .expect("selected recent_turns remains reprojectable"),
        compact_plan_budget.min(128)
    );
}

#[test]
fn live_input_recovery_renders_each_persisted_batch_and_body_once() {
    use holon_domain::IdleInputBatchItem;

    let mut first = LiveInputBatchRecord {
        activation_id: "recovered-activation".into(),
        safe_point: holon_domain::LiveInputSafePoint::new("original-activation", 2),
        cutoff: chrono::Utc::now(),
        max_items: 8,
        max_bytes: 24 * 1024,
        rendered_bytes: 0,
        result_identities: Vec::new(),
        ordinary_message_ids: Vec::new(),
        items: vec![IdleInputBatchItem {
            message_id: "reply-original".into(),
            source_ref: "message:reply-original".into(),
            rendered: "persisted original reply body\n".into(),
        }],
    };
    first.rendered_bytes = first.rendered_text().len();
    let mut second = first.clone();
    second.safe_point = holon_domain::LiveInputSafePoint::new("recovered-activation", 2);
    second.items.push(IdleInputBatchItem {
        message_id: "later-result".into(),
        source_ref: "message:later-result".into(),
        rendered: "persisted later result body\n".into(),
    });
    second.rendered_bytes = second.rendered_text().len();
    let rendered =
        render_live_input_batches(&[first.clone(), first, second], &mut BTreeSet::new()).unwrap();
    assert_eq!(rendered.matches("Current live input batch").count(), 2);
    assert_eq!(rendered.matches("persisted original reply body").count(), 1);
    assert_eq!(rendered.matches("persisted later result body").count(), 1);
    assert!(rendered.find("original reply").unwrap() < rendered.find("later result").unwrap());
}

#[test]
fn recovery_input_projection_keeps_full_source_evidence_and_shared_message_deduplication() {
    use holon_domain::{IdleInputBatchItem, IdleInputBatchRecord};

    let item = |id: &str, body: &str| IdleInputBatchItem {
        message_id: id.into(),
        source_ref: format!("message:{id}"),
        rendered: format!("source_ref: message:{id}\n{body}\n"),
    };
    let full_body = format!("live-body-start{}live-body-end", "evidence".repeat(400));
    let mut source = TurnRecord::new("default", "validated-source-turn", 1);
    source.idle_input_batch = Some(IdleInputBatchRecord {
        anchor_message_id: "root".into(),
        activation_id: Some("source-activation".into()),
        sender_agent_id: "sender".into(),
        cutoff: chrono::Utc::now(),
        max_items: 8,
        max_bytes: 24 * 1024,
        rendered_bytes: 0,
        items: vec![
            item("root", "root body already anchored"),
            item("idle-follower", "idle follower full body"),
            item("current-idle", "current idle body already projected"),
        ],
    });
    let live = LiveInputBatchRecord {
        activation_id: "source-activation".into(),
        safe_point: holon_domain::LiveInputSafePoint::new("source-activation", 2),
        cutoff: chrono::Utc::now(),
        max_items: 8,
        max_bytes: 24 * 1024,
        rendered_bytes: 0,
        result_identities: Vec::new(),
        ordinary_message_ids: vec!["live-follower".into()],
        items: vec![
            item("root", "root body already anchored"),
            item("idle-follower", "idle follower full body"),
            item("live-follower", &full_body),
        ],
    };
    source.live_input_batches = vec![live.clone()];
    source.input_message_ids = vec![
        "root".into(),
        "idle-follower".into(),
        "current-idle".into(),
        "live-follower".into(),
    ];
    let mut seen = BTreeSet::from(["root".into(), "current-input".into(), "current-idle".into()]);
    let rendered = render_recovery_input_batches(&|_| CapturedRecords::empty(), &source, &mut seen)
        .unwrap()
        .unwrap();
    assert!(rendered.contains("source turn validated-source-turn (original provenance)"));
    assert!(!rendered.contains("already anchored"));
    assert!(!rendered.contains("already projected"));
    assert_eq!(rendered.matches("idle follower full body").count(), 1);
    assert_eq!(rendered.matches(full_body.as_str()).count(), 1);
    assert!(rendered.contains("source_ref: message:live-follower"));
    assert!(render_live_input_batches(&[live], &mut seen).is_none());

    for id in ["recovery_input_batches", "recovered_operator_inputs"] {
        let section = turn_section(id, rendered.clone());
        let candidate = context_candidate(section.clone(), Some(section), true);
        assert!(candidate.policy.pinned);
        assert_eq!(
            candidate.full.content,
            candidate.compact.as_ref().unwrap().content
        );
        let budget = estimate_section_tokens(&candidate.full);
        let plan = plan_context(vec![candidate.clone()], budget).unwrap();
        assert_eq!(plan.sections[0].content, rendered);
        assert!(plan_context(vec![candidate], budget.saturating_sub(1)).is_err());
    }
}
