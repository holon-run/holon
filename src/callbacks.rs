use crate::types::CallbackDeliveryMode;
use sha2::{Digest, Sha256};
use std::fmt::Write;

pub(crate) fn generate_callback_token() -> String {
    crate::ids::capability_id("cb")
}

pub(crate) fn hash_callback_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

pub(crate) fn callback_token_hash_matches(token: &str, expected: &str) -> bool {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    if expected.len() != 64 {
        return false;
    }
    Sha256::digest(token.as_bytes())
        .iter()
        .zip(expected.as_bytes().chunks_exact(2))
        .all(|(byte, pair)| {
            pair[0] == HEX[usize::from(byte >> 4)] && pair[1] == HEX[usize::from(byte & 15)]
        })
}

pub(crate) fn build_callback_url(
    base_url: &str,
    delivery_mode: &CallbackDeliveryMode,
    token: &str,
) -> String {
    let mode = match delivery_mode {
        CallbackDeliveryMode::EnqueueMessage => "enqueue",
        CallbackDeliveryMode::WakeHint => "wake",
    };
    format!(
        "{}/api/callbacks/{}/{}",
        base_url.trim_end_matches('/'),
        mode,
        token
    )
}
