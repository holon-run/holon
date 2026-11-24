use holon_domain::model_values::ServiceTier;

#[test]
fn service_tier_wire_values_round_trip() {
    for (tier, value) in [
        (ServiceTier::Default, "default"),
        (ServiceTier::Fast, "fast"),
        (ServiceTier::Ultrafast, "ultrafast"),
    ] {
        assert_eq!(tier.preference_value(), value);
        assert_eq!(
            serde_json::to_string(&tier).unwrap(),
            format!("\"{value}\"")
        );
        assert_eq!(
            serde_json::from_str::<ServiceTier>(&format!("\"{value}\"")).unwrap(),
            tier
        );
    }
}

#[test]
fn priority_stays_a_fast_alias() {
    assert_eq!(
        serde_json::from_str::<ServiceTier>("\"priority\"").unwrap(),
        ServiceTier::Fast
    );
}

#[test]
fn unknown_service_tier_is_an_error() {
    let error = serde_json::from_str::<ServiceTier>("\"turbo\"").unwrap_err();
    assert!(error.to_string().contains("turbo"), "{error}");
}
