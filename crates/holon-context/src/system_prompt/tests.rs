use super::*;
use crate::fingerprint::{prompt_cache_key, prompt_cache_scope_fingerprint};
use holon_domain::{
    AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState,
    AgentVisibility, AuthorityClass, MessageKind, Priority,
};
use holon_system::ExecutionSnapshot;
use std::path::PathBuf;

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

fn sample_child_identity() -> AgentIdentityView {
    AgentIdentityView {
        agent_id: "child_test".into(),
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
        delegated_from_task_id: Some("task-1".into()),
    }
}

fn sample_execution_snapshot() -> ExecutionSnapshot {
    ExecutionSnapshot {
        profile: holon_system::ExecutionProfile::default(),
        policy: holon_system::ExecutionProfile::default().policy_snapshot(),
        attached_workspaces: vec![],
        workspace_id: Some("workspace-1".into()),
        workspace_anchor: PathBuf::from("/repo"),
        execution_root: PathBuf::from("/repo"),
        cwd: PathBuf::from("/repo/src"),
        execution_root_id: Some("canonical_root:workspace-1".into()),
        projection_kind: Some(holon_system::types::WorkspaceProjectionKind::CanonicalRoot),
        access_mode: Some(holon_system::types::WorkspaceAccessMode::SharedRead),
        worktree_root: None,
        execution_roots: Vec::new(),
    }
}

#[test]
fn system_prompt_consolidates_shared_core_into_seven_l3a_sections() {
    use crate::shared_prefix_len;

    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("/repo"),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let boundary = shared_prefix_len(&sections);
    let shared_names = sections[..boundary]
        .iter()
        .map(|section| section.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        shared_names,
        vec![
            "identity",
            "authority_and_trust",
            "work_and_verification",
            "agent_home_boundary",
            "durable_work_tracking",
            "runtime_scheduling_contract",
            "delivery_contract",
        ]
    );
    for retired in [
        "core_contract",
        "tool_use_contract",
        "engineering_guardrails",
        "instruction_precedence",
        "agent_home_contract",
        "context_completion",
        "reporting_contract",
        "file_reference_contract",
        "exploration_discipline",
        "planning_discipline",
        "work_item_first_execution",
        "trust_boundary",
        "verification",
        "execution_environment_contract",
        "holon_app_mechanism",
    ] {
        assert!(
            !sections.iter().any(|section| section.name == retired),
            "retired split section `{retired}` must not be emitted"
        );
    }
}

#[test]
fn shared_contract_bytes_survive_independent_agent_workspace_task_and_source_inputs() {
    use crate::{
        fingerprint::{context, scope, shared_prefix_fingerprint},
        render::render_current_input_section,
        render_sections, shared_prefix_len, PromptLayout,
    };
    use holon_domain::AgentsMdScope;

    let tools = vec![
        ToolSpec {
            name: "WaitFor".into(),
            description: "Wait for a wake".into(),
            input_schema: serde_json::json!({"type": "object"}),
            freeform_grammar: None,
        },
        ToolSpec {
            name: "ExecCommand".into(),
            description: "Run a shell command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            freeform_grammar: None,
        },
    ];
    let mut results = Vec::new();
    for mut message in [sample_message(), sample_message()] {
        let unique = message.id.clone();
        let root = PathBuf::from(format!("/isolated/{unique}/workspace"));
        let mut identity = sample_child_identity();
        identity.agent_id = format!("agent-{unique}");
        identity.lineage_parent_agent_id = Some(format!("parent-{unique}"));
        identity.delegated_from_task_id = Some(format!("task-{unique}"));
        message.agent_id = identity.agent_id.clone();
        message.work_item_id = Some(format!("work-{unique}"));
        let input = format!("Edit only file: src/{unique}.rs; task-{unique}");
        message.body = MessageBody::Text {
            text: input.clone(),
        };
        let source = |scope, name: &str| AgentsMdSource {
            scope,
            kind: AgentsMdKind::AgentsMd,
            path: PathBuf::from(format!("/sources/{unique}/{name}/AGENTS.md")),
            content: format!("{name}-guidance-{unique}"),
        };
        let loaded = LoadedAgentsMd {
            user_global_source: Some(source(AgentsMdScope::UserGlobal, "global")),
            agent_source: Some(source(AgentsMdScope::Agent, "agent")),
            workspace_source: Some(source(AgentsMdScope::Workspace, "workspace")),
            ..LoadedAgentsMd::default()
        };
        let memory = |name: &str| {
            let content = format!("{name}-memory-{unique}");
            AgentMemorySource {
                path: PathBuf::from(format!("/memories/{unique}/{name}.md")),
                total_chars: content.chars().count(),
                content,
                truncated: false,
            }
        };
        let memories = LoadedAgentMemory {
            operator_source: Some(memory("operator")),
            self_source: Some(memory("self")),
            ..LoadedAgentMemory::default()
        };
        let skills = SkillsRuntimeView {
            discoverable_skills: vec![holon_domain::SkillCatalogEntry {
                skill_id: format!("workspace:{unique}"),
                root_id: format!("root-{unique}"),
                skill_dir: "review".into(),
                name: "review".into(),
                description: format!("Review {unique}"),
                path: root.join("skills/review/SKILL.md"),
                scope: holon_domain::SkillScope::Workspace,
            }]
            .into(),
            ..SkillsRuntimeView::default()
        };
        let sections = build_system_sections(
            &identity,
            &message,
            &root,
            &loaded,
            &memories,
            &skills,
            &tools,
            ToolPromptContext::default(),
        );
        let boundary = shared_prefix_len(&sections);
        let prefix = render_sections(&sections[..boundary]);
        let suffix = render_sections(&sections[boundary..]);
        assert_eq!(sections[boundary].name, "workspace_context");
        assert!(sections[boundary..]
            .iter()
            .all(|section| section.layout == PromptLayout::ScopedSuffix));
        assert_eq!(sections.last().unwrap().name, "response_language");
        for name in [
            "identity",
            "authority_and_trust",
            "work_and_verification",
            "agent_home_boundary",
            "durable_work_tracking",
            "runtime_scheduling_contract",
            "delivery_contract",
            "tool_wait_for",
            "tool_exec_command",
            "holon_cli_contract",
        ] {
            assert!(sections[..boundary]
                .iter()
                .any(|section| section.name == name));
        }
        assert!(prefix.contains("No in-turn blocking output wait is provided"));
        assert!(prefix.contains("does not grant extra authority"));
        assert!(!prefix.contains(&unique));
        assert!(suffix.contains(root.to_str().unwrap()));
        assert!(suffix.contains(&identity.agent_id));
        assert!(suffix.contains("Creation provenance and lineage"));
        assert!(suffix.contains("This is a constrained repair request"));
        for source in [
            loaded.user_global_source.as_ref().unwrap(),
            loaded.agent_source.as_ref().unwrap(),
            loaded.workspace_source.as_ref().unwrap(),
        ] {
            assert_eq!(suffix.matches(source.path.to_str().unwrap()).count(), 1);
            assert_eq!(suffix.matches(source.content.as_str()).count(), 1);
        }
        for source in [
            memories.operator_source.as_ref().unwrap(),
            memories.self_source.as_ref().unwrap(),
        ] {
            assert_eq!(suffix.matches(source.path.to_str().unwrap()).count(), 1);
            assert_eq!(suffix.matches(source.content.as_str()).count(), 1);
        }
        let contexts = vec![
            render_current_input_section(&message, 4096, None),
            section(
                "history",
                PromptStability::AgentScoped,
                format!("history-{unique}"),
            ),
            section(
                "skills_catalog",
                PromptStability::AgentScoped,
                skills.discoverable_skills[0].path.display().to_string(),
            ),
        ];
        let attachment = render_sections(&contexts);
        assert!(attachment.contains(&input));
        assert!(attachment.contains(&format!("work-{unique}")));
        assert!(attachment.contains(&format!("history-{unique}")));
        assert!(attachment.contains(skills.discoverable_skills[0].path.to_str().unwrap()));
        let mut execution = sample_execution_snapshot();
        execution.workspace_anchor = root.clone();
        execution.execution_root = root.clone();
        execution.cwd = root;
        results.push((
            prefix,
            render_sections(&sections),
            shared_prefix_fingerprint(&sections),
            scope(&identity.agent_id, &execution, &sections, &contexts, &tools),
            context(
                &identity.agent_id,
                0,
                &execution,
                &sections,
                &contexts,
                &tools,
            ),
        ));
    }
    assert_eq!(results[0].0, results[1].0);
    assert_ne!(results[0].1, results[1].1);
    assert_eq!(results[0].2, results[1].2);
    assert_ne!(results[0].3, results[1].3);
    assert_ne!(results[0].4, results[1].4);
    assert_ne!(
        prompt_cache_key("agent-one", &results[0].3),
        prompt_cache_key("agent-two", &results[1].3)
    );
}

#[test]
fn prefix_observation_tracks_contracts_and_tool_surfaces_but_not_scoped_content() {
    use crate::{fingerprint::shared_prefix_fingerprint, render_sections, shared_prefix_len};
    use holon_domain::ApplyPatchSurface;

    let build = |loaded: &LoadedAgentsMd, tools: &[ToolSpec], surface| {
        build_system_sections(
            &sample_identity(),
            &sample_message(),
            Path::new("/repo"),
            loaded,
            &LoadedAgentMemory::default(),
            &SkillsRuntimeView::default(),
            tools,
            ToolPromptContext {
                apply_patch_surface: surface,
            },
        )
    };
    let empty = build(&LoadedAgentsMd::default(), &[], None);
    let with_empty_guidance = build(
        &LoadedAgentsMd {
            agent_source: Some(AgentsMdSource {
                scope: holon_domain::AgentsMdScope::Agent,
                kind: AgentsMdKind::AgentsMd,
                path: PathBuf::from("/different-agent/AGENTS.md"),
                content: String::new(),
            }),
            ..LoadedAgentsMd::default()
        },
        &[],
        None,
    );
    assert_eq!(
        shared_prefix_fingerprint(&empty),
        shared_prefix_fingerprint(&with_empty_guidance)
    );
    assert!(tools::tool_sections_with_context(&[], ToolPromptContext::default()).is_empty());
    assert!(empty
        .iter()
        .all(|section| !section.name.starts_with("tool_")));
    assert!(empty
        .iter()
        .take(shared_prefix_len(&empty))
        .any(|section| section.name == "authority_and_trust"));

    let tools = [ToolSpec {
        name: "ApplyPatch".into(),
        description: "Apply a patch".into(),
        input_schema: serde_json::json!({"type": "object"}),
        freeform_grammar: None,
    }];
    let json = build(
        &LoadedAgentsMd::default(),
        &tools,
        Some(ApplyPatchSurface::UnifiedDiffJson),
    );
    let dsl = build(
        &LoadedAgentsMd::default(),
        &tools,
        Some(ApplyPatchSurface::CodexDslFreeform),
    );
    let json_prefix = render_sections(&json[..shared_prefix_len(&json)]);
    let dsl_prefix = render_sections(&dsl[..shared_prefix_len(&dsl)]);
    assert!(json_prefix.contains("Current ApplyPatch surface is a JSON/function tool"));
    assert!(dsl_prefix.contains("Current ApplyPatch surface is Codex DSL freeform"));
    assert_ne!(
        shared_prefix_fingerprint(&empty),
        shared_prefix_fingerprint(&json)
    );
    assert_ne!(
        shared_prefix_fingerprint(&json),
        shared_prefix_fingerprint(&dsl)
    );
    let mut scoped_change = json.clone();
    let boundary = shared_prefix_len(&scoped_change);
    for section in &mut scoped_change[boundary..] {
        section.content.push_str("\nscoped-only change");
    }
    assert_eq!(
        shared_prefix_fingerprint(&json),
        shared_prefix_fingerprint(&scoped_change)
    );
    for name in ["work_and_verification", "tool_apply_patch"] {
        let mut changed = json.clone();
        changed
            .iter_mut()
            .find(|section| section.name == name)
            .unwrap()
            .content
            .push_str("\nchanged fixed contract");
        assert_ne!(
            shared_prefix_fingerprint(&json),
            shared_prefix_fingerprint(&changed)
        );
    }
}

#[test]
fn prompt_cache_key_changes_when_user_global_agents_md_changes() {
    let session = AgentState::new("default");
    let execution = sample_execution_snapshot();
    let mut first_loaded = LoadedAgentsMd::default();
    first_loaded.user_global_source = Some(AgentsMdSource {
        scope: holon_domain::AgentsMdScope::UserGlobal,
        kind: AgentsMdKind::AgentsMd,
        path: PathBuf::from("/tmp/user/.agents/AGENTS.md"),
        content: "first user-global guidance".into(),
    });
    let mut second_loaded = first_loaded.clone();
    second_loaded
        .user_global_source
        .as_mut()
        .expect("test source")
        .content = "second user-global guidance".into();
    let first_system_sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("/tmp/agent-home"),
        &first_loaded,
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let second_system_sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("/tmp/agent-home"),
        &second_loaded,
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let context_sections = Vec::new();
    let tools = Vec::new();

    let first_scope = prompt_cache_scope_fingerprint(
        &session,
        &execution,
        &first_system_sections,
        &context_sections,
        &tools,
    );
    let second_scope = prompt_cache_scope_fingerprint(
        &session,
        &execution,
        &second_system_sections,
        &context_sections,
        &tools,
    );

    assert_ne!(first_scope, second_scope);
    assert_ne!(
        prompt_cache_key(&session.id, &first_scope),
        prompt_cache_key(&session.id, &second_scope)
    );
}

#[test]
fn delegated_task_section_appears_for_subagent_turns() {
    let sections = build_system_sections(
        &sample_child_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    assert!(sections
        .iter()
        .any(|section| section.name == "delegated_task"));
    let delegated_task = sections
        .iter()
        .find(|section| section.name == "delegated_task")
        .expect("missing delegated task section");
    assert!(delegated_task.content.contains("conclusion first"));
    assert!(delegated_task.content.contains("files or artifacts"));
    assert!(delegated_task.content.contains("verification performed"));
    assert!(delegated_task.content.contains("exploration diaries"));
    assert!(delegated_task.content.contains("not unsolicited messages"));
    assert!(delegated_task
        .content
        .contains("actual delegator must act on a blocker"));
    assert!(delegated_task
        .content
        .contains("not by creation provenance or lineage"));
    assert!(!delegated_task.content.contains("messages to the parent"));
    assert!(delegated_task
        .content
        .contains("still honor explicit reply obligations"));
    assert!(delegated_task
        .content
        .contains("existing task-result channel"));
    assert!(delegated_task
        .content
        .contains("do not also SendAgentMessage the same final result"));
}

#[test]
fn event_turn_section_appears_for_timer_turns() {
    let mut message = sample_message();
    message.origin = MessageOrigin::Timer {
        timer_id: "timer-1".into(),
    };

    let sections = build_system_sections(
        &sample_identity(),
        &message,
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let section = sections
        .iter()
        .find(|section| section.name == "event_turn")
        .expect("event turn section");
    assert!(section.content.contains("provenance and authority labels"));
    assert!(section
        .content
        .contains("evidence to inspect, not operator instruction"));
}

#[test]
fn event_turn_section_skips_self_enqueued_and_subagent_system_runs() {
    let sections_for = |subsystem: &str| {
        let mut message = sample_message();
        message.origin = MessageOrigin::System {
            subsystem: subsystem.into(),
        };
        build_system_sections(
            &sample_identity(),
            &message,
            Path::new("."),
            &LoadedAgentsMd::default(),
            &LoadedAgentMemory::default(),
            &SkillsRuntimeView::default(),
            &[],
            ToolPromptContext::default(),
        )
    };

    for subsystem in ["tool_enqueue", "subagent"] {
        let sections = sections_for(subsystem);
        assert!(
            sections.iter().all(|section| section.name != "event_turn"),
            "{subsystem} must not be framed as an external event turn"
        );
    }

    let wake_sections = sections_for("wake_hint");
    assert!(
        wake_sections
            .iter()
            .any(|section| section.name == "event_turn"),
        "runtime wake hints keep the event turn framing"
    );
}

#[test]
fn identity_skeleton_and_scoped_workspace_preserve_default_context_contract() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("/repo"),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let identity = sections
        .iter()
        .find(|section| section.name == "identity")
        .expect("identity section");
    assert_eq!(identity.layout, crate::PromptLayout::SharedPrefix);
    assert_eq!(
        identity.content,
        "You are a Holon agent: a long-lived, general-purpose assistant. Understand the operator's goal, use the available tools and context to carry work through, and report clear results."
    );
    let identity_lower = identity.content.to_ascii_lowercase();
    for implementation_attribute in ["headless", "event-driven", "coding-oriented"] {
        assert!(
            !identity_lower.contains(implementation_attribute),
            "identity must not prescribe the {implementation_attribute} implementation attribute"
        );
    }
    let section = sections
        .iter()
        .find(|section| section.name == "workspace_context")
        .expect("workspace context section");
    assert_eq!(section.layout, crate::PromptLayout::ScopedSuffix);
    assert_eq!(section.stability, PromptStability::AgentScoped);
    assert!(section.content.contains("/repo"));

    assert!(section
        .content
        .contains("default long-lived project context"));
    assert!(section.content.contains("default cwd"));
    assert!(section.content.contains("relative ApplyPatch targets"));
    assert!(section.content.contains("scoped AGENTS.md guidance"));
    assert!(section
        .content
        .contains("explicit absolute paths from the operator or task context"));
    assert!(section
        .content
        .contains("Use AttachWorkspace to bind a new project"));
    assert!(section
        .content
        .contains("SwitchWorkspace to activate an existing workspace"));
    assert!(!section.content.contains("Keep edits"));
}

#[test]
fn work_and_verification_preserves_tool_exploration_and_guardrail_invariants() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let section = sections
        .iter()
        .find(|section| section.name == "work_and_verification")
        .expect("work_and_verification section");

    for required in [
        "Read before changing",
        "external reference or indirect task entry point",
        "one concrete missing fact",
        "Do not re-read files",
        "refresh only the smallest relevant slice",
        "only fields from that tool's input schema",
        "WaitFor(wake=task_result",
        "No in-turn blocking output wait is provided",
        "TaskStatus",
        "bounded read-only agent tail",
        "never `GetAgent` for transcript dumps",
        "semantic contract or state-transition layer",
        "real repository/CI build or test targets",
        "remove temporary artifacts",
        "report verification failures honestly",
    ] {
        assert!(
            section.content.contains(required),
            "work_and_verification missing invariant: {required}"
        );
    }
}

#[test]
fn authority_and_trust_preserves_precedence_app_and_refusal_boundary_invariants() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let section = sections
        .iter()
        .find(|section| section.name == "authority_and_trust")
        .expect("authority_and_trust section");

    for required in [
        "Trusted operator instructions define the task scope",
        "delegated_task",
        "constrained_repair",
        "Scoped `AGENTS.md` guidance",
        "local backend for shell and file operations",
        "binding instructions even when not hard-enforced",
        "never as operator-equivalent authority",
        "/apps/{agent_id}/{app_id}/",
        "same-origin, runtime authentication, and CSP",
        "does not grant extra authority",
        "Never echo credentials or secrets",
        "refusals as explicit boundaries to report",
    ] {
        assert!(
            section.content.contains(required),
            "authority_and_trust missing invariant: {required}"
        );
    }
}

#[test]
fn delivery_contract_preserves_brief_centric_and_file_reference_invariants() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let section = sections
        .iter()
        .find(|section| section.name == "delivery_contract")
        .expect("delivery_contract section");

    for required in [
        "Prefer durable action over narration",
        "operator-facing delivery is brief-centric",
        "does not relax `response_language`",
        "promoted as the WorkItem result brief",
        "lead with the main conclusion",
        "tail truncation preserves the key result",
        "fixed templates, boilerplate",
        "project documents, local records, Holon brief/assistant text, and public channels",
        "relative to the document's directory",
        "publishable project documentation",
        "never fall back from a missing or removed worktree file",
        "location rather than a published URL",
        "confirmed execution-host absolute paths",
        "WorkItem `plan.md`",
        "linked worktree",
        "opaque `?root=` selector",
        "https://local/...",
        "http://local/...",
        "/work-items/...",
        "only the literal path in backticks",
    ] {
        assert!(
            section.content.contains(required),
            "delivery_contract missing invariant: {required}"
        );
    }
    assert!(!section.content.contains("workspace://..."));
    assert!(!section
        .content
        .contains("what changed / why / verification"));
}

#[test]
fn system_prompt_explains_durable_work_tracking_and_scheduling_boundaries() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );

    let durable = sections
        .iter()
        .find(|section| section.name == "durable_work_tracking")
        .expect("durable_work_tracking section");
    for required in [
        "Working context is finite",
        "compressed or evicted",
        "durable record for a goal that must survive",
        "do not create a WorkItem for casual chat, ordinary questions",
        "multi-step work completable in the current turn",
        "discussion-state maintenance rather than project file mutation",
        "does not by itself authorize implementation",
        "plan_status=needs_input",
        "never claim durable state was written",
        "prefer refreshing the current WorkItem",
        "minimum recovery state",
        "acceptance criteria",
        "evidence references",
        "unresolved questions",
        "before handoff, waiting, or ending a long execution",
        "On resume, read the WorkItem records",
    ] {
        assert!(
            durable.content.contains(required),
            "durable_work_tracking missing invariant: {required}"
        );
    }

    let scheduling = sections
        .iter()
        .find(|section| section.name == "runtime_scheduling_contract")
        .expect("runtime scheduling contract section");
    assert!(scheduling
        .content
        .contains("focus, not execution ownership or lifecycle"));
    assert!(scheduling.content.contains("does not migrate that task"));
    assert!(scheduling
        .content
        .contains("from the execution-bound WorkItem first"));
    assert!(scheduling
        .content
        .contains("any WorkItem of that agent may hold the wait without migrating"));
    assert!(!scheduling
        .content
        .contains("a task-result wait must still match"));
    assert!(scheduling
        .content
        .contains("edits to a plan artifact's body do not change scheduling state"));
    assert!(scheduling
        .content
        .contains("UpdateWorkItem.plan_status does affect WorkItem readiness"));
    assert!(scheduling
        .content
        .contains("before any control operation that may end the turn"));
}

#[test]
fn system_prompt_includes_response_language_contract() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let section = sections
        .iter()
        .find(|section| section.name == "response_language")
        .expect("response language section");

    assert!(section
        .content
        .contains("all operator-visible assistant prose"));
    assert!(section.content.contains("task rejoin events"));
    assert!(section.content.contains("verbose assistant-round text"));
    assert!(section.content.contains("progress before tool calls"));
    assert!(section
        .content
        .contains("assistant-authored prose that tools publish"));
    assert!(section.content.contains("pull-request reviews"));
    assert!(section.content.contains("machine-readable fields"));
    assert!(section
        .content
        .contains("current trusted operator's explicit language request"));
    assert!(section
        .content
        .contains("loaded `agent_home/memory/operator.md`"));
    assert!(section
        .content
        .contains("until a trusted operator instruction explicitly requests another language"));
    assert!(section.content.contains("not signals to switch languages"));
    assert!(section.content.contains("code identifiers"));
    assert!(section.content.contains("direct quotations"));
    assert!(section
        .content
        .contains("target repository's existing language"));
}

#[test]
fn response_language_section_follows_loaded_guidance_and_tools() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd {
            user_global_source: Some(AgentsMdSource {
                scope: holon_domain::AgentsMdScope::UserGlobal,
                kind: AgentsMdKind::AgentsMd,
                path: PathBuf::from("/tmp/user/.agents/AGENTS.md"),
                content: "User prefers Chinese communication.".into(),
            }),
            agent_source: Some(AgentsMdSource {
                scope: holon_domain::AgentsMdScope::Agent,
                kind: AgentsMdKind::AgentsMd,
                path: PathBuf::from("/tmp/agent-home/AGENTS.md"),
                content: "Respond in Chinese.".into(),
            }),
            workspace_source: Some(AgentsMdSource {
                scope: holon_domain::AgentsMdScope::Workspace,
                kind: AgentsMdKind::AgentsMd,
                path: PathBuf::from("/repo/AGENTS.md"),
                content: "Workspace guidance".into(),
            }),
            ..LoadedAgentsMd::default()
        },
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let names = sections
        .iter()
        .map(|section| section.name.as_str())
        .collect::<Vec<_>>();
    let response_language_idx = names
        .iter()
        .position(|name| *name == "response_language")
        .expect("response language section");
    let user_global_idx = names
        .iter()
        .position(|name| *name == "user_global_agents_md")
        .expect("user global AGENTS.md section");

    assert!(response_language_idx > user_global_idx);
    assert_eq!(response_language_idx, names.len() - 1);
}

#[test]
fn system_prompt_includes_runtime_scheduling_contract() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let section = sections
        .iter()
        .find(|section| section.name == "runtime_scheduling_contract")
        .expect("runtime scheduling contract section");

    assert!(section.content.contains("Holon is event-driven"));
    assert!(section
        .content
        .contains("resumes work from persisted runtime state"));
    assert!(section
        .content
        .contains("Current WorkItem means focus, not execution ownership"));
    assert!(section
        .content
        .contains("a task keeps the WorkItem owner captured when created"));
    assert!(section
        .content
        .contains("WaitFor resolves ownership from the execution-bound WorkItem first"));
    assert!(section
        .content
        .contains("then an explicit owned open work_item_id"));
    assert!(!section
        .content
        .contains("WaitFor resolves ownership from an explicit owned open work_item_id"));
    assert!(section
        .content
        .contains("edits to a plan artifact's body do not change scheduling state"));
    assert!(section
        .content
        .contains("UpdateWorkItem.plan_status does affect WorkItem readiness"));
    assert!(section.content.contains("Queued runnable WorkItems"));
    assert!(section.content.contains("yielded or parked WorkItems"));
    assert!(section.content.contains("continuation frame"));
    assert!(section.content.contains("do not mark them blocked"));
    assert!(section
        .content
        .contains("PickWorkItem from runnable A to B records an A -> B continuation"));
    assert!(section
        .content
        .contains("completing B may restore A and end the turn"));
    assert!(section
        .content
        .contains("Completing the execution-bound WorkItem ends the turn"));
    assert!(section
        .content
        .contains("detached completion requires an idle agent execution lane"));
    assert!(section
        .content
        .contains("Persist required recovery state and arrange any successor"));
    assert!(section.content.contains("External triggers"));
    assert!(section
        .content
        .contains("WaitFor(task_result) instead of polling"));
}

#[test]
fn system_prompt_includes_agent_contract_section() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let section = sections
        .iter()
        .find(|section| section.name == "agent_contract")
        .expect("agent contract section");

    assert!(section.content.contains("stable identity `default`"));
    assert!(section
        .content
        .contains("independently addressable agent identity"));
    assert!(section.content.contains("canonical capability policy"));
    assert!(section
        .content
        .contains("canonical lifecycle attachment and active supervision"));
    assert!(!section.content.contains("public_named"));
    assert!(!section.content.contains("private_child"));
}

#[test]
fn creation_lineage_does_not_imply_delegation_or_reporting() {
    let mut named_identity = sample_identity();
    named_identity.agent_id = "long_lived".into();
    named_identity.kind = AgentKind::Named;
    named_identity.is_default_agent = false;
    named_identity.parent_agent_id = Some("creator".into());
    named_identity.lineage_parent_agent_id = Some("creator".into());

    for identity in [sample_identity(), named_identity, sample_child_identity()] {
        let sections = build_system_sections(
            &identity,
            &sample_message(),
            Path::new("."),
            &LoadedAgentsMd::default(),
            &LoadedAgentMemory::default(),
            &SkillsRuntimeView::default(),
            &[],
            ToolPromptContext::default(),
        );
        let contract = &sections
            .iter()
            .find(|section| section.name == "agent_contract")
            .expect("agent contract without invocation tools")
            .content;
        for rule in [
            "Creation provenance and lineage do not create reporting obligations",
            "Reply to the operator for operator work",
            "to the actual delegator for delegated work",
            "using the current task or message routing context",
            "never infer a reporting recipient from an identity or lineage parent",
            "Tool availability is determined by canonical capability policy",
            "Lifecycle cleanup authority is determined by canonical lifecycle attachment",
        ] {
            assert!(
                contract.contains(rule),
                "{} missing rule: {rule}",
                identity.agent_id
            );
        }
        assert_eq!(
            sections
                .iter()
                .any(|section| section.name == "delegated_task"),
            identity.delegated_from_task_id.is_some(),
            "{} delegation must not follow creation lineage alone",
            identity.agent_id,
        );
    }
}

#[test]
fn system_prompt_includes_agent_home_boundary_section() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );
    let section = sections
        .iter()
        .find(|section| section.name == "agent_home_boundary")
        .expect("agent home boundary section");

    assert!(section
        .content
        .contains("evolving long-lived contract for this specific agent"));
    assert!(section
        .content
        .contains("role, standing responsibilities, granted authority"));
    assert!(section
        .content
        .contains("work-items/<work_item_id>/plan.md"));
    assert!(section
        .content
        .contains("agent-authored durable plan artifact"));
    assert!(section
        .content
        .contains("agent_home/tmp/` is for short-lived working files"));
    assert!(section.content.contains("near turn end"));
    assert!(section.content.contains("transient plans/notes"));
    assert!(section.content.contains("`.holon/`"));
}

#[test]
fn system_sections_share_tools_before_guidance_without_changing_guidance_order() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd {
            user_global_source: Some(AgentsMdSource {
                scope: holon_domain::AgentsMdScope::UserGlobal,
                kind: AgentsMdKind::AgentsMd,
                path: PathBuf::from("/tmp/user/.agents/AGENTS.md"),
                content: "user-global guidance".into(),
            }),
            agent_source: Some(AgentsMdSource {
                scope: holon_domain::AgentsMdScope::Agent,
                kind: AgentsMdKind::AgentsMd,
                path: PathBuf::from("/tmp/agent-home/AGENTS.md"),
                content: "agent guidance".into(),
            }),
            workspace_source: Some(AgentsMdSource {
                scope: holon_domain::AgentsMdScope::Workspace,
                kind: AgentsMdKind::AgentsMd,
                path: PathBuf::from("/repo/AGENTS.md"),
                content: "workspace guidance".into(),
            }),
            ..LoadedAgentsMd::default()
        },
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView {
            discoverable_skills: vec![holon_domain::SkillCatalogEntry {
                skill_id: "workspace:review".into(),
                root_id: "workspace:test-root".into(),
                skill_dir: "review".into(),
                name: "review".into(),
                description: "Review workflow".into(),
                path: PathBuf::from("/repo/.agents/skills/review/SKILL.md"),
                scope: holon_domain::SkillScope::Workspace,
            }]
            .into(),
            ..SkillsRuntimeView::default()
        },
        &[ToolSpec {
            name: "ExecCommand".into(),
            description: "Run a shell command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            freeform_grammar: None,
        }],
        ToolPromptContext::default(),
    );

    let names = sections
        .iter()
        .map(|section| section.name.as_str())
        .collect::<Vec<_>>();
    let global_idx = names
        .iter()
        .position(|name| *name == "user_global_agents_md")
        .unwrap();
    let agent_idx = names
        .iter()
        .position(|name| *name == "agent_agents_md")
        .unwrap();
    let workspace_idx = names
        .iter()
        .position(|name| *name == "workspace_agents_md")
        .unwrap();
    let skills_idx = names
        .iter()
        .position(|name| *name == "skills_usage_contract")
        .unwrap();
    let tools_idx = names
        .iter()
        .position(|name| *name == "tool_exec_command")
        .unwrap();

    assert!(global_idx < agent_idx);
    assert!(agent_idx < workspace_idx);
    assert!(workspace_idx < skills_idx);
    assert!(tools_idx < global_idx);
    let boundary = crate::shared_prefix_len(&sections);
    assert!(tools_idx < boundary);
    assert!(global_idx >= boundary);
    assert!(sections[boundary..]
        .iter()
        .all(|section| section.layout == crate::PromptLayout::ScopedSuffix));
    assert_eq!(names.last(), Some(&"response_language"));
    let without_guidance_or_skills = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("/another-workspace"),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[ToolSpec {
            name: "ExecCommand".into(),
            description: "Run a shell command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            freeform_grammar: None,
        }],
        ToolPromptContext::default(),
    );
    assert_eq!(
        crate::render_sections(&sections[..boundary]),
        crate::render_sections(
            &without_guidance_or_skills[..crate::shared_prefix_len(&without_guidance_or_skills)]
        )
    );
}

#[test]
fn constrained_repair_section_emitted_for_narrow_fix_patterns() {
    let test_cases = vec![
        "Fix only file: src/prompt/mod.rs by adding a new section",
        "Edit only in exactly one file: src/types.rs and run cargo test",
        "Implement only the file src/lib.rs with narrow changes",
        "Fix only this component. Run exactly: cargo test --lib constrained_repair",
        "Edit only the relevant code. Run exactly this command: cargo fmt --all",
    ];

    for text in test_cases {
        let mut message = sample_message();
        message.body = MessageBody::Text { text: text.into() };

        let sections = build_system_sections(
            &sample_identity(),
            &message,
            Path::new("."),
            &LoadedAgentsMd::default(),
            &LoadedAgentMemory::default(),
            &SkillsRuntimeView::default(),
            &[],
            ToolPromptContext::default(),
        );

        assert!(
            sections.iter().any(|s| s.name == "constrained_repair"),
            "constrained_repair section should be emitted for: {}",
            text
        );
    }
}

#[test]
fn constrained_repair_section_not_emitted_for_general_requests() {
    let general_requests = vec![
        "Fix the bug",
        "Implement the feature",
        "Update the tests",
        "Refactor this code",
        "Narrow the scope",
        "Make a narrow change",
        "Fix only things",
        "Edit only stuff",
    ];

    for text in general_requests {
        let mut message = sample_message();
        message.body = MessageBody::Text { text: text.into() };

        let sections = build_system_sections(
            &sample_identity(),
            &message,
            Path::new("."),
            &LoadedAgentsMd::default(),
            &LoadedAgentMemory::default(),
            &SkillsRuntimeView::default(),
            &[],
            ToolPromptContext::default(),
        );

        assert!(
            !sections.iter().any(|s| s.name == "constrained_repair"),
            "constrained_repair section should NOT be emitted for: {}",
            text
        );
    }
}

#[test]
fn constrained_repair_section_only_applies_to_trusted_operator() {
    let test_cases = vec![
        (
            MessageOrigin::Task {
                task_id: "test-task".into(),
            },
            "Fix only file: src/lib.rs",
        ),
        (
            MessageOrigin::Webhook {
                source: "github".into(),
                event_type: Some("pr_comment".into()),
            },
            "Edit only src/lib.rs. Run exactly: cargo test",
        ),
    ];

    for (origin, text) in test_cases {
        let mut message = sample_message();
        message.origin = origin.clone();
        message.body = MessageBody::Text { text: text.into() };

        let sections = build_system_sections(
            &sample_identity(),
            &message,
            Path::new("."),
            &LoadedAgentsMd::default(),
            &LoadedAgentMemory::default(),
            &SkillsRuntimeView::default(),
            &[],
            ToolPromptContext::default(),
        );

        assert!(
            !sections.iter().any(|s| s.name == "constrained_repair"),
            "constrained_repair section should NOT be emitted for non-operator origin: {:?}",
            origin
        );
    }
}

#[test]
fn constrained_repair_section_does_not_require_silent_wait_without_exact_verification() {
    let mut message = sample_message();
    message.body = MessageBody::Text {
        text: "Fix only file: src/lib.rs by changing the parser".into(),
    };

    let sections = build_system_sections(
        &sample_identity(),
        &message,
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView::default(),
        &[],
        ToolPromptContext::default(),
    );

    let section = sections
        .iter()
        .find(|s| s.name == "constrained_repair")
        .expect("constrained repair section");
    assert!(section
        .content
        .contains("use the narrowest relevant local verification"));
    assert!(!section.content.contains("must wait for the operator"));
}

#[test]
fn skills_usage_contract_appears_when_skills_are_discoverable() {
    let sections = build_system_sections(
        &sample_identity(),
        &sample_message(),
        Path::new("."),
        &LoadedAgentsMd::default(),
        &LoadedAgentMemory::default(),
        &SkillsRuntimeView {
            discoverable_skills: vec![holon_domain::SkillCatalogEntry {
                skill_id: "user:demo".into(),
                root_id: "user_global:test-root".into(),
                skill_dir: "demo".into(),
                name: "demo".into(),
                description: "demo skill".into(),
                path: PathBuf::from("/tmp/user/.agents/skills/demo/SKILL.md"),
                scope: holon_domain::SkillScope::UserGlobal,
            }]
            .into(),
            ..SkillsRuntimeView::default()
        },
        &[],
        ToolPromptContext::default(),
    );
    let section = sections
        .iter()
        .find(|section| section.name == "skills_usage_contract")
        .expect("skills usage contract section");
    assert!(section.content.contains("open that skill's `SKILL.md`"));
    assert!(section
        .content
        .contains("skill bodies are not loaded automatically"));
    assert!(section
        .content
        .contains("use the available managed interface"));
    assert!(section
        .content
        .contains("follow `holon_cli_contract` when present"));
    assert!(section
        .content
        .contains("do not substitute manual directory copying"));
}
