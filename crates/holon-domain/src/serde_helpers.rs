use crate::workspace_identity::AGENT_HOME_WORKSPACE_ID;

pub(super) fn default_agent_home_workspace_id() -> String {
    AGENT_HOME_WORKSPACE_ID.to_string()
}

pub(super) fn is_false(value: &bool) -> bool {
    !*value
}
