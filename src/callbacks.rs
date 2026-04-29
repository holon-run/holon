use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::types::CallbackDeliveryMode;

pub(crate) fn generate_callback_token() -> String {
    format!("cb_{}", Uuid::new_v4().simple())
}

pub(crate) fn hash_callback_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

pub(crate) fn build_callback_url(
    base_url: &str,
    delivery_mode: &CallbackDeliveryMode,
    token: &str,
) -> String {
    let mode = match delivery_mode {
        CallbackDeliveryMode::EnqueueMessage => "enqueue",
        CallbackDeliveryMode::WakeOnly => "wake",
    };
    format!(
        "{}/callbacks/{}/{}",
        base_url.trim_end_matches('/'),
        mode,
        token
    )
}
