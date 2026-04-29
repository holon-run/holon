//! Tool layer implementation
//!
//! This module provides a clear separation of concerns for tool functionality:
//! - `spec`: Tool schema definitions (ToolSpec, ToolCall, ToolResult)
//! - `dispatch`: Tool routing and registry (ToolRegistry)
//! - `helpers`: Shared utility functions
//! - `tools`: Builtin tool modules, one per tool

mod apply_patch;
pub mod dispatch;
pub mod error;
pub(crate) mod helpers;
pub(crate) mod schema_support;
pub mod spec;
pub(crate) mod tools;

pub(crate) use schema_support as schema;

// Re-export the key types that are used throughout the codebase
pub use dispatch::ToolRegistry;
pub use error::ToolError;
pub use spec::{ToolCall, ToolResult, ToolSpec};
