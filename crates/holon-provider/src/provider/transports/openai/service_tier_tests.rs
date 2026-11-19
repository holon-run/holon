use super::*;
use crate::config::ServiceTier;

#[test]
fn service_tier_preserves_legacy_requests_and_independent_reasoning() {
    let original = json!({"model":"gpt-6-astra","reasoning":{"effort":"high"}});
    let mut body = original.clone();
    apply_service_tier(&mut body, None, false);
    assert_eq!(body, original);
    apply_service_tier(&mut body, Some(ServiceTier::Fast), true);
    assert_eq!(body["service_tier"], "priority");
    assert_eq!(body["reasoning"]["effort"], "high");
    let mut standard = original.clone();
    apply_service_tier(&mut standard, Some(ServiceTier::Default), true);
    assert_eq!(standard, original);
    apply_service_tier(&mut standard, Some(ServiceTier::Default), false);
    assert_eq!(standard["service_tier"], "default");
    assert_eq!(
        serde_json::from_str::<ServiceTier>("\"priority\"").unwrap(),
        ServiceTier::Fast
    );
}

#[test]
fn service_tier_codex_routing_hint_matches_the_body_and_preserves_legacy_requests() {
    assert_eq!(openai_codex_routing_hint("gpt-6-astra", None), None);
    assert_eq!(
        openai_codex_routing_hint("gpt-6-astra", Some(ServiceTier::Fast)),
        Some((
            "x-codex-routing-hint",
            "model=gpt-6-astra;tier=priority".into()
        ))
    );
    assert_eq!(
        openai_codex_routing_hint("gpt-6-astra", Some(ServiceTier::Default)),
        Some(("x-codex-routing-hint", "model=gpt-6-astra".into()))
    );
    assert_eq!(
        openai_codex_routing_hint("bad\nmodel", Some(ServiceTier::Fast)),
        None
    );
}

#[test]
fn served_service_tier_survives_response_parsing() {
    let parsed = responses::parse_openai_response_with_transport_state(json!({
            "id":"response-fast","service_tier":"default","status":"completed",
            "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}]
        })).unwrap();
    assert_eq!(parsed.served_service_tier.as_deref(), Some("default"));
}
