use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

const SHORT_RANDOM_HEX_LEN: usize = 15; // 15 random hex nibbles ~= 60 bits
const UUID_VERSION_NIBBLE_INDEX: usize = 12;

fn short_random_hex() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .enumerate()
        .filter_map(|(index, ch)| (index != UUID_VERSION_NIBBLE_INDEX).then_some(ch))
        .take(SHORT_RANDOM_HEX_LEN)
        .collect()
}

pub(crate) fn runtime_id(prefix: &str) -> String {
    format!("{prefix}_{}", short_random_hex())
}

pub fn message_id() -> String {
    runtime_id("msg")
}

pub fn task_id() -> String {
    runtime_id("task")
}

pub fn run_id() -> String {
    runtime_id("run")
}

pub fn turn_id() -> String {
    runtime_id("turn")
}

pub fn tool_execution_id() -> String {
    runtime_id("tool")
}

pub fn workspace_id() -> String {
    runtime_id("ws")
}

/// Derive a workspace ID deterministically from a (normalized) anchor path.
/// Same path always produces the same ID, preventing stale-ID accumulation.
pub fn deterministic_workspace_id(anchor: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(anchor.to_string_lossy().as_bytes());
    let result = hasher.finalize();
    let hex: String = result.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("ws_{}", &hex[..SHORT_RANDOM_HEX_LEN])
}

pub fn work_item_id() -> String {
    runtime_id("work")
}

pub fn brief_id() -> String {
    runtime_id("brief")
}

pub fn transcript_entry_id() -> String {
    runtime_id("tr")
}

pub fn episode_id() -> String {
    runtime_id("ep")
}

pub fn wait_condition_id() -> String {
    runtime_id("wait")
}

pub fn timer_id() -> String {
    runtime_id("timer")
}

pub fn delivery_summary_id() -> String {
    runtime_id("deliv")
}

pub fn external_trigger_id() -> String {
    runtime_id("trigger")
}

pub fn operator_notification_id() -> String {
    runtime_id("notify")
}

pub fn operator_delivery_intent_id() -> String {
    runtime_id("odi")
}

pub fn workspace_occupancy_id() -> String {
    runtime_id("occ")
}

pub fn audit_event_id() -> String {
    runtime_id("event")
}

pub fn work_item_delegation_id() -> String {
    runtime_id("delegation")
}

pub fn work_item_continuation_id() -> String {
    runtime_id("wic")
}

pub fn capability_id(prefix: &str) -> String {
    format!(
        "{prefix}_{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_runtime_id(value: &str, prefix: &str) {
        let (actual_prefix, random) = value.split_once('_').expect("id should contain prefix");
        assert_eq!(actual_prefix, prefix);
        assert_eq!(random.len(), SHORT_RANDOM_HEX_LEN);
        assert!(random.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn ordinary_runtime_ids_use_compact_prefixed_shape() {
        for (id, prefix) in [
            (message_id(), "msg"),
            (task_id(), "task"),
            (run_id(), "run"),
            (turn_id(), "turn"),
            (tool_execution_id(), "tool"),
            (workspace_id(), "ws"),
            (work_item_id(), "work"),
            (brief_id(), "brief"),
            (transcript_entry_id(), "tr"),
            (episode_id(), "ep"),
            (wait_condition_id(), "wait"),
            (timer_id(), "timer"),
            (delivery_summary_id(), "deliv"),
            (external_trigger_id(), "trigger"),
            (operator_notification_id(), "notify"),
            (operator_delivery_intent_id(), "odi"),
            (workspace_occupancy_id(), "occ"),
            (audit_event_id(), "event"),
            (work_item_delegation_id(), "delegation"),
            (work_item_continuation_id(), "wic"),
        ] {
            assert_runtime_id(&id, prefix);
        }
    }

    #[test]
    fn capability_ids_are_not_shortened_to_runtime_id_entropy() {
        let id = capability_id("cb");
        let (prefix, random) = id.split_once('_').expect("id should contain prefix");
        assert_eq!(prefix, "cb");
        assert!(random.len() >= 64);
        assert!(random.chars().all(|ch| ch.is_ascii_hexdigit()));
    }
}
