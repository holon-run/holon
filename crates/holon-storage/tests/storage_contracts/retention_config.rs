use holon_domain::MAX_EVENT_STREAM_WINDOW;
use holon_storage::retention_config::RuntimeDbRetentionPolicy;

#[test]
fn policy_defaults_to_disabled_and_rejects_small_audit_floor() {
    assert!(!RuntimeDbRetentionPolicy::default().enabled);
    let error = RuntimeDbRetentionPolicy {
        audit_events_min_rows_per_scope: MAX_EVENT_STREAM_WINDOW - 1,
        ..RuntimeDbRetentionPolicy::default()
    }
    .validate()
    .expect_err("audit floor below replay window must fail");
    assert!(error.to_string().contains("must be at least"));
}
