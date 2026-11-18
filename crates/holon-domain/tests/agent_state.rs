use holon_domain::AgentState;

#[test]
fn agent_timezone_configuration_validation_and_compatibility() {
    let mut state = AgentState::new("timezone-test");
    let old_state = serde_json::to_value(&state).unwrap();
    assert!(old_state.get("timezone_override").is_none());
    assert!(serde_json::from_value::<AgentState>(old_state)
        .unwrap()
        .timezone_override
        .is_none());
    state.set_timezone(Some("America/Los_Angeles")).unwrap();
    assert!(state.set_timezone(Some("invalid/zone")).is_err());
    assert_eq!(
        state.timezone_override.as_deref(),
        Some("America/Los_Angeles")
    );
    let mut encoded = serde_json::to_value(&state).unwrap();
    assert_eq!(
        serde_json::from_value::<AgentState>(encoded.clone())
            .unwrap()
            .timezone_override,
        state.timezone_override
    );
    encoded["timezone_override"] = serde_json::json!("invalid/zone");
    assert!(serde_json::from_value::<AgentState>(encoded).is_err());
    state.set_timezone(None).unwrap();
    assert!(state.timezone_override.is_none());
}
