//! Web search and page retrieval from resolved provider configuration.

pub mod fetch;
pub mod policy;
mod remote_fetch;
pub mod search;

pub use holon_config::web_config::*;

#[cfg(test)]
fn test_tool_error(error: &anyhow::Error) -> holon_domain::ToolError {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<holon_domain::ToolError>())
        .cloned()
        .expect("web retrieval error should carry a ToolError")
}
