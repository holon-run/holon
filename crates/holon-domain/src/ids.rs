use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

#[doc(hidden)]
pub const SHORT_RANDOM_HEX_LEN: usize = 15; // 15 random hex nibbles ~= 60 bits
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

pub fn runtime_id(prefix: &str) -> String {
    format!("{prefix}_{}", short_random_hex())
}

pub fn message_id() -> String {
    runtime_id("msg")
}

pub fn wait_recheck_message_id(wait_id: &str, recheck_at: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"holon.wait-recheck-message.v1\x00");
    hasher.update(wait_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(recheck_at.as_bytes());
    let digest = hasher.finalize();
    format!("msg_recheck1_{}", hex(&digest[..16]))
}

pub fn agent_message_delivery_id(idempotency_scope: &str, idempotency_key_digest: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"holon.agent-message-delivery.v1\x00");
    hasher.update(idempotency_scope.as_bytes());
    hasher.update(b"\x00");
    hasher.update(idempotency_key_digest.as_bytes());
    let digest = hasher.finalize();
    format!("delivery1_{}", hex(&digest[..16]))
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

pub fn interaction_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"holon.interaction.v1\x00");
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\x00");
    }
    let digest = hasher.finalize();
    format!("interaction1_{}", hex(&digest[..16]))
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

pub fn event_log_epoch_id() -> String {
    runtime_id("epoch")
}

pub fn runtime_installation_id() -> String {
    runtime_id("runtime")
}

/// Derives the opaque `visibility_scope_id` from stable server-side facts:
/// the runtime installation identity, the server-resolved authority
/// principal, the normalized visibility entitlement, and the visibility
/// policy generation. Credential material is never an input: rotating a
/// credential with unchanged entitlement keeps the scope stable, while a
/// principal, entitlement, or policy change rotates it.
pub fn visibility_scope_id(
    runtime_id: &str,
    authority_principal: &str,
    authority_entitlement: &str,
    policy_generation: u64,
) -> String {
    let scope = visibility_scope_id_bytes(
        runtime_id,
        authority_principal,
        authority_entitlement,
        policy_generation,
    );
    std::str::from_utf8(&scope)
        .expect("visibility scope encoding is ASCII")
        .to_owned()
}

/// Fixed-size encoding for synchronous verification without an owned String.
pub fn visibility_scope_id_bytes(
    runtime_id: &str,
    authority_principal: &str,
    authority_entitlement: &str,
    policy_generation: u64,
) -> [u8; 40] {
    let mut hasher = Sha256::new();
    hasher.update(b"holon.visibility-scope.v1\x00");
    hasher.update(runtime_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(authority_principal.as_bytes());
    hasher.update(b"\x00");
    hasher.update(authority_entitlement.as_bytes());
    hasher.update(b"\x00");
    let mut decimal = [0u8; 20];
    let mut start = decimal.len();
    let mut remaining = policy_generation;
    loop {
        start -= 1;
        decimal[start] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    hasher.update(&decimal[start..]);
    let digest = hasher.finalize();
    let mut scope = [0u8; 40];
    scope[..8].copy_from_slice(b"vscope1_");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (index, byte) in digest[..16].iter().enumerate() {
        scope[8 + index * 2] = HEX[(byte >> 4) as usize];
        scope[9 + index * 2] = HEX[(byte & 15) as usize];
    }
    scope
}

#[doc(hidden)]
pub fn hex(bytes: &[u8]) -> String {
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
