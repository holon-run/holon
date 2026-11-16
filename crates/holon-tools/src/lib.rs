//! Built-in tools over application-provided runtime capabilities.

use holon_domain as types;

mod tool;
pub use spec::{ToolCall, ToolResult, ToolSpec};
pub use tool::apply_patch as patch;
pub use tool::model_tool_schema_inventory;
pub use tool::{dispatch, helpers, names, schema, spec, summary, tools as builtins, ToolError};
