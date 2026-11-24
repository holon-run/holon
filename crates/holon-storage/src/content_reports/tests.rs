use super::*;

#[test]
fn snapshot_only_accepts_user_visible_text_bodies() {
    let text = serde_json::json!({"body": {"type": "text", "text": "hello"}});
    let brief = serde_json::json!({"body": {"type": "brief", "text": "hello"}});
    let blocks = serde_json::json!({
        "visibility": "operator_visible",
        "blocks": [
            {"type": "thinking", "text": "private"},
            {"type": "text", "text": "hello"},
            {"type": "tool_use", "input": {"secret": "private"}}
        ]
    });
    let private = serde_json::json!({
        "visibility": "runtime_private",
        "blocks": [{"type": "text", "text": "checkpoint"}]
    });
    let json = serde_json::json!({"body": {"type": "json", "value": "secret"}});
    assert_eq!(reportable_snapshot(&text).as_deref(), Some("hello"));
    assert_eq!(reportable_snapshot(&brief).as_deref(), Some("hello"));
    assert_eq!(reportable_snapshot(&blocks).as_deref(), Some("hello"));
    assert_eq!(reportable_snapshot(&private), None);
    assert_eq!(reportable_snapshot(&json), None);
}

#[test]
fn snapshot_is_bounded_by_unicode_scalars() {
    let input = "🙂".repeat(MAX_SNAPSHOT_CHARS + 1);
    let (snapshot, truncated) = bounded_snapshot(&input);
    assert_eq!(snapshot.chars().count(), MAX_SNAPSHOT_CHARS);
    assert!(truncated);
}
