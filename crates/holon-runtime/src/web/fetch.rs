use crate::tool::ToolError;

pub use holon_web_retrieval::fetch::*;

pub fn error_result(tool_name: &str, error: ToolError) -> crate::tool::ToolResult {
    crate::tool::ToolResult::error(tool_name, error)
}
