use super::*;

#[test]
fn aborted_work_preserves_runnable_work_without_an_input_replay_anchor() {
    let outcome = WorkItemOutcome::Aborted {
        reason: "operator_aborted".into(),
    };
    assert_eq!(
        plan_work_item_outcome(7, &outcome).unwrap(),
        WorkItemExecutionState::Runnable {
            generation: 8,
            recovery_ref: None
        },
    );
    let wire = serde_json::to_value(&outcome).unwrap();
    assert_eq!(wire["kind"], "aborted");
    assert_eq!(
        serde_json::from_value::<WorkItemOutcome>(wire).unwrap(),
        outcome
    );
    let conversation = ConversationOutcome::Aborted {
        reason: "operator_aborted".into(),
    };
    assert_eq!(
        serde_json::from_value::<ConversationOutcome>(
            serde_json::to_value(&conversation).unwrap(),
        )
        .unwrap(),
        conversation,
    );
}

#[test]
fn reentry_outcome_maps_to_runnable_with_a_recovery_reference() {
    let outcome = WorkItemOutcome::Reentry {
        recovery_ref: "provider_dispatch_rejected:turn-1".into(),
    };
    assert_eq!(
        plan_work_item_outcome(7, &outcome).unwrap(),
        WorkItemExecutionState::Runnable {
            generation: 8,
            recovery_ref: Some("provider_dispatch_rejected:turn-1".into()),
        }
    );
    let wire = serde_json::to_value(&outcome).unwrap();
    assert_eq!(wire["kind"], "reentry");
    assert_eq!(
        serde_json::from_value::<WorkItemOutcome>(wire).unwrap(),
        outcome
    );
}
