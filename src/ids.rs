use uuid::Uuid;

const SHORT_RANDOM_HEX_LEN: usize = 15; // 60 bits

fn short_random_hex() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(SHORT_RANDOM_HEX_LEN)
        .collect()
}

fn runtime_id(prefix: &str) -> String {
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

pub fn tool_execution_id() -> String {
    runtime_id("tool")
}

pub fn workspace_id() -> String {
    runtime_id("ws")
}

pub fn work_item_id() -> String {
    runtime_id("work")
}

pub fn brief_id() -> String {
    runtime_id("brief")
}

pub fn transcript_entry_id() -> String {
    runtime_id("transcript")
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
    runtime_id("delivery")
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
            (tool_execution_id(), "tool"),
            (workspace_id(), "ws"),
            (work_item_id(), "work"),
            (brief_id(), "brief"),
            (transcript_entry_id(), "transcript"),
            (episode_id(), "ep"),
            (wait_condition_id(), "wait"),
            (timer_id(), "timer"),
            (delivery_summary_id(), "delivery"),
            (external_trigger_id(), "trigger"),
            (operator_notification_id(), "notify"),
            (operator_delivery_intent_id(), "odi"),
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
