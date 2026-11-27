use super::*;
use serde_json::json;

fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: String::new(),
        input_schema: json!({}),
        freeform_grammar: None,
    }
}

#[test]
fn test_sleep_and_retired_sections_are_not_emitted() {
    let sections = tool_sections(&[spec("ExecCommand"), spec("TaskOutput")]);
    for retired in [
        "tool_sleep",
        "tool_read",
        "tool_glob",
        "tool_grep",
        "tool_task_control",
    ] {
        assert!(
            !sections.iter().any(|s| s.name == retired),
            "unexpected section {retired}"
        );
    }
}

#[test]
fn test_agent_invocation_section_gating_and_core_routing_invariants() {
    for name in [tn::CREATE_AGENT, tn::INVOKE_AGENT, tn::SEND_AGENT_MESSAGE] {
        let sections = tool_sections(&[spec(name)]);
        let content = &sections
            .iter()
            .find(|section| section.name == "tool_agent_invocation")
            .expect("agent invocation section")
            .content;
        assert!(content.contains("actual delegator"));
        assert!(content.contains("lineage parent"));
        assert!(content.contains("WaitFor"));
        assert!(!content.contains("TaskOutput"));
    }

    let send_only = &tool_sections(&[spec("SendAgentMessage")])[0].content;
    assert!(send_only.contains("reply_context_id"));
    assert!(send_only.contains("forward=true"));
    assert!(!send_only.contains("new_subagent"));
    assert!(!send_only.contains("CreateAgent"));
    assert!(!send_only.contains("holon tail"));

    let create_only = &tool_sections(&[spec("CreateAgent")])[0].content;
    assert!(create_only.contains("CreateAgent"));
    assert!(create_only.contains("real parallelism"));
    assert!(!create_only.contains("new_subagent"));
    assert!(!create_only.contains("reply_context_id"));

    let invoke_content = &tool_sections(&[spec("InvokeAgent")])[0].content;
    assert!(invoke_content.contains("existing_agent"));
    assert!(invoke_content.contains("new_subagent"));
    assert!(invoke_content.contains("SendAgentMessage"));
    assert!(!invoke_content.contains("reply_context_id"));

    let invoke_with_cli = &tool_sections(&[spec("InvokeAgent"), spec("ExecCommand")])
        .into_iter()
        .find(|s| s.name == "tool_agent_invocation")
        .expect("agent invocation with cli")
        .content;
    assert!(invoke_with_cli.contains("holon docs agents"));

    assert!(!tool_sections(&[])
        .iter()
        .any(|section| section.name == "tool_agent_invocation"));
}

#[test]
fn test_get_agent_section_preserves_readonly_and_task_status_boundary() {
    let sections = tool_sections(&[spec("GetAgent")]);
    let section = sections
        .iter()
        .find(|s| s.name == "tool_get_agent")
        .expect("agent get section");
    assert!(section.content.contains("read-only"));
    assert!(section.content.contains("TaskStatus"));
    assert!(!section.content.contains("TaskOutput"));
}

#[test]
fn test_enqueue_section_preserves_receipt_and_authority_cap() {
    let sections = tool_sections(&[spec("Enqueue")]);
    let section = sections
        .iter()
        .find(|s| s.name == "tool_enqueue")
        .expect("enqueue section");
    assert!(section.content.contains("receipt"));
    assert!(section
        .content
        .contains("never inherits operator authority"));
}

#[test]
fn test_task_control_section_gating_and_non_polling_invariants() {
    for trigger in ["ListTasks", "TaskStatus", "TaskInput", "TaskStop"] {
        let sections = tool_sections(&[spec(trigger)]);
        let section = sections
            .iter()
            .find(|s| s.name == "tool_task_control")
            .unwrap_or_else(|| panic!("task control section for {trigger}"));
        assert!(section.content.contains("TaskStatus"));
        assert!(section.content.contains("WaitFor"));
        assert!(section.content.contains("accepts_input"));
        assert!(!section.content.contains("TaskOutput"));
        assert!(!section.content.contains("holon task output"));
    }
}

#[test]
fn test_exec_command_and_holon_cli_invariants() {
    let sections = tool_sections(&[spec("ExecCommand")]);
    let exec = sections
        .iter()
        .find(|s| s.name == "tool_exec_command")
        .expect("exec command section");
    assert!(exec.content.contains("rg -n"));
    assert!(exec.content.contains("reuse_running"));
    assert!(!exec.content.contains("disposition=promoted_to_task"));

    let cli = sections
        .iter()
        .find(|s| s.name == "holon_cli_contract")
        .expect("holon cli contract section");
    assert!(cli.content.contains("not authentication"));
    assert!(cli.content.contains("holon commands"));
    assert!(cli.content.contains("holon context"));
    assert!(cli.content.contains("holon docs"));
    assert!(cli.content.contains("recursive `holon run`"));
    assert!(cli.content.contains("DeleteAgent(agent_id, incarnation)"));
    assert!(!cli.content.contains("remove only that owned child"));
}

#[test]
fn test_exec_command_batch_non_promotion_invariant() {
    let sections = tool_sections(&[spec("ExecCommandBatch")]);
    let batch = sections
        .iter()
        .find(|s| s.name == "tool_exec_command_batch")
        .expect("exec command batch section");
    assert!(batch.content.contains("does not promote"));
    assert!(batch.content.contains("ExecCommand"));
    assert!(!batch.content.contains("shell-level `&`"));
}

#[test]
fn test_work_item_sections_preserve_lifecycle_invariants() {
    let write_sections = tool_sections(&[spec("CreateWorkItem")]);
    let write = write_sections
        .iter()
        .find(|s| s.name == "tool_work_item_write")
        .expect("work item write section");
    assert!(write.content.contains("plan_artifact.path"));
    assert!(write.content.contains("plan_status"));
    assert!(write.content.contains("WaitFor"));
    assert!(write.content.contains("CompleteWorkItem"));
    assert!(!write.content.contains("complete it first"));
    assert!(!write.content.contains("result_summary"));

    let sched = write_sections
        .iter()
        .find(|s| s.name == "tool_work_item_scheduling")
        .expect("work item scheduling section");
    assert!(sched.content.contains("yielded"));
    assert!(sched.content.contains("continuation frame"));
    assert!(
        !write.content.contains("PickWorkItem") && !sched.content.contains("PickWorkItem"),
        "PickWorkItem guidance must be gated out when PickWorkItem is absent"
    );

    let with_pick = tool_sections(&[spec("CreateWorkItem"), spec("PickWorkItem")]);
    assert!(with_pick
        .iter()
        .find(|s| s.name == "tool_work_item_write")
        .unwrap()
        .content
        .contains("PickWorkItem"));
    assert!(with_pick
        .iter()
        .find(|s| s.name == "tool_work_item_scheduling")
        .unwrap()
        .content
        .contains("PickWorkItem"));

    let read_sections = tool_sections(&[spec("GetWorkItem"), spec("ListWorkItems")]);
    assert_eq!(
        read_sections
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["tool_work_item_read"],
        "read-only WorkItem surfaces must not emit write or scheduling transition sections"
    );
    let read = &read_sections[0];
    assert!(read.content.contains("ListWorkItems"));
    assert!(read.content.contains("GetWorkItem"));
    for write_tool in ["PickWorkItem", "UpdateWorkItem", "CompleteWorkItem"] {
        assert!(
            !read.content.contains(write_tool),
            "read-only WorkItem guidance must not recommend absent write tool {write_tool}"
        );
    }
}

#[test]
fn test_wait_for_and_timer_sections_preserve_ownership_and_wake_invariants() {
    let wait_sections = tool_sections(&[spec("WaitFor")]);
    let wait = wait_sections
        .iter()
        .find(|s| s.name == "tool_wait_for")
        .expect("wait for section");
    for required in [
        "wake=task_result",
        "wake=timer",
        "wake=system",
        "wake=external",
        "wake=operator_input",
        "delivery=final",
        "delivery=silent",
        "execution-bound WorkItem first",
        "any WorkItem of that agent may hold the wait",
        "continue_turn",
        "yield_and_wait",
        "yield_and_reenter",
    ] {
        assert!(wait.content.contains(required), "missing {required}");
    }
    assert!(!wait.content.contains("To wait on another WorkItem"));

    let timer_sections = tool_sections(&[spec("CreateTimer")]);
    let timer = timer_sections
        .iter()
        .find(|s| s.name == "tool_timer")
        .expect("timer section");
    assert!(timer.content.contains("does not pause the current turn"));
    assert!(timer.content.contains("WaitFor"));
    assert!(timer.content.contains("CancelTimer"));
}

#[test]
fn test_external_trigger_section_prefers_canonical_names() {
    let sections = tool_sections(&[spec("CreateExternalTrigger")]);
    let section = sections
        .iter()
        .find(|s| s.name == "tool_external_trigger")
        .expect("external trigger section");
    assert!(section.content.contains("CreateExternalTrigger"));
    assert!(section.content.contains("CancelExternalTrigger"));
}

#[test]
fn test_apply_patch_surface_exclusivity_between_json_and_codex_dsl() {
    let json_sections = tool_sections(&[spec("ApplyPatch")]);
    assert!(
        !json_sections.iter().any(|s| s.name == "tool_file_mutation"),
        "file mutation guidance must be consolidated into tool_apply_patch"
    );
    let json_patch = json_sections
        .iter()
        .find(|s| s.name == "tool_apply_patch")
        .expect("json apply patch section");
    assert!(json_patch
        .content
        .contains("Current ApplyPatch surface is a JSON/function tool"));
    assert!(json_patch.content.contains("--- a/relative/path"));
    assert!(!json_patch
        .content
        .contains("Current ApplyPatch surface is Codex DSL freeform"));
    assert!(!json_patch.content.contains("*** Begin Patch"));

    let dsl_sections = tool_sections(&[ToolSpec {
        name: "ApplyPatch".into(),
        description: String::new(),
        input_schema: json!({}),
        freeform_grammar: Some(holon_domain::ToolFreeformGrammar {
            syntax: "lark".into(),
            definition: "start: /.+/".into(),
        }),
    }]);
    assert!(!dsl_sections.iter().any(|s| s.name == "tool_file_mutation"));
    let dsl_patch = dsl_sections
        .iter()
        .find(|s| s.name == "tool_apply_patch")
        .expect("dsl apply patch section");
    assert!(dsl_patch
        .content
        .contains("Current ApplyPatch surface is Codex DSL freeform"));
    assert!(dsl_patch.content.contains("*** Begin Patch"));
    assert!(!dsl_patch.content.contains("--- a/relative/path"));
    assert!(!dsl_patch
        .content
        .contains("Current ApplyPatch surface is a JSON/function tool"));
}

#[test]
fn test_workspace_section_preserves_root_and_shell_cd_invariants() {
    let sections = tool_sections(&[spec("SwitchWorkspace")]);
    let section = sections
        .iter()
        .find(|s| s.name == "tool_workspace")
        .expect("workspace section");
    assert!(section.content.contains("active workspace"));
    assert!(section.content.contains("agent_home"));
    assert!(section.content.contains("Shell `cd`"));
    assert!(!section.content.contains("workspace://"));
}

#[test]
fn test_all_registered_tool_families_emit_non_empty_sections() {
    let tools = [
        "WaitFor",
        "CreateTimer",
        "CreateAgent",
        "GetAgent",
        "Enqueue",
        "CreateExternalTrigger",
        "CreateWorkItem",
        "GetWorkItem",
        "TaskStatus",
        "ExecCommand",
        "ExecCommandBatch",
        "ApplyPatch",
        "SwitchWorkspace",
    ]
    .into_iter()
    .map(spec)
    .collect::<Vec<_>>();

    let sections = tool_sections(&tools);
    assert!(!sections.is_empty());
    for section in &sections {
        assert!(
            !section.content.trim().is_empty(),
            "empty content for {}",
            section.name
        );
    }
    assert!(tool_sections(&[]).is_empty());
}

#[test]
fn test_guidance_normalizes_line_endings() {
    assert_eq!(guidance("one\r\ntwo\r\n"), "one\ntwo");
    assert_eq!(guidance("one\ntwo\n\n"), "one\ntwo");
}
