use serde_json::json;

use super::tool_result_summary;
use crate::tool::{ToolError, ToolResult};

#[test]
fn result_summary_falls_back_to_completed_instead_of_the_tool_name() {
    let result = ToolResult::success(
        "GetAgent",
        json!({
            "profile": {"name": "default"},
            "active_tasks": [{"id": "task-1"}]
        }),
        None,
    );
    assert_eq!(tool_result_summary(&result.envelope), "completed");

    let error = ToolResult::error("ExecCommand", ToolError::new("failure", "command exploded"));
    assert_eq!(tool_result_summary(&error.envelope), "command exploded");
}

#[test]
fn result_summary_uses_tool_specific_result_fields() {
    let result = ToolResult::success(
        "ExecCommand",
        json!({
            "disposition": "completed",
            "exit_status": 2,
            "truncated": false
        }),
        None,
    );
    assert_eq!(
        tool_result_summary(&result.envelope),
        "completed exit_status=2 truncated=false"
    );
}

#[test]
fn view_image_result_summary_trims_observation_text() {
    let result = ToolResult::success(
        "ViewImage",
        json!({
            "visual_reference": {
                "id": "image-1",
                "mime": "image/png"
            },
            "observation": {
                "schema": "visual_observation.v1",
                "prompt": "  inspect this  ",
                "summary": "  a diagram  ",
                "ocr": [{"text": "  label  "}]
            }
        }),
        None,
    );

    assert_eq!(
        tool_result_summary(&result.envelope),
        concat!(
            "visual_observation schema=visual_observation.v1 ",
            "ref=image-1 mime=image/png prompt=\"inspect this\" ",
            "summary=\"a diagram\" ocr=[label]"
        )
    );
}
