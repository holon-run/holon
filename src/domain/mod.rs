//! Canonical runtime domain records.

pub mod scheduler_protocol;
pub mod scheduler_semantic;
pub mod work_item;
pub mod workspace;

pub use work_item::*;
pub use workspace::{agent_home_workspace_id, AGENT_HOME_WORKSPACE_ID};
