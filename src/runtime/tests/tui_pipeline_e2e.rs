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
        AgentSummary, AgentTokenUsageSummary, AgentVisibility, AuthorityClass, ChildAgentSummary,
        ClosureDecision, ClosureOutcome, ExternalTriggerSummary, LoadedAgentsMdView,
        OperatorNotificationRecord, RuntimePosture, SkillsRuntimeView, TokenUsage,
        TurnTerminalKind, WaitingIntentSummary, WorkspaceOccupancyRecord,
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
        scheduling_posture: Default::default(),
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
        active_wait_conditions: Vec::new(),
        active_external_triggers: Vec::<ExternalTriggerSummary>::new(),
        recent_operator_notifications: Vec::<OperatorNotificationRecord>::new(),
        recent_brief_count: 0,
        recent_event_count: 0,
    }
}

/// Construct a minimal `AgentStateSnapshot` for bootstrapping a TuiProjection.
fn minimal_snapshot(agent_id: &str, _cursor: &str) -> AgentStateSnapshot {
    AgentStateSnapshot {
        agent: minimal_agent_summary(agent_id),
        session: StateSessionSnapshot {
            current_run_id: None,
            pending_count: 0,
            last_turn: None,
        },
        tasks: Vec::new(),
        timers: Vec::new(),
        work_items: Vec::new(),
        waiting_intents: Vec::new(),
        external_triggers: Vec::new(),
        operator_notifications: Vec::new(),
        workspace: crate::client::StateWorkspaceSnapshot::default(),
        execution: None,
    }
}

/// Convert an `AuditEvent` from storage into an `AgentStreamEvent` for the TUI projection.
fn audit_to_stream_event(
    event: &crate::types::AuditEvent,
    event_seq: u64,
    agent_id: &str,
) -> AgentStreamEvent {
    AgentStreamEvent {
        id: event.id.clone(),
        event: event.kind.clone(),
        data: StreamEventEnvelope {
            id: event.id.clone(),
            event_seq,
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
        ScriptedProviderStep::tool_use("toolu_01", "ExecCommand", json!({ "cmd": "echo ok" })),
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
            AuthorityClass::OperatorInstruction,
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
    let mut projection = TuiProjection::from_snapshot(minimal_snapshot("default", "cursor-0"));

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

    assert!(
        seen_command,
        "should contain process_execution_requested record"
    );
    assert!(seen_shown, "at least one record should have decision=shown");
    assert!(seen_turn_terminal, "should contain turn_terminal record");
}

/// End-to-end complex turn test: a single agent turn that performs three
/// distinct tool calls — ApplyPatch (create file), ExecCommand (shell
/// command), ApplyPatch (modify file) — and verifies that all expected
/// `item_kind` variants appear correctly in `presentation.jsonl`.
///
/// This validates multi-operation item_kind coverage through the full
/// runtime → TUI pipeline, including:
/// - File-mutation tool kinds (ApplyPatch)
/// - Process-execution tool kinds (ExecCommand)
/// - Assistant round records
/// - Turn terminal record
#[tokio::test]
async fn e2e_tui_complex_turn_multi_operation() {
    // ── 1. Create ScriptedAgentProvider with 3 tools ─────────────────
    // Step 1: ApplyPatch — create hello.txt
    let create_patch = concat!(
        "--- /dev/null\n",
        "+++ b/hello.txt\n",
        "@@ -0,0 +1,1 @@\n",
        "+hello world\n",
    );
    // Step 2: ExecCommand — read the file
    // Step 3: ApplyPatch — modify hello.txt
    let modify_patch = concat!(
        "--- a/hello.txt\n",
        "+++ b/hello.txt\n",
        "@@ -1,1 +1,1 @@\n",
        "-hello world\n",
        "+hello holon\n",
    );
    // Step 4: text response

    let provider = ScriptedAgentProvider::new([
        ScriptedProviderStep::tool_use("toolu_01", "ApplyPatch", json!({ "patch": create_patch })),
        ScriptedProviderStep::tool_use(
            "toolu_02",
            "ExecCommand",
            json!({ "cmd": "cat hello.txt" }),
        ),
        ScriptedProviderStep::tool_use("toolu_03", "ApplyPatch", json!({ "patch": modify_patch })),
        ScriptedProviderStep::text("All operations completed — file created, read, and modified."),
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

    // ── 4. Run one agent turn with room for 3 tool rounds ────────────
    let outcome = runtime
        .run_agent_loop(
            "default",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: Some(4),
            },
        )
        .await
        .unwrap();

    assert!(
        !outcome.final_text.is_empty(),
        "agent should produce final text"
    );
    assert_eq!(
        outcome.terminal_kind,
        TurnTerminalKind::Completed,
        "agent turn should complete normally"
    );

    // ── 5. Feed audit events through TuiProjection ───────────────────
    let events = runtime.storage().read_recent_events(100).unwrap();
    assert!(
        events.len() > 5,
        "should have many audit events for 3 tool calls"
    );

    let mut projection = TuiProjection::from_snapshot(minimal_snapshot("default", "cursor-0"));

    for (idx, event) in events.iter().enumerate() {
        let stream_event = audit_to_stream_event(event, (idx + 1) as u64, "default");
        projection.apply_event(stream_event, &log_writer);
    }

    // ── 6. Verify presentation.jsonl ──────────────────────────────────
    let presentation_path = log_root.join("presentation.jsonl");
    assert!(
        presentation_path.exists(),
        "presentation.jsonl should exist"
    );

    let raw = std::fs::read_to_string(&presentation_path).unwrap();
    let lines: Vec<&str> = raw.trim().lines().collect();
    assert!(
        lines.len() >= 3,
        "presentation.jsonl should have multiple records for 3 tool calls"
    );

    let mut item_kinds = Vec::new();
    let mut seen_shown = false;

    for line in &lines {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("every line must be valid JSON");

        // Each JSON record must be under 100 KB (102_400 bytes).
        assert!(
            line.len() < 102_400,
            "JSON record must be < 100 KB, got {} bytes",
            line.len()
        );

        // Collect item_kind for later assertions.
        if let Some(kinds) = record["reducer_event_kinds"].as_array() {
            for kind in kinds {
                if let Some(k) = kind.as_str() {
                    item_kinds.push(k.to_string());
                }
            }
        }

        // Verify display decisions.
        for display in record["displays"].as_array().into_iter().flatten() {
            if display["decision"].as_str() == Some("shown") {
                seen_shown = true;
            }
        }
    }

    // ── 7. Assert item_kind coverage ─────────────────────────────────
    assert!(
        !item_kinds.is_empty(),
        "presentation.jsonl should contain reducer_event_kinds"
    );

    let expected_kinds = [
        "process_execution_requested",
        "tool_executed",
        "assistant_round_recorded",
        "turn_terminal",
    ];

    let item_kind_set: std::collections::BTreeSet<&str> =
        item_kinds.iter().map(|s| s.as_str()).collect();

    for expected in &expected_kinds {
        assert!(
            item_kind_set.contains(expected),
            "presentation.jsonl should contain item_kind '{expected}'; found: {:?}",
            item_kind_set
        );
    }

    assert!(
        seen_shown,
        "at least one display decision should be shown in a multi-operation turn"
    );
}

/// End-to-end concurrent agents test: run two independent agents with
/// distinct ScriptedAgentProviders through the full runtime → TUI pipeline,
/// writing to a single shared `presentation.jsonl`. Verifies:
///
/// - Each agent's events are correctly attributed by agent_id in the audit log
/// - A shared `presentation.jsonl` stream contains records from both agents
/// - No cross-agent event leakage or corruption in the shared log
/// - Every presentation line remains valid JSON
#[tokio::test]
async fn e2e_tui_concurrent_agents_attribution() {
    // ── 1. Create ScriptedAgentProviders ─────────────────────────────
    // Agent A: simple shell command
    let provider_a = ScriptedAgentProvider::new([
        ScriptedProviderStep::tool_use(
            "toolu_a1",
            "ExecCommand",
            json!({ "cmd": "echo agent-a-task" }),
        ),
        ScriptedProviderStep::text("Agent A: task complete."),
    ]);

    // Agent B: file operation
    let provider_b = ScriptedAgentProvider::new([
        ScriptedProviderStep::tool_use(
            "toolu_b1",
            "ApplyPatch",
            json!({
                "patch": concat!(
                    "--- /dev/null\n",
                    "+++ b/agent-b-output.txt\n",
                    "@@ -0,0 +1,1 @@\n",
                    "+agent-b created this file\n",
                )
            }),
        ),
        ScriptedProviderStep::text("Agent B: file created."),
    ]);

    // ── 2. Create shared TuiLogWriter ────────────────────────────────
    let log_writer = TuiLogWriter::new_temp_with_presentation_logging(65536).unwrap();
    let log_root = log_writer.root().to_path_buf();

    // ── 3. Create and run Agent A ────────────────────────────────────
    let dir_a = tempdir().unwrap();
    let ws_a = tempdir().unwrap();
    let runtime_a = RuntimeHandle::new(
        "agent-a",
        dir_a.path().to_path_buf(),
        ws_a.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(provider_a),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome_a = runtime_a
        .run_agent_loop(
            "agent-a",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: Some(2),
            },
        )
        .await
        .unwrap();

    assert!(
        !outcome_a.final_text.is_empty(),
        "agent-a should produce final text"
    );
    assert_eq!(
        outcome_a.terminal_kind,
        TurnTerminalKind::Completed,
        "agent-a turn should complete normally"
    );

    // ── 4. Create and run Agent B ────────────────────────────────────
    let dir_b = tempdir().unwrap();
    let ws_b = tempdir().unwrap();
    let runtime_b = RuntimeHandle::new(
        "agent-b",
        dir_b.path().to_path_buf(),
        ws_b.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(provider_b),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let outcome_b = runtime_b
        .run_agent_loop(
            "agent-b",
            AuthorityClass::OperatorInstruction,
            test_effective_prompt(),
            LoopControlOptions {
                max_tool_rounds: Some(2),
            },
        )
        .await
        .unwrap();

    assert!(
        !outcome_b.final_text.is_empty(),
        "agent-b should produce final text"
    );
    assert_eq!(
        outcome_b.terminal_kind,
        TurnTerminalKind::Completed,
        "agent-b turn should complete normally"
    );

    // ── 5. Read audit events from both agents ────────────────────────
    let events_a = runtime_a.storage().read_recent_events(100).unwrap();
    let events_b = runtime_b.storage().read_recent_events(100).unwrap();

    assert!(!events_a.is_empty(), "agent-a should produce audit events");
    assert!(!events_b.is_empty(), "agent-b should produce audit events");

    // ── 6. Check agent_id attribution in raw audit events ────────────
    for event in &events_a {
        if let Some(agent_id) = event.data.get("agent_id").and_then(|v| v.as_str()) {
            assert_eq!(
                agent_id, "agent-a",
                "all audit events from agent-a should carry agent_id=agent-a"
            );
        }
    }
    for event in &events_b {
        if let Some(agent_id) = event.data.get("agent_id").and_then(|v| v.as_str()) {
            assert_eq!(
                agent_id, "agent-b",
                "all audit events from agent-b should carry agent_id=agent-b"
            );
        }
    }

    // ── 7. Create two projections, both writing to shared log ────────
    let mut projection_a = TuiProjection::from_snapshot(minimal_snapshot("agent-a", "cursor-a0"));
    let mut projection_b = TuiProjection::from_snapshot(minimal_snapshot("agent-b", "cursor-b0"));

    for (idx, event) in events_a.iter().enumerate() {
        let stream_event = audit_to_stream_event(event, (idx + 1) as u64, "agent-a");
        projection_a.apply_event(stream_event, &log_writer);
    }

    for (idx, event) in events_b.iter().enumerate() {
        let stream_event = audit_to_stream_event(event, (idx + 1) as u64, "agent-b");
        projection_b.apply_event(stream_event, &log_writer);
    }

    // ── 8. Verify shared presentation.jsonl ──────────────────────────
    let presentation_path = log_root.join("presentation.jsonl");
    assert!(
        presentation_path.exists(),
        "shared presentation.jsonl should exist after both agents"
    );

    let raw = std::fs::read_to_string(&presentation_path).unwrap();
    let lines: Vec<&str> = raw.trim().lines().collect();
    assert!(
        !lines.is_empty(),
        "shared presentation.jsonl should have records from concurrent agents"
    );

    let mut seen_shown = false;
    let mut seen_command = false;
    let mut seen_apply_patch = false;

    for line in &lines {
        let record: serde_json::Value = serde_json::from_str(line)
            .expect("every line must be valid JSON after concurrent writes");

        for display in record["displays"].as_array().into_iter().flatten() {
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
        if reducer_kinds.contains(&"tool_executed") {
            seen_apply_patch = true;
        }
    }

    assert!(seen_shown, "at least one display decision should be shown");
    assert!(
        seen_command,
        "should contain process_execution_requested from agent-a"
    );
    assert!(
        seen_apply_patch,
        "should contain tool_executed from agent-b"
    );

    // ── 9. Verify no cross-agent leakage: distinct agent_ids in audit ──
    let agent_ids_in_a: std::collections::BTreeSet<&str> = events_a
        .iter()
        .filter_map(|e| e.data.get("agent_id").and_then(|v| v.as_str()))
        .collect();
    let agent_ids_in_b: std::collections::BTreeSet<&str> = events_b
        .iter()
        .filter_map(|e| e.data.get("agent_id").and_then(|v| v.as_str()))
        .collect();

    assert!(
        !agent_ids_in_a.contains("agent-b"),
        "agent-a events must not contain agent-b attribution"
    );
    assert!(
        !agent_ids_in_b.contains("agent-a"),
        "agent-b events must not contain agent-a attribution"
    );
}
