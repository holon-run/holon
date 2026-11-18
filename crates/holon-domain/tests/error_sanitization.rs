#![cfg(feature = "error-sanitization")]

use holon_domain::sanitize_runtime_error_text;

#[test]
fn sanitizer_redacts_json_secrets_and_structured_bodies() {
    assert_eq!(
        sanitize_runtime_error_text(r#"provider failed: {"api_key":"secret"}"#),
        "<redacted-sensitive-error-context>"
    );
    assert_eq!(
        sanitize_runtime_error_text(r#"provider failed: {"access_token":"secret"}"#),
        "<redacted-sensitive-error-context>"
    );
    assert_eq!(
        sanitize_runtime_error_text("provider failed: x-api-key: short-secret"),
        "<redacted-sensitive-error-context>"
    );
    assert_eq!(
        sanitize_runtime_error_text(r#"provider failed: {"detail":"raw backend response body"}"#),
        "provider failed: <redacted-structured-error-context>"
    );
    assert_eq!(
        sanitize_runtime_error_text(
            "callback failed for https://example.test/api/callbacks/wake/cb_secret"
        ),
        "<redacted-sensitive-error-context>"
    );
    assert_eq!(
        sanitize_runtime_error_text(
            "callback failed for https://example.test/callbacks/enqueue/cb_secret"
        ),
        "<redacted-sensitive-error-context>"
    );
}
