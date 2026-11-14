//! Canonical workspace identity helpers.

pub const AGENT_HOME_WORKSPACE_ID: &str = "agent_home";

pub fn agent_home_workspace_id(agent_id: &str) -> String {
    format!("{AGENT_HOME_WORKSPACE_ID}:{agent_id}")
}
