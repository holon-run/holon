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

pub fn completion_report_request_id() -> String {
    runtime_id("completion")
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

pub(crate) fn event_log_epoch_id() -> String {
    runtime_id("epoch")
}

pub(crate) fn runtime_installation_id() -> String {
    runtime_id("runtime")
}

/// Derives the opaque `visibility_scope_id` from stable server-side facts:
/// the runtime installation identity, the server-resolved authority
/// principal, the normalized visibility entitlement, and the visibility
/// policy generation. Credential material is never an input: rotating a
/// credential with unchanged entitlement keeps the scope stable, while a
/// principal, entitlement, or policy change rotates it.
pub(crate) fn visibility_scope_id(
    runtime_id: &str,
    authority_principal: &str,
    authority_entitlement: &str,
    policy_generation: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"holon.visibility-scope.v1\x00");
    hasher.update(runtime_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(authority_principal.as_bytes());
    hasher.update(b"\x00");
    hasher.update(authority_entitlement.as_bytes());
    hasher.update(b"\x00");
    hasher.update(policy_generation.to_string().as_bytes());
    let digest = hasher.finalize();
    format!("vscope1_{}", &hex(&digest[..16]))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn work_item_delegation_id() -> String {
    runtime_id("delegation")
}

pub fn work_item_continuation_id() -> String {
    runtime_id("wic")
}

pub fn agent_deletion_id() -> String {
    runtime_id("agentdel")
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
