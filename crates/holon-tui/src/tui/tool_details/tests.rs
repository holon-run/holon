use super::*;
use chrono::Utc;
use serde_json::json;

fn record(name: &str, input: Value, result: Value) -> ToolExecutionRecord {
    ToolExecutionRecord {
        id: "tool-test".into(),
        agent_id: "test".into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: Some("turn-test".into()),
        tool_name: name.into(),
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
        duration_ms: 1,
        authority_class: crate::types::AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input,
        output: json!({"envelope": {"tool_name": name, "status": "success", "result": result}}),
        summary: "not a source of details".into(),
        call_binding: None,
        invocation_surface: None,
    }
}

#[test]
fn generic_details_show_structured_input_output_not_envelope() {
    let record = record(
        "GetWorkItem",
        json!({"work_item_id": "work-1"}),
        json!({"state": "open", "todo_list": [{"text": "verify", "state": "pending"}]}),
    );
    let details = tool_detail_text(&record, true);
    for expected in ["work-1", "open", "todo_list", "verify", "pending"] {
        assert!(details.contains(expected), "{details}");
    }
    assert!(!details.contains("envelope"));
    assert!(!details.contains(&record.summary));
}

#[test]
fn batch_shows_all_item_states_and_failed_item_error_first() {
    let record = record(
        "ExecCommandBatch",
        json!({"workdir": "/repo", "items": [{"cmd": "echo ok"}, {"cmd": "false"}]}),
        json!({
            "item_count": 4, "completed_count": 1, "failed_count": 1,
            "rejected_count": 1, "skipped_count": 1,
            "items": [
                {"index": 1, "cmd": "echo ok", "status": "completed",
                 "result": {"disposition": "completed", "exit_status": 0, "stdout_preview": "ok"}},
                {"index": 2, "cmd": "false", "status": "failed", "error_message": "bad command",
                 "result": {"disposition": "completed", "exit_status": 7, "stderr_preview": "failed"}},
                {"index": 3, "cmd": "sleep 100", "status": "rejected", "error_kind": "timeout"},
                {"index": 4, "cmd": "echo skipped", "status": "skipped"}
            ]
        }),
    );
    let details = tool_detail_text(&record, true);
    for expected in [
        "[1] completed",
        "[2] failed",
        "[3] rejected",
        "[4] skipped",
        "exit: 7",
        "bad command",
        "cwd: /repo",
        "stdout: ok",
        "stderr: failed",
    ] {
        assert!(details.contains(expected), "{details}");
    }
    let preview = tool_detail_text(&record, false);
    assert!(preview.contains("[2] failed"));
    assert!(preview.contains("bad command"));
}

#[test]
fn patch_reports_actions_paths_diagnostics_and_limited_diff() {
    let record = record(
        "ApplyPatch",
        json!("*** Begin Patch\n*** End Patch"),
        json!({
            "changed_file_count": 2,
            "changed_paths": ["src/new.rs", "src/lib.rs"],
            "changed_files": [
                {"action": "move", "from_path": "src/old.rs", "path": "src/new.rs"},
                {"action": "modify", "path": "src/lib.rs", "diff_preview": "+new\n-old",
                 "diff_truncated": true}
            ],
            "diagnostics": [{"path": "src/lib.rs", "kind": "context_mismatch", "message": "nearby context used"}],
            "ignored_metadata": [{"path": "src/lib.rs", "kind": "mode", "value": "100644"}]
        }),
    );
    let details = tool_detail_text(&record, true);
    for expected in [
        "R from: src/old.rs",
        "R: src/new.rs",
        "M: src/lib.rs",
        "context_mismatch",
        "nearby context used",
        "ignored_metadata",
        "+new",
        "[diff truncated upstream]",
    ] {
        assert!(details.contains(expected), "{details}");
    }
}

#[test]
fn background_receipts_do_not_claim_process_completion() {
    for disposition in ["promoted_to_task", "already_running"] {
        let record = record(
            "ExecCommand",
            json!({"cmd": "sleep 100", "workdir": "/repo"}),
            json!({
                "disposition": disposition,
                "task_handle": {"task_id": "task-1", "status": "running"},
                "initial_output_preview": "starting",
                "initial_output_truncated": true
            }),
        );
        let details = tool_detail_text(&record, false);
        assert!(details.contains("completion pending"), "{details}");
        assert!(details.contains("task-1"));
        assert!(details.contains("exit: pending"));
        assert!(!details.contains("command: completed"));
        assert!(!details.contains("exit: 0"));
    }
}

#[test]
fn missing_exit_and_empty_output_are_not_success_or_absent_details() {
    let record = record(
        "ExecCommand",
        json!({"cmd": "echo -n"}),
        json!({"disposition": "completed", "exit_status": null, "stdout_preview": "", "stderr_preview": ""}),
    );
    let details = tool_detail_text(&record, false);
    assert!(details.contains("exit: unknown (not reported)"));
    assert!(details.contains("stdout: (empty)"));
    assert!(details.contains("stderr: (empty)"));
    assert!(details.contains("cwd: (default; not recorded)"));
    assert!(!details.contains("exit: 0"));
}

#[test]
fn errors_are_visible_without_a_result_or_summary_parsing() {
    let mut record = record("ExecCommand", json!({"cmd": "false"}), Value::Null);
    record.status = ToolExecutionStatus::Error;
    record.output = json!({
        "envelope": {"status": "error", "error": {
            "kind": "spawn_failed", "message": "shell unavailable", "recovery_hint": "check shell"
        }}
    });
    let details = tool_detail_text(&record, false);
    assert!(details.contains("shell unavailable"));
    assert!(details.contains("spawn_failed"));
    assert!(details.contains("check shell"));
    assert!(details.contains("exit: unknown"));
    assert!(!details.contains("exit: 0"));
}

#[test]
fn truncation_keeps_artifacts_and_obeys_byte_line_and_unicode_limits() {
    for output in ["界".repeat(20_000), "line\n".repeat(20_000)] {
        let record = record(
            "ExecCommand",
            json!({"cmd": "cat large.txt"}),
            json!({
                "disposition": "completed", "exit_status": 0, "stdout_preview": output,
                "truncated": true, "artifacts": [{"path": "/repo/full-output.log"}],
                "output_ref": "tool_execution:tool-test:output",
                "recovery_artifact": {"path": "/repo/canonical.log"}
            }),
        );
        for (expanded, lines, bytes) in [
            (false, PREVIEW_LINES, PREVIEW_BYTES),
            (true, EXPANDED_LINES, EXPANDED_BYTES),
        ] {
            let details = tool_detail_text(&record, expanded);
            assert!(details.contains(TRUNCATED));
            assert!(details.contains("/repo/full-output.log"));
            assert!(details.contains("tool_execution:tool-test:output"));
            assert!(details.contains("/repo/canonical.log"));
            assert!(details.len() <= bytes, "{}", details.len());
            assert!(details.lines().count() <= lines);
        }
    }
}

#[test]
fn unknown_tool_and_empty_output_still_show_input_and_output() {
    let record = record("FutureTool", json!({"query": "hello"}), Value::Null);
    let details = tool_detail_text(&record, false);
    assert!(details.contains("query: hello"));
    assert!(details.contains("output: (none)"));
}

#[test]
fn sensitive_fields_streams_capabilities_and_internal_content_are_hidden() {
    let record = record(
        "FutureTool",
        json!({
            "api_key": "input-key-value",
            "nested": {"access_token": "access-token-value", "ok": "visible"},
            "callback_url": "https://local/api/callbacks/wake/cb_fake-secret",
            "command": "curl --token opaque-value"
        }),
        json!({
            "authorization": "Bearer credential-value",
            "text": "TOKEN=stdout-token-value",
            "diff": "+https://local/api/callbacks/wake/cb_diff-secret",
            "provider_envelope": {"content": "provider-hidden-value"},
            "reasoning": "hidden-reasoning-value",
            "blocks": [{"type": "thinking", "content": "hidden-thinking-value"}],
            "artifacts": [{"path": "/repo/output.log"}],
            "ok": "visible"
        }),
    );
    let details = tool_detail_text(&record, true);
    for forbidden in [
        "input-key-value",
        "access-token-value",
        "cb_fake-secret",
        "opaque-value",
        "credential-value",
        "stdout-token-value",
        "cb_diff-secret",
        "provider-hidden-value",
        "hidden-reasoning-value",
        "hidden-thinking-value",
    ] {
        assert!(!details.contains(forbidden), "{forbidden}: {details}");
    }
    assert!(details.contains("[redacted"));
    assert!(details.contains("visible"));
    assert!(details.contains("/repo/output.log"));
}

#[test]
fn multiline_sensitive_values_and_artifacts_under_private_fields_are_hidden() {
    let record = record(
        "FutureTool",
        json!({
            "cmd": "curl --token \\\nmultiline-command-credential",
            "env": {"AWS_SECRET_ACCESS_KEY": "aws-credential", "apiKey": "camel-key"}
        }),
        json!({
            "text": "Authorization:\nmultiline-header-credential",
            "callbackConfig": {"output_ref": "private-callback-reference"},
            "blocks": [{
                "type": "reasoning",
                "output_ref": "private-reasoning-reference"
            }],
            "artifacts": [{"path": "/repo/public.log"}]
        }),
    );
    let details = tool_detail_text(&record, true);
    for forbidden in [
        "multiline-command-credential",
        "aws-credential",
        "camel-key",
        "multiline-header-credential",
        "private-callback-reference",
        "private-reasoning-reference",
    ] {
        assert!(!details.contains(forbidden), "{forbidden}: {details}");
    }
    assert!(details.contains("/repo/public.log"));
}

#[test]
fn multiline_field_marks_omitted_tail_after_prior_sections_fill_preview() {
    let mut text = DetailText::new(false, 0);
    for _ in 0..16 {
        text.line("earlier section");
    }
    text.field("output", "one\ntwo\nomitted tail");
    let details = text.finish("");
    assert!(details.contains(TRUNCATED));
    assert!(!details.contains("omitted tail"));
    assert!(details.lines().count() <= PREVIEW_LINES);
}

#[test]
fn patch_diffs_are_bounded_and_sensitive_multiline_diffs_are_redacted() {
    for (diff, expected) in [
        ("+new\n".repeat(100), "[diff preview truncated]"),
        (
            "+api_key = \"\n+multiline-diff-credential\n+\"".into(),
            "[redacted sensitive diff]",
        ),
    ] {
        let record = record(
            "ApplyPatch",
            Value::Null,
            json!({
                "changed_file_count": 1,
                "changed_files": [{"action": "modify", "path": "src/lib.rs", "diff_preview": diff}]
            }),
        );
        let details = tool_detail_text(&record, true);
        assert!(details.contains(expected), "{details}");
        assert!(!details.contains("multiline-diff-credential"));
    }
}
