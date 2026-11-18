use super::*;

#[test]
fn legacy_interrupt_priority_deserializes_as_interject() {
    let args: EnqueueArgs = serde_json::from_value(serde_json::json!({
        "text": "follow up",
        "priority": "interrupt"
    }))
    .unwrap();

    assert!(matches!(args.priority, Some(EnqueuePriority::Interject)));
}
