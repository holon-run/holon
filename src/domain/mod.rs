//! Canonical runtime domain records.

pub mod execution_protocol;
pub mod scheduler;
pub mod scheduler_protocol;
pub mod work_item;
pub mod workspace;

pub use work_item::*;
pub use workspace::{agent_home_workspace_id, AGENT_HOME_WORKSPACE_ID};
