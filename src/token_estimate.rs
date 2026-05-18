use serde_json::Value;

pub(crate) fn estimate_text_tokens(text: &str) -> usize {
    let bytes = text.len();
    bytes.saturating_add(3) / 4
}

pub(crate) fn estimate_json_tokens(value: &Value) -> usize {
    match serde_json::to_string(value) {
        Ok(json) => estimate_text_tokens(&json),
        Err(_) => 1,
    }
}
