use holon_tools::builtins::memory_get::{validate_max_chars, MAX_CHARS_MAX};
use holon_tools::ToolError;

#[test]
fn max_chars_accepts_omitted_and_bounded_values() {
    assert_eq!(validate_max_chars(None).unwrap(), None);
    assert_eq!(validate_max_chars(Some(1)).unwrap(), Some(1));
    assert_eq!(
        validate_max_chars(Some(MAX_CHARS_MAX)).unwrap(),
        Some(MAX_CHARS_MAX)
    );
}

#[test]
fn max_chars_rejects_zero_and_oversized_values() {
    for max_chars in [0, MAX_CHARS_MAX + 1] {
        let error = validate_max_chars(Some(max_chars)).unwrap_err();
        let error = error.downcast_ref::<ToolError>().unwrap();
        assert_eq!(error.kind, "invalid_tool_input");
        assert!(error
            .details
            .as_ref()
            .and_then(|details| details.get("maximum"))
            .is_some());
    }
}
