use super::*;
use axum::http::header;

#[test]
fn if_none_match_satisfied_uses_weak_comparison() {
    let headers = |value: &str| {
        let mut map = HeaderMap::new();
        map.insert(header::IF_NONE_MATCH, HeaderValue::from_str(value).unwrap());
        map
    };
    // Strong echo and `*` both satisfy.
    assert!(if_none_match_satisfied(&headers("\"abc\""), "\"abc\""));
    assert!(if_none_match_satisfied(&headers("*"), "\"abc\""));
    // RFC 9110: If-None-Match compares weakly, so W/"abc" matches "abc".
    assert!(if_none_match_satisfied(&headers("W/\"abc\""), "\"abc\""));
    assert!(if_none_match_satisfied(
        &headers("\"x\", W/\"abc\""),
        "\"abc\""
    ));
    // Different opaque tags never satisfy, and neither does a missing header.
    assert!(!if_none_match_satisfied(&headers("\"stale\""), "\"abc\""));
    assert!(!if_none_match_satisfied(&headers("W/\"stale\""), "\"abc\""));
    assert!(!if_none_match_satisfied(&HeaderMap::new(), "\"abc\""));
}
