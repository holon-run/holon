use super::*;

#[test]
fn http_error_envelope_always_serializes_machine_code() {
    let value = serde_json::to_value(HttpErrorEnvelope::new(
        "invalid_request",
        "request is invalid",
    ))
    .expect("serialize HTTP error envelope");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "request is invalid");
    assert_eq!(value["code"], "invalid_request");
}
