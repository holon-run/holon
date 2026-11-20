//! Application bindings for the built-in tool crate
//!
//! This module provides a clear separation of concerns for tool functionality:
//! - `spec`: Tool schema definitions (ToolSpec, ToolCall, ToolResult)
//! - `dispatch`: Tool routing and registry (ToolRegistry)
//! - `helpers`: Shared utility functions
//! - `host`: Runtime operations used by the built-in tools

pub(crate) mod apply_patch;
pub mod dispatch;
pub mod error;
pub(crate) mod helpers;
pub mod names;
pub mod spec;
pub(crate) mod summary;
#[path = "host/mod.rs"]
pub(crate) mod tools;

// Re-export the key types that are used throughout the codebase
pub use apply_patch::ApplyPatchSurface;
pub use dispatch::ToolRegistry;
pub use error::{tool_error_from_anyhow, ToolError};
pub use spec::{ToolCall, ToolResult, ToolSpec};

pub use holon_tools::model_tool_schema_inventory;
