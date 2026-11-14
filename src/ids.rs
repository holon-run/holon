pub use holon_domain::ids::*;

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
            (agent_deletion_id(), "agentdel"),
            (audit_event_id(), "event"),
            (work_item_delegation_id(), "delegation"),
            (work_item_continuation_id(), "wic"),
        ] {
            assert_runtime_id(&id, prefix);
        }
    }

    #[test]
    fn interaction_ids_are_stable_and_scope_sensitive() {
        let first = interaction_id(&["agent", "control"]);
        assert_eq!(first, interaction_id(&["agent", "control"]));
        assert_ne!(first, interaction_id(&["agent", "cli"]));
        assert!(first.starts_with("interaction1_"));
    }

    #[test]
    fn capability_ids_are_not_shortened_to_runtime_id_entropy() {
        let id = capability_id("cb");
        let (prefix, random) = id.split_once('_').expect("id should contain prefix");
        assert_eq!(prefix, "cb");
        assert!(random.len() >= 64);
        assert!(random.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn deterministic_workspace_id_is_stable_for_same_path() {
        let path = Path::new("/home/user/project");
        let id1 = deterministic_workspace_id(path);
        let id2 = deterministic_workspace_id(path);
        assert_eq!(id1, id2, "same path must produce same workspace ID");
    }

    #[test]
    fn deterministic_workspace_id_differs_for_different_paths() {
        let id1 = deterministic_workspace_id(Path::new("/home/user/project-a"));
        let id2 = deterministic_workspace_id(Path::new("/home/user/project-b"));
        assert_ne!(id1, id2, "different paths must produce different IDs");
    }

    #[test]
    fn deterministic_workspace_id_uses_ws_prefix_and_hex_shape() {
        let id = deterministic_workspace_id(Path::new("/tmp/test"));
        let (prefix, hex) = id.split_once('_').expect("id should contain prefix");
        assert_eq!(prefix, "ws");
        assert_eq!(
            hex.len(),
            SHORT_RANDOM_HEX_LEN,
            "hex portion should be {SHORT_RANDOM_HEX_LEN} characters"
        );
        assert!(
            hex.chars().all(|ch| ch.is_ascii_hexdigit()),
            "hex portion should only contain hex digits"
        );
    }

    #[test]
    fn visibility_scope_id_is_stable_and_rotates_only_on_contract_inputs() {
        let scope = visibility_scope_id("runtime_fixture", "control", "control", 0);
        assert!(scope.starts_with("vscope1_"));
        assert_eq!(scope.len(), "vscope1_".len() + 32);
        // Deterministic: credential rotation with unchanged principal and
        // entitlement has no input at all, so the scope cannot move.
        assert_eq!(
            scope,
            visibility_scope_id("runtime_fixture", "control", "control", 0)
        );
        // Principal, entitlement, policy generation, and runtime identity
        // changes each rotate the scope.
        assert_ne!(
            scope,
            visibility_scope_id("runtime_fixture", "public", "control", 0)
        );
        assert_ne!(
            scope,
            visibility_scope_id("runtime_fixture", "control", "public", 0)
        );
        assert_ne!(
            scope,
            visibility_scope_id("runtime_fixture", "control", "control", 1)
        );
        assert_ne!(
            scope,
            visibility_scope_id("runtime_other", "control", "control", 0)
        );
    }
}
