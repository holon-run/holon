use super::*;

fn settle_runnable(outcome: WorkItemOutcome) -> WorkItemExecutionState {
    let state = WorkItemExecutionState::Runnable {
        generation: 7,
        recovery_ref: None,
    };
    let source = ExecutionSourceIdentity::WorkItemContinuation {
        work_item_id: "work-1".into(),
    };
    let turn = ExecutionOutcome::WorkItem(outcome).model_turn().unwrap();
    let transition = plan_bound_turn_transition(&turn, &state, &source).unwrap();
    plan_work_item_transition(7, transition).unwrap()
}

#[test]
fn turn_attribution_projects_one_owner_without_dropping_context() {
    let attribution = TurnAttribution {
        work_item_id: Some("work-1".into()),
        interaction_id: Some("interaction-1".into()),
    };
    assert_eq!(
        attribution.clone().into_turn_owner("agent-1"),
        crate::TurnOwner::WorkItem {
            work_item_id: "work-1".into(),
        }
    );
    let wire = serde_json::to_value(&attribution).unwrap();
    assert_eq!(
        serde_json::from_value::<TurnAttribution>(wire).unwrap(),
        attribution
    );
}

#[test]
fn aborted_work_preserves_runnable_work_without_an_input_replay_anchor() {
    let outcome = WorkItemOutcome::Aborted {
        reason: "operator_aborted".into(),
    };
    assert_eq!(
        settle_runnable(outcome.clone()),
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
        settle_runnable(outcome.clone()),
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
