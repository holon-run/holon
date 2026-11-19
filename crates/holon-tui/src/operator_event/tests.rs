use super::{
    present_operator_event, OperatorDisplayMode, OperatorEventCategory,
    OperatorPresentationContext, OperatorVisibility,
};
use serde_json::json;

const RFC_OPERATOR_EVENT_KINDS: &[&str] = &[
    "operator_notification_requested",
    "brief_created",
    "operator_interjection_admitted",
    "message_enqueued",
    "message_admitted",
    "message_processing_started",
    "message_processing_aborted",
    "turn_started",
    "assistant_round_recorded",
    "text_only_round_observed",
    "provider_round_completed",
    "process_execution_requested",
    "tool_executed",
    "tool_execution_failed",
    "truncated_mutation_tool_call_rejected",
    "work_item_written",
    "work_item_picked",
    "work_item_focus_released",
    "work_item_enqueue_requested",
    "work_item_turn_end_committed",
    "work_item_turn_end_commit_skipped",
    "work_item_stale_reminder_injected",
    "work_item_stale_reminder_skipped",
    "missing_current_work_item_before_wait",
    "work_queue_redispatch_stalled",
    "work_item_delegation_created",
    "work_item_delegation_completed",
    "task_created",
    "task_status_updated",
    "task_result_received",
    "task_child_spawned",
    "task_input_delivered",
    "task_create_requested",
    "supervised_child_task_monitor_reattached",
    "supervised_child_task_recovery_failed",
    "command_task_runner_failed",
    "command_task_result_enqueue_failed",
    "callback_delivered",
    "timer_create_requested",
    "timer_created",
    "timer_fired",
    "timer_fire_failed",
    "workspace_attach_requested",
    "workspace_attached",
    "workspace_entered",
    "workspace_exit_requested",
    "workspace_exited",
    "workspace_detach_requested",
    "workspace_detached",
    "workspace_used",
    "worktree_entered",
    "worktree_exited",
    "worktree_created_for_task",
    "task_worktree_metadata_recorded",
    "worktree_retained_for_review",
    "worktree_auto_cleaned_up",
    "worktree_auto_cleanup_failed",
    "task_worktree_cleanup_already_removed",
    "task_worktree_cleanup_retained",
    "task_worktree_cleanup_failed",
    "task_worktree_branch_cleanup_retained",
    "skill_activated",
    "skill_installed",
    "skill_uninstalled",
    "agent_created",
    "agent_model_override_requested",
    "agent_model_override_set",
    "agent_model_override_clear_requested",
    "agent_model_override_cleared",
    "agent_state_changed",
    "state_changed",
    "session_state_changed",
    "control_request_admitted",
    "control_applied",
    "current_run_aborted",
    "wake_requested",
    "continuation_trigger_received",
    "continuation_resolved",
    "closure_decided",
    "scheduler_decision",
    "system_tick_emitted",
    "system_tick_suppressed",
    "runtime_service_shutdown_requested",
    "debug_prompt_requested",
    "turn_context_built",
    "turn_context_length_exceeded",
    "turn_local_baseline_over_budget",
    "turn_local_compaction_applied",
    "turn_local_checkpoint_requested",
    "turn_local_checkpoint_recorded",
    "turn_local_checkpoint_resume_requested",
    "episode_memory_finalized",
    "working_memory_updated",
    "recovery_cleared_missing_worktree_session",
    "max_output_tokens_recovery",
    "operator_delivery_submitted",
    "operator_delivery_completed",
    "operator_notification_mirror_failed",
    "operator_transport_binding_upserted",
];

#[test]
fn rfc_operator_events_have_friendly_default_presentations() {
    let context = OperatorPresentationContext::default();
    for kind in RFC_OPERATOR_EVENT_KINDS {
        let presentation = present_operator_event(kind, &json!({}), kind, &context);
        assert_ne!(
            presentation.title, *kind,
            "{kind} should not render its raw event kind as title"
        );
        assert_ne!(
            presentation.summary, *kind,
            "{kind} should not render its raw event kind as summary"
        );
        assert!(
            !presentation.title.contains('_'),
            "{kind} title should be human readable: {}",
            presentation.title
        );
    }
}

#[test]
fn brief_created_lightweight_payload_uses_summary_and_work_item_visibility() {
    let mut context = OperatorPresentationContext::default();
    context.completed_work_item_ids.insert("wi-1".into());
    let payload = json!({
        "brief_id": "brief-1",
        "agent_id": "default",
        "workspace_id": "agent_home",
        "work_item_id": "wi-1",
        "kind": "result",
        "created_at": "2026-01-01T00:00:00Z",
        "content_char_count": 22
    });

    let presentation = present_operator_event("brief_created", &payload, "fallback", &context);

    assert_eq!(presentation.visibility, OperatorVisibility::WorkDone);
    assert_eq!(presentation.body, None);
    assert_eq!(presentation.summary, "Brief");
}

#[test]
fn operator_display_mode_parse_accepts_names_and_numeric_aliases() {
    assert_eq!(
        OperatorDisplayMode::parse("3"),
        Some(OperatorDisplayMode::Info)
    );
    assert_eq!(
        OperatorDisplayMode::parse(" info "),
        Some(OperatorDisplayMode::Info)
    );
    assert_eq!(
        OperatorDisplayMode::parse("VERBOSE"),
        Some(OperatorDisplayMode::Verbose)
    );
    assert_eq!(
        OperatorDisplayMode::parse("5"),
        Some(OperatorDisplayMode::Debug)
    );
    assert_eq!(OperatorDisplayMode::parse("trace"), None);
}

#[test]
fn display_mode_filter_handles_malformed_message_payloads() {
    let context = OperatorPresentationContext::default();
    assert!(!super::is_operator_event_in_display_mode(
        "message_enqueued",
        &json!({ "origin": { "kind": "channel" } }),
        "message",
        &context,
        OperatorDisplayMode::Info
    ));
    assert!(!super::is_operator_event_in_display_mode(
        "message_enqueued",
        &json!({ "origin": "operator" }),
        "message",
        &context,
        OperatorDisplayMode::Info
    ));
    assert!(super::is_operator_event_in_display_mode(
        "message_enqueued",
        &json!({ "origin": { "kind": "operator" }, "body": { "kind": "text", "text": "hi" } }),
        "message",
        &context,
        OperatorDisplayMode::Info
    ));
}

#[test]
fn external_wake_events_are_verbose_only_when_they_resume_work() {
    let context = OperatorPresentationContext::default();
    let triggered = json!({
        "source": "github",
        "resource": "pull/42",
        "disposition": "Triggered"
    });
    assert!(!super::is_operator_event_in_display_mode(
        "callback_delivered",
        &triggered,
        "callback_delivered",
        &context,
        OperatorDisplayMode::Info
    ));
    assert!(super::is_operator_event_in_display_mode(
        "callback_delivered",
        &triggered,
        "callback_delivered",
        &context,
        OperatorDisplayMode::Verbose
    ));

    let coalesced = json!({
        "source": "github",
        "resource": "pull/42",
        "disposition": "Coalesced"
    });
    assert!(!super::is_operator_event_in_display_mode(
        "callback_delivered",
        &coalesced,
        "callback_delivered",
        &context,
        OperatorDisplayMode::Verbose
    ));
    assert!(super::is_operator_event_in_display_mode(
        "callback_delivered",
        &coalesced,
        "callback_delivered",
        &context,
        OperatorDisplayMode::Debug
    ));

    let presentation = present_operator_event(
        "callback_delivered",
        &triggered,
        "callback_delivered",
        &context,
    );
    assert_eq!(
        presentation.summary,
        "External event received from github for pull/42; resuming agent"
    );
}

#[test]
fn work_item_lifecycle_display_policy_is_level_driven() {
    let context = OperatorPresentationContext::default();
    let created = json!({
        "action": "created",
        "record": {
            "id": "wi-1",
            "agent_id": "default",
            "workspace_id": "agent_home",
            "objective": "ship lifecycle display",
            "state": "open",
            "plan_status": "draft",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }
    });
    let created_presentation =
        present_operator_event("work_item_written", &created, "fallback", &context);
    assert_eq!(
        created_presentation.visibility,
        OperatorVisibility::TurnResult
    );
    assert_eq!(
        created_presentation.body.as_deref(),
        Some("ship lifecycle display")
    );
    assert!(super::is_operator_event_in_display_mode(
        "work_item_written",
        &created,
        "fallback",
        &context,
        OperatorDisplayMode::Info
    ));

    let updated = json!({
        "action": "updated",
        "record": {
            "id": "wi-1",
            "agent_id": "default",
            "workspace_id": "agent_home",
            "objective": "ship lifecycle display",
            "state": "open",
            "plan_status": "draft",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:01Z"
        }
    });
    let updated_presentation =
        present_operator_event("work_item_written", &updated, "fallback", &context);
    assert_eq!(
        updated_presentation.visibility,
        OperatorVisibility::Progress
    );
    assert!(!super::is_operator_event_in_display_mode(
        "work_item_written",
        &updated,
        "fallback",
        &context,
        OperatorDisplayMode::Info
    ));
    assert!(super::is_operator_event_in_display_mode(
        "work_item_written",
        &updated,
        "fallback",
        &context,
        OperatorDisplayMode::Verbose
    ));

    let completed = json!({
        "action": "completed",
        "record": {
            "id": "wi-1",
            "agent_id": "default",
            "workspace_id": "agent_home",
            "objective": "ship lifecycle display",
            "state": "completed",
            "plan_status": "ready",
            "result_summary": "merged PR",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:02Z"
        }
    });
    let completed_presentation =
        present_operator_event("work_item_written", &completed, "fallback", &context);
    assert_eq!(
        completed_presentation.visibility,
        OperatorVisibility::WorkDone
    );
    assert_eq!(completed_presentation.summary, "Work completed: merged PR");
    assert!(super::is_operator_event_in_display_mode(
        "work_item_written",
        &completed,
        "fallback",
        &context,
        OperatorDisplayMode::Info
    ));

    let lightweight = json!({
        "agent_id": "default",
        "work_item_id": "wi-1",
        "workspace_id": "agent_home",
        "revision": 3,
        "action": "completed",
        "state": "completed",
        "plan_status": "ready",
        "readiness": "completed",
        "updated_at": "2026-01-01T00:00:03Z",
        "objective_preview": "ship lifecycle display",
        "objective_len": 22,
        "result_summary_preview": "merged slim PR"
    });
    let lightweight_presentation =
        present_operator_event("work_item_written", &lightweight, "fallback", &context);
    assert_eq!(
        lightweight_presentation.visibility,
        OperatorVisibility::WorkDone
    );
    assert_eq!(
        lightweight_presentation.summary,
        "Work completed: merged slim PR"
    );
}

#[test]
fn continuation_triggers_are_verbose_when_they_explain_resume() {
    let context = OperatorPresentationContext::default();
    assert!(super::is_operator_event_in_display_mode(
        "continuation_trigger_received",
        &json!({ "trigger_kind": "task_result", "task_terminal": true }),
        "continuation_trigger_received",
        &context,
        OperatorDisplayMode::Verbose
    ));
    assert!(!super::is_operator_event_in_display_mode(
        "continuation_trigger_received",
        &json!({ "trigger_kind": "control_tick" }),
        "continuation_trigger_received",
        &context,
        OperatorDisplayMode::Verbose
    ));
}

#[test]
fn assistant_round_recorded_distinguishes_text_from_tool_requests() {
    let context = OperatorPresentationContext::default();
    let text = present_operator_event(
        "assistant_round_recorded",
        &json!({ "text_preview": "thinking", "tool_names": ["ExecCommand"] }),
        "fallback",
        &context,
    );
    assert_eq!(text.visibility, OperatorVisibility::Progress);
    assert_eq!(text.category, OperatorEventCategory::AssistantProgress);
    assert_eq!(text.summary, "Assistant round: thinking");

    let multiline = present_operator_event(
        "assistant_round_recorded",
        &json!({ "text_preview": "thinking\n\nabout\ttools  now" }),
        "fallback",
        &context,
    );
    assert_eq!(
        multiline.summary,
        "Assistant round: thinking about tools now"
    );

    let tools = present_operator_event(
        "assistant_round_recorded",
        &json!({ "text_preview": null, "tool_names": ["ExecCommand", "ReadFile"] }),
        "fallback",
        &context,
    );
    assert_eq!(
        tools.summary,
        "Assistant requested tools: ExecCommand, ReadFile"
    );
    assert_eq!(tools.body.as_deref(), Some("ExecCommand, ReadFile"));

    let empty = present_operator_event(
        "assistant_round_recorded",
        &json!({ "text_preview": null, "tool_names": [], "stop_reason": "end_turn" }),
        "fallback",
        &context,
    );
    assert_eq!(
        empty.summary,
        "Assistant round completed without text (stop=end_turn)"
    );
}

#[test]
fn runtime_checkpoint_round_is_debug_only_and_explicitly_labeled() {
    let context = OperatorPresentationContext::default();
    let payload = json!({
        "round_purpose": "runtime_checkpoint",
        "checkpoint_mode": "full",
        "text_preview": "internal checkpoint text"
    });
    let checkpoint =
        present_operator_event("assistant_round_recorded", &payload, "fallback", &context);

    assert_eq!(checkpoint.visibility, OperatorVisibility::Trace);
    assert_eq!(checkpoint.title, "Runtime checkpoint");
    assert_eq!(
        checkpoint.body.as_deref(),
        Some("Internal continuity evidence (mode=full)")
    );
    assert!(!super::is_operator_event_in_display_mode(
        "assistant_round_recorded",
        &payload,
        "fallback",
        &context,
        OperatorDisplayMode::Verbose
    ));
    assert!(super::is_operator_event_in_display_mode(
        "assistant_round_recorded",
        &payload,
        "fallback",
        &context,
        OperatorDisplayMode::Debug
    ));
}

#[test]
fn provider_round_completed_presents_provider_telemetry() {
    let context = OperatorPresentationContext::default();
    let provider = present_operator_event(
        "provider_round_completed",
        &json!({
            "round": 2,
            "active_model": "deepseek-v4-flash",
            "stop_reason": "tool_use",
            "input_tokens": 12,
            "output_tokens": 7,
            "tool_call_count": 1
        }),
        "fallback",
        &context,
    );
    assert_eq!(provider.visibility, OperatorVisibility::Progress);
    assert_eq!(provider.category, OperatorEventCategory::AssistantProgress);
    assert_eq!(
        provider.summary,
        "Provider round 2: model=deepseek-v4-flash; stop=tool_use; 12/7 tokens; tools=1"
    );
    assert_eq!(provider.title, "Provider round completed");
}

#[test]
fn completed_turn_terminal_is_trace_but_failures_are_turn_results() {
    let context = OperatorPresentationContext::default();
    let completed = present_operator_event(
        "turn_terminal",
        &json!({ "kind": "completed", "duration_ms": 42 }),
        "turn completed",
        &context,
    );
    assert_eq!(completed.visibility, OperatorVisibility::Trace);
    assert_eq!(completed.summary, "Turn completed: 42 ms");

    let aborted = present_operator_event(
        "turn_terminal",
        &json!({ "kind": "aborted", "last_assistant_message": "need more input" }),
        "turn aborted",
        &context,
    );
    assert_eq!(aborted.visibility, OperatorVisibility::TurnResult);
    assert_eq!(aborted.summary, "Turn aborted: need more input");
}

#[test]
fn provider_lineage_failure_events_are_turn_results() {
    let context = OperatorPresentationContext::default();
    let deferred = present_operator_event(
        "deferred_to_fallback",
        &json!({
            "operator_message": "OpenAI Codex authentication failed. Queued fallback turn on anthropic/claude-sonnet-4-6.",
            "fallback_model_ref": "anthropic/claude-sonnet-4-6"
        }),
        "fallback",
        &context,
    );
    assert_eq!(deferred.visibility, OperatorVisibility::TurnResult);
    assert_eq!(deferred.category, OperatorEventCategory::Runtime);
    assert!(deferred
        .summary
        .contains("Provider failed; fallback queued"));
    assert!(deferred
        .summary
        .contains("OpenAI Codex authentication failed"));

    let terminal = present_operator_event(
        "turn_terminal",
        &json!({ "kind": "deferred_to_fallback", "last_assistant_message": "fallback queued" }),
        "turn terminal",
        &context,
    );
    assert_eq!(terminal.visibility, OperatorVisibility::Trace);
}

#[test]
fn stalled_queued_work_item_is_visible_in_info_mode() {
    let payload = json!({
        "work_item_id": "wi-1",
        "no_progress_attempts": 5,
        "awaiting": "new_external_signal_or_work_item_update"
    });
    let context = OperatorPresentationContext::default();
    let presentation = present_operator_event(
        "work_queue_redispatch_stalled",
        &payload,
        "fallback",
        &context,
    );

    assert_eq!(presentation.category, OperatorEventCategory::WorkItem);
    assert_eq!(presentation.visibility, OperatorVisibility::TurnResult);
    assert_eq!(presentation.title, "Queued work stalled");
    assert!(presentation.summary.contains("wi-1"));
    assert!(presentation.summary.contains("5 redispatch attempts"));
    assert!(super::is_operator_event_in_display_mode(
        "work_queue_redispatch_stalled",
        &payload,
        "fallback",
        &context,
        OperatorDisplayMode::Info
    ));
}

#[test]
fn turn_local_events_explain_context_management() {
    let context = OperatorPresentationContext::default();
    let checkpoint = present_operator_event(
        "turn_local_checkpoint_resume_requested",
        &json!({ "round": 3 }),
        "turn_local_checkpoint_resume_requested",
        &context,
    );
    assert_eq!(
        checkpoint.summary,
        "Refreshing local context; asking the model to continue"
    );

    let recovery = present_operator_event(
        "max_output_tokens_recovery",
        &json!({ "attempt": 2 }),
        "max_output_tokens_recovery",
        &context,
    );
    assert_eq!(
        recovery.summary,
        "Output limit recovery: continuing (attempt 2)"
    );
}

#[test]
fn state_sync_is_not_conversation_presentation() {
    let presentation = present_operator_event(
        "agent_state_changed",
        &json!({ "status": "AwakeRunning" }),
        "agent_state_changed",
        &OperatorPresentationContext::default(),
    );
    assert_eq!(presentation.category, OperatorEventCategory::StateSync);
    assert_eq!(presentation.visibility, OperatorVisibility::Trace);
    assert!(!presentation.is_conversation_candidate());
}

#[test]
fn delegated_child_events_use_supervision_vocabulary() {
    let context = OperatorPresentationContext::default();
    let spawned = present_operator_event(
        "task_child_spawned",
        &json!({
            "id": "task-1",
            "detail": {
                "child_agent_id": "child-1",
                "workspace_mode": "worktree"
            }
        }),
        "fallback",
        &context,
    );
    assert_eq!(spawned.category, OperatorEventCategory::Task);
    assert_eq!(spawned.title, "Delegated child started");
    assert_eq!(
        spawned.summary,
        "Delegated child child-1 started under supervision task task-1 (worktree)"
    );

    let followup = present_operator_event(
        "task_input_delivered",
        &json!({
            "task_id": "task-1",
            "child_agent_id": "child-1",
            "input_target": "child_followup"
        }),
        "fallback",
        &context,
    );
    assert_eq!(followup.title, "Parent follow-up delivered");
    assert_eq!(
        followup.summary,
        "Parent follow-up delivered to child child-1 via supervision task task-1"
    );

    let notification = present_operator_event(
        "operator_notification_requested",
        &json!({
            "requested_by_agent_id": "child-1",
            "target_operator_boundary": "parent_supervisor",
            "summary": "need parent decision"
        }),
        "fallback",
        &context,
    );
    assert_eq!(notification.visibility, OperatorVisibility::ActionRequired);
    assert_eq!(notification.title, "Parent supervision needed");
    assert_eq!(
        notification.summary,
        "Child child-1 needs parent supervision: need parent decision"
    );
}

#[test]
fn process_execution_requested_uses_command_vocabulary() {
    let presentation = present_operator_event(
        "process_execution_requested",
        &json!({
            "surface": "ExecCommand",
            "cmd_preview": "cargo test -q tui::chat",
        }),
        "process_execution_requested",
        &OperatorPresentationContext::default(),
    );

    assert_eq!(presentation.category, OperatorEventCategory::Tool);
    assert_eq!(presentation.visibility, OperatorVisibility::Trace);
    assert_eq!(
        presentation.summary,
        "Command started: cargo test -q tui::chat"
    );

    let background = present_operator_event(
        "process_execution_requested",
        &json!({ "surface": "command_task" }),
        "process_execution_requested",
        &OperatorPresentationContext::default(),
    );
    assert_eq!(background.summary, "Background command started");
    assert_eq!(background.body.as_deref(), None);
}

#[test]
fn workspace_events_prefer_execution_root_context() {
    let presentation = present_operator_event(
        "workspace_used",
        &json!({
            "workspace_id": "ws-1",
            "workspace_label": "holon",
            "execution_root": "/repo/holon/worktree"
        }),
        "workspace_used",
        &OperatorPresentationContext::default(),
    );

    assert_eq!(presentation.title, "Workspace used");
    assert_eq!(presentation.summary, "Workspace used: /repo/holon/worktree");
}

#[test]
fn tool_executed_uses_friendly_tool_labels() {
    let context = OperatorPresentationContext::default();
    let samples = [
        ("ExecCommandBatch", "Command batch finished"),
        ("ListTasks", "Listed tasks"),
        ("TaskList", "Listed tasks"),
        ("TaskStatus", "Read task status"),
        ("CreateAgent", "Created agent"),
        ("InvokeAgent", "Invoked agent"),
        ("AttachWorkspace", "Workspace attached"),
        ("DetachWorkspace", "Workspace detached"),
        ("SwitchWorkspace", "Workspace selected"),
        ("CreateWorktree", "Worktree created"),
        ("RemoveWorktree", "Worktree removed"),
        ("UseWorkspace", "Workspace selected"),
        ("Sleep", "Slept"),
        ("WaitFor", "Waiting"),
    ];
    for (tool_name, expected) in samples {
        let presentation = present_operator_event(
            "tool_executed",
            &json!({ "tool_name": tool_name }),
            "tool_executed",
            &context,
        );
        assert_eq!(presentation.summary, expected);
    }

    let failed = present_operator_event(
        "tool_execution_failed",
        &json!({ "tool_name": "ExecCommand", "exec_command_cmd": "cargo test" }),
        "tool_execution_failed",
        &context,
    );
    assert_eq!(failed.title, "Command failed");
    assert_eq!(failed.summary, "Command failed: cargo test");

    let batch = present_operator_event(
        "tool_executed",
        &json!({
            "tool_name": "ExecCommandBatch",
            "exec_command_batch_items": [
                { "index": 0, "cmd": "rg operator_event src" },
                { "index": 1, "cmd": "cargo test operator_event" },
                { "index": 2, "cmd": "cargo fmt --check" }
            ]
        }),
        "tool_executed",
        &context,
    );
    assert_eq!(batch.title, "Command batch finished");
    assert_eq!(
        batch.summary,
        "Command batch finished: 3 items: rg operator_event src; cargo test operator_event; ..."
    );

    let script = present_operator_event(
        "process_execution_requested",
        &json!({
            "surface": "ExecCommand",
            "cmd_preview": "[omitted: command contains heredoc or inline script]",
            "cmd_display": "python - <<'PY'\nprint('hello')"
        }),
        "process_execution_requested",
        &context,
    );
    assert_eq!(
        script.summary,
        "Command started: python - <<'PY'\nprint('hello')"
    );

    let workspace = present_operator_event(
        "tool_executed",
        &json!({
            "tool_name": "UseWorkspace",
            "summary": "using workspace holon at /repo/holon"
        }),
        "tool_executed",
        &context,
    );
    assert_eq!(workspace.title, "Workspace selected");
    assert_eq!(
        workspace.summary,
        "Workspace selected: using workspace holon at /repo/holon"
    );

    let unknown = present_operator_event(
        "tool_executed",
        &json!({ "tool_name": "CustomTool" }),
        "tool_executed",
        &context,
    );
    assert_eq!(unknown.title, "Tool finished");
    assert_eq!(unknown.summary, "Tool finished: CustomTool");

    let unknown_failed = present_operator_event(
        "tool_execution_failed",
        &json!({ "tool_name": "CustomTool" }),
        "tool_execution_failed",
        &context,
    );
    assert_eq!(unknown_failed.title, "Tool failed");
    assert_eq!(unknown_failed.summary, "Tool failed: CustomTool");
}
