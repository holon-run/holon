use super::super::*;
use super::support::*;

use crate::{
    client::{AgentStateSnapshot, AgentStreamEvent, StateSessionSnapshot, StreamEventEnvelope},
    config::ModelRef,
    model_catalog::ResolvedRuntimeModelPolicy,
    provider::test_support::{ScriptedAgentProvider, ScriptedProviderStep},
    system::{ExecutionProfile, ExecutionSnapshot},
    tui::logging::TuiLogWriter,
    tui::projection::TuiProjection,
    types::{
        AgentIdentityView, AgentKind, AgentLifecycleHint, AgentModelSource, AgentModelState,
        AgentOwnership, AgentProfilePreset, AgentRegistryStatus, AgentState, AgentStatus,
        AgentSummary, AgentTokenUsageSummary, AgentVisibility, ChildAgentSummary,
        ClosureDecision, ClosureOutcome,
        ExternalTriggerSummary, LoadedAgentsMdView, OperatorNotificationRecord, RuntimePosture,
        SkillsRuntimeView, TokenUsage, TrustLevel, TurnTerminalKind, WaitingIntentSummary,
        WorkspaceOccupancyRecord,
    },
};
use serde_json::json;

/// Construct a minimal `AgentSummary` for bootstrapping a TuiProjection.
fn minimal_agent_summary(agent_id: &str) -> AgentSummary {
    let mut state = AgentState::new(agent_id);
    state.status = AgentStatus::AwakeIdle;
    AgentSummary {
        identity: AgentIdentityView {
            agent_id: agent_id.into(),
            kind: AgentKind::Default,
            visibility: AgentVisibility::Public,
            ownership: AgentOwnership::SelfOwned,
            profile_preset: AgentProfilePreset::PublicNamed,
            status: AgentRegistryStatus::Active,
            is_default_agent: agent_id == "default",
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        },
        agent: state,
        active_task_count: 0,
        lifecycle: AgentLifecycleHint::default(),
        model: AgentModelState {
            effective_model: ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            requested_model: None,
            active_model: None,
            fallback_active: false,
            runtime_default_model: ModelRef::parse("anthropic/claude-sonnet-4-6").unwrap(),
            override_model: None,
            override_reasoning_effort: None,
            source: AgentModelSource::RuntimeDefault,
            effective_fallback_models: Vec::new(),
            resolved_policy: ResolvedRuntimeModelPolicy::default(),
        },
        token_usage: AgentTokenUsageSummary {
            total: TokenUsage::new(0, 0),
            total_model_rounds: 0,
            last_turn: None,
        },
        closure: ClosureDecision {
            outcome: ClosureOutcome::Completed,
            waiting_reason: None,
            work_signal: None,
            runtime_posture: RuntimePosture::Awake,
            evidence: Vec::new(),
        },
        execution: ExecutionSnapshot {
            profile: ExecutionProfile::default(),
            policy: ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: vec![],
            workspace_id: None,
            workspace_anchor: "/tmp/agent-home".into(),
            execution_root: "/tmp/agent-home".into(),
            cwd: "/tmp/agent-home".into(),
            execution_root_id: None,
            projection_kind: None,
            access_mode: None,
            worktree_root: None,
        },
        active_workspace_occupancy: None::<WorkspaceOccupancyRecord>,
        loaded_agents_md: LoadedAgentsMdView::default(),
        skills: SkillsRuntimeView::default(),
        active_children: Vec::<ChildAgentSummary>::new(),
        active_waiting_intents: Vec::<WaitingIntentSummary>::new(),
        active_external_triggers: Vec::<ExternalTriggerSummary>::new(),
        recent_operator_notifications: Vec::<OperatorNotificationRecord>::new(),
        recent_brief_count: 0,
        recent_event_count: 0,
    }
}

/// Construct a minimal `AgentStateSnapshot` for bootstrapping a TuiProjection.
fn minimal_snapshot(agent_id: &str, cursor: &str) -> AgentStateSnapshot {
    AgentStateSnapshot {
        agent: minimal_agent_summary(agent_id),
        session: StateSessionSnapshot {
            current_run_id: None,
            pending_count: 0,
            last_turn: None,
        },
        tasks: Vec::new(),
        transcript_tail: Vec::new(),
        operator_messages: Vec::new(),
        timers: Vec::new(),
        work_items: Vec::new(),
        waiting_intents: Vec::new(),
        external_triggers: Vec::new(),
        operator_notifications: Vec::new(),
        workspace: crate::client::StateWorkspaceSnapshot::default(),
        execution: None,
        events_tail: Vec::new(),
        cursor: Some(cursor.into()),
    }
}

/// Convert an `AuditEvent` from storage into an `AgentStreamEvent` for the TUI projection.
fn audit_to_stream_event(event: &crate::types::AuditEvent, seq: u64, agent_id: &str) -> AgentStreamEvent {
    AgentStreamEvent {
        id: event.id.clone(),
        event: event.kind.clone(),
        data: StreamEventEnvelope {
            id: event.id.clone(),
            seq,
            ts: event.created_at,
            agent_id: agent_id.into(),
            event_type: event.kind.clone(),
            projection: None,
            provenance: None,
            payload: event.data.clone(),
        },
    }
}

/// End-to-end smoke test: run a real agent turn through the runtime with a
/// ScriptedAgentProvider, feed resulting audit events through the TUI
/// projection pipeline, and verify `presentation.jsonl` output.
///
/// This validates that the full runtime → TUI pipeline works correctly:
/// - Agent turn completes without error
/// - Audit events are produced for all expected lifecycle stages
/// - TUI projection correctly reduces events into presentation items
/// - `presentation.jsonl` contains valid JSON records with expected structure
#[tokio::test]
async fn e2e_tui_pipeline_smoke_scripted_agent() {
    // ── 1. Create ScriptedAgentProvider ──────────────────────────────
    // First provider call: the agent decides to run `echo ok`.
    // Second provider call: after seeing the tool result, the agent responds.
    let provider = ScriptedAgentProvider::new([
        ScriptedProviderStep::tool_use(
            "toolu_01",
            "ExecCommand",
            json!({ "cmd": "echo ok" }),
        ),
        ScriptedProviderStep::text("Command completed — echo ok succeeded."),
    ]);

    // ── 2. Create RuntimeHandle ──────────────────────────────────────
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(provider),
        "default".into(),
        context_config(),
    )
    .unwrap();

    // ── 3. Create TuiLogWriter with presentation logging ─────────────
    let log_writer = TuiLogWriter::new_temp_with_presentation_logging(65536).unwrap();
    let log_root = log_writer.root().to_path_buf();

    // ── 4. Run one agent turn ────────────────────────────────────────
    let outcome = runtime
        .run_agent_loop(
            "default",
            TrustLevel::TrustedOperator,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: Some(2),
            },
        )
        .await
        .unwrap();

    // Agent should complete without error and produce final text.
    assert!(
        !outcome.final_text.is_empty(),
        "agent should produce final text"
    );
    assert_eq!(
        outcome.terminal_kind,
        TurnTerminalKind::Completed,
        "agent turn should complete normally"
    );

    // ── 5. Read audit events and apply to TuiProjection ──────────────
    let events = runtime.storage().read_recent_events(50).unwrap();
    assert!(!events.is_empty(), "should have audit events after a turn");

    // Verify that expected event kinds are present in the audit log.
    let event_kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert!(
        event_kinds.contains(&"process_execution_requested"),
        "audit log should contain process_execution_requested"
    );
    assert!(
        event_kinds.contains(&"tool_executed"),
        "audit log should contain tool_executed"
    );
    assert!(
        event_kinds.contains(&"assistant_round_recorded"),
        "audit log should contain assistant_round_recorded"
    );
    assert!(
        event_kinds.contains(&"turn_terminal"),
        "audit log should contain turn_terminal"
    );

    // Bootstrap projection and feed all audit events through it.
    let mut projection =
        TuiProjection::from_snapshot(minimal_snapshot("default", "cursor-0"));

    for (idx, event) in events.iter().enumerate() {
        let stream_event = audit_to_stream_event(event, (idx + 1) as u64, "default");
        projection.apply_event(stream_event, &log_writer);
    }

    // ── 6. Verify presentation.jsonl ──────────────────────────────────
    let presentation_path = log_root.join("presentation.jsonl");
    assert!(
        presentation_path.exists(),
        "presentation.jsonl should exist after pipeline events"
    );

    let raw = std::fs::read_to_string(&presentation_path).unwrap();
    let lines: Vec<&str> = raw.trim().lines().collect();
    assert!(
        !lines.is_empty(),
        "presentation.jsonl should have records after a full turn"
    );

    let mut seen_shown = false;
    let mut seen_turn_terminal = false;
    let mut seen_command = false;

    for line in &lines {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("every line must be valid JSON");

        // Check display decisions: at least one record should have decision=shown.
        let displays = record["displays"]
            .as_array()
            .expect("displays must be an array");
        for display in displays {
            if display["decision"].as_str() == Some("shown") {
                seen_shown = true;
            }
        }

        let reducer_kinds: Vec<&str> = record["reducer_event_kinds"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        if reducer_kinds.contains(&"process_execution_requested") {
            seen_command = true;
        }
        if reducer_kinds.contains(&"turn_terminal") {
            seen_turn_terminal = true;
        }
    }

    assert!(seen_command, "should contain process_execution_requested record");
    assert!(seen_shown, "at least one record should have decision=shown");
    assert!(
        seen_turn_terminal,
        "should contain turn_terminal record"
    );
}
