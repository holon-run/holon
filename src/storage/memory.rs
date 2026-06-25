//! Memory index storage helpers.

pub(crate) fn memory_index_agent_key(agent_id: &str) -> String {
    agent_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
