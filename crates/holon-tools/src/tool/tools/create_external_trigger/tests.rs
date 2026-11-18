use super::*;

#[test]
fn delivery_mode_accepts_legacy_wake_only_alias() {
    let args: CreateExternalTriggerArgs = serde_json::from_value(serde_json::json!({
        "description": "Check external queue",
        "source": "test",
        "delivery_mode": "wake_only"
    }))
    .expect("legacy wake_only alias should deserialize");

    assert!(matches!(
        args.delivery_mode,
        CallbackDeliveryModeArgs::WakeHint
    ));
}
