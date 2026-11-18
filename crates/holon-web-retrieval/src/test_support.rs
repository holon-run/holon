pub(super) fn test_tool_error(error: &anyhow::Error) -> holon_domain::ToolError {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<holon_domain::ToolError>())
        .cloned()
        .expect("web retrieval error should carry a ToolError")
}
