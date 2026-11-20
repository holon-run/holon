use holon_domain::ApplyPatchSurface;
use holon_tools::builtins::{
    apply_patch_tool, builtin_tool_definitions, render_tool_result_for_model,
};
use holon_tools::{ToolError, ToolResult};

#[test]
fn builtin_tool_descriptions_come_from_markdown_files() {
    let definitions = builtin_tool_definitions().unwrap();

    for definition in definitions {
        let path = description_path(&definition.spec.name)
            .unwrap_or_else(|| panic!("missing description path for {}", definition.spec.name));
        let markdown = std::fs::read_to_string(manifest_path(path)).unwrap();
        assert_eq!(
            definition.spec.description, markdown,
            "{}",
            definition.spec.name
        );
    }

    let codex = apply_patch_tool::definition_for_surface(ApplyPatchSurface::CodexDslFreeform)
        .unwrap()
        .spec
        .description;
    assert_eq!(
        codex,
        std::fs::read_to_string(manifest_path(
            "src/tool/tool_descriptions/apply_patch_codex_dsl_freeform.md"
        ))
        .unwrap()
    );
}

#[test]
fn non_command_tools_default_to_canonical_json_render() {
    let result = ToolResult::success(
        "GetAgent",
        serde_json::json!({"agent": {"id": "default"}}),
        None,
    );
    let rendered = render_tool_result_for_model(&result).unwrap();
    assert!(rendered.starts_with("{\"tool_name\":\"GetAgent\""));
}

#[test]
fn tool_errors_use_shared_model_visible_receipt() {
    let result = ToolResult::error(
        "ExecCommand",
        ToolError::new("invalid_tool_input", "missing required field")
            .with_details(serde_json::json!({ "field": "cmd" }))
            .with_recovery_hint("provide `cmd`"),
    );
    let rendered = render_tool_result_for_model(&result).unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(receipt["ok"], false);
    assert_eq!(receipt["tool_name"], "ExecCommand");
    assert_eq!(receipt["kind"], "invalid_tool_input");
    assert_eq!(receipt["field"], "cmd");
    assert_eq!(receipt["hint"], "provide `cmd`");
}

fn manifest_path(path: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn description_path(tool_name: &str) -> Option<&'static str> {
    Some(match tool_name {
        "GetAgent" => "src/tool/tool_descriptions/get_agent.md",
        "ApplyPatch" => "src/tool/tool_descriptions/apply_patch_unified_diff_json.md",
        "AttachWorkspace" => "src/tool/tool_descriptions/attach_workspace.md",
        "CancelExternalTrigger" => "src/tool/tool_descriptions/cancel_external_trigger.md",
        "CancelTimer" => "src/tool/tool_descriptions/cancel_timer.md",
        "CompleteWorkItem" => "src/tool/tool_descriptions/complete_work_item.md",
        "CreateExternalTrigger" => "src/tool/tool_descriptions/create_external_trigger.md",
        "CreateTimer" => "src/tool/tool_descriptions/create_timer.md",
        "CreateWorkItem" => "src/tool/tool_descriptions/create_work_item.md",
        "CreateWorktree" => "src/tool/tool_descriptions/create_worktree.md",
        "DetachWorkspace" => "src/tool/tool_descriptions/detach_workspace.md",
        "DeleteAgent" => "src/tool/tool_descriptions/delete_agent.md",
        "ListSubagents" | "ListSubagentCleanup" => "src/tool/tool_descriptions/list_subagents.md",
        "Enqueue" => "src/tool/tool_descriptions/enqueue.md",
        "ExecCommand" => "src/tool/tool_descriptions/exec_command.md",
        "ExecCommandBatch" => "src/tool/tool_descriptions/exec_command_batch.md",
        "GenerateImage" => "src/tool/tool_descriptions/generate_image.md",
        "GetTimer" => "src/tool/tool_descriptions/get_timer.md",
        "GetWorkItem" => "src/tool/tool_descriptions/get_work_item.md",
        "GetWorkspaceState" => "src/tool/tool_descriptions/get_workspace_state.md",
        "AdvisoryDecision" => "src/tool/tool_descriptions/advisory_decision.md",
        "ListModelProviders" => "src/tool/tool_descriptions/list_model_providers.md",
        "ListProviderModels" => "src/tool/tool_descriptions/list_provider_models.md",
        "ListTasks" => "src/tool/tool_descriptions/list_tasks.md",
        "ListTimers" => "src/tool/tool_descriptions/list_timers.md",
        "ListWebFetchProviders" => "src/tool/tool_descriptions/list_web_fetch_providers.md",
        "ListWebSearchProviders" => "src/tool/tool_descriptions/list_web_search_providers.md",
        "ListWorkItems" => "src/tool/tool_descriptions/list_work_items.md",
        "MemoryGet" => "src/tool/tool_descriptions/memory_get.md",
        "MemorySearch" => "src/tool/tool_descriptions/memory_search.md",
        "PickWorkItem" => "src/tool/tool_descriptions/pick_work_item.md",
        "RemoveWorktree" => "src/tool/tool_descriptions/remove_worktree.md",
        "ScheduleDestructiveOperation" => {
            "src/tool/tool_descriptions/schedule_destructive_operation.md"
        }
        "SendAgentMessage" => "src/tool/tool_descriptions/send_agent_message.md",
        "Sleep" => "src/tool/tool_descriptions/sleep.md",
        "CreateAgent" => "src/tool/tool_descriptions/create_agent.md",
        "InvokeAgent" => "src/tool/tool_descriptions/invoke_agent.md",
        "TaskInput" => "src/tool/tool_descriptions/task_input.md",
        "TaskList" => "src/tool/tool_descriptions/task_list_legacy.md",
        "TaskStatus" => "src/tool/tool_descriptions/task_status.md",
        "TaskStop" => "src/tool/tool_descriptions/task_stop.md",
        "SwitchWorkspace" => "src/tool/tool_descriptions/switch_workspace.md",
        "UpdateWorkItem" => "src/tool/tool_descriptions/update_work_item.md",
        "UseWorkspace" => "src/tool/tool_descriptions/use_workspace.md",
        "ViewImage" => "src/tool/tool_descriptions/view_image.md",
        "WaitFor" => "src/tool/tool_descriptions/wait_for.md",
        "WebFetch" => "src/tool/tool_descriptions/web_fetch.md",
        "WebSearch" => "src/tool/tool_descriptions/web_search.md",
        "XSearch" => "src/tool/tool_descriptions/x_search.md",
        _ => return None,
    })
}

#[test]
fn built_in_tool_source_schemas_remain_valid() {
    let registry = holon_tools::dispatch::ToolRegistry::new(std::path::PathBuf::from("."));
    for spec in registry.tool_specs().unwrap() {
        holon_tools::schema::validate_source_tool_schema(&spec.input_schema)
            .unwrap_or_else(|error| panic!("{} source schema invalid: {error}", spec.name));
    }
}
