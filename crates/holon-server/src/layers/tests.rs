use super::redact_request_path;

#[test]
fn callback_paths_hide_capabilities() {
    assert_eq!(
        redact_request_path("/api/callbacks/wake/opaque"),
        "/api/callbacks/wake/<redacted>"
    );
    assert_eq!(
        redact_request_path("/api/callbacks/enqueue/opaque"),
        "/api/callbacks/enqueue/<redacted>"
    );
    assert_eq!(redact_request_path("/api/agents"), "/api/agents");
}
