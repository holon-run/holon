use super::*;

fn evidence(reference: &str, role: ProjectionEvidenceRole) -> ProjectionEvidenceRef {
    ProjectionEvidenceRef::new(
        reference,
        role,
        ProjectionOwner::AgentLifecycle {
            agent_id: "agent-1".into(),
        },
    )
}

fn manifest(refs: Vec<ProjectionEvidenceRef>) -> ProjectionManifest {
    ProjectionManifest {
        schema_version: PROJECTION_MANIFEST_SCHEMA_VERSION,
        projector: "test".into(),
        activation_owner: ProjectionOwner::AgentLifecycle {
            agent_id: "agent-1".into(),
        },
        activation_binding: None,
        turn_id: Some("turn-1".into()),
        prompt_budget_estimated_tokens: 100,
        allocated_estimated_tokens: 20,
        diagnostics: None,
        system_sections: Vec::new(),
        context_sections: vec![ProjectionSectionManifest {
            order: 0,
            section_id: "current_input".into(),
            section_name: "current_input".into(),
            stability: PromptStability::TurnScoped,
            representation: ProjectionRepresentation::Full,
            reason: "selected_full".into(),
            requested_estimated_tokens: 20,
            allocated_estimated_tokens: 20,
            rendered_chars: 40,
            content_sha256: Some("abc".into()),
            selected_evidence_refs: refs,
            omitted_evidence_refs: Vec::new(),
        }],
        invariant_results: Vec::new(),
    }
    .evaluate()
}

fn omitted_manifest(refs: Vec<ProjectionEvidenceRef>) -> ProjectionManifest {
    let mut manifest = manifest(vec![evidence(
        "message:current",
        ProjectionEvidenceRole::CurrentInput,
    )]);
    manifest.context_sections.push(ProjectionSectionManifest {
        order: 1,
        section_id: "omitted".into(),
        section_name: "omitted".into(),
        stability: PromptStability::TurnScoped,
        representation: ProjectionRepresentation::Omitted,
        reason: "omitted_lower_priority".into(),
        requested_estimated_tokens: 20,
        allocated_estimated_tokens: 0,
        rendered_chars: 0,
        content_sha256: None,
        selected_evidence_refs: Vec::new(),
        omitted_evidence_refs: refs,
    });
    manifest.evaluate()
}

#[test]
fn history_selector_serializes_stably() {
    let recent: HistorySelector = serde_json::from_str("\"recent_turns\"").unwrap();
    let scoped: HistorySelector = serde_json::from_str("\"work_item_scoped\"").unwrap();
    assert_eq!(recent, HistorySelector::RecentTurns);
    assert_eq!(scoped, HistorySelector::WorkItemScoped);
    assert_eq!(serde_json::to_string(&recent).unwrap(), "\"recent_turns\"");
    assert_eq!(
        serde_json::to_string(&scoped).unwrap(),
        "\"work_item_scoped\""
    );
}

#[test]
fn projection_diagnostics_new_fields_are_backward_compatible() {
    let diagnostics: ProjectionDiagnostics = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "history_selector": "recent_turns",
        "selection_reason": "legacy",
        "current_work_item_id": null,
        "input_message_count": 1,
        "input_estimated_tokens": 2,
        "input_chars": 3,
        "fallback_reason": null,
        "policy_version": "history_selector_v1"
    }))
    .unwrap();
    assert_eq!(diagnostics.history_window_limit, None);
    assert_eq!(diagnostics.projection_outcome, None);
    assert_eq!(diagnostics.invariant_results, None);
}

#[test]
fn identical_render_noop_is_not_reported_as_fallback() {
    let diagnostics = ProjectionDiagnostics::new(
        HistorySelector::WorkItemScoped,
        "identical_render",
        None,
        0,
        0,
        0,
    )
    .with_projection_outcome(ProjectionOutcome::IdenticalRenderNoop);
    assert_eq!(
        diagnostics.projection_outcome,
        Some(ProjectionOutcome::IdenticalRenderNoop)
    );
    assert_eq!(diagnostics.fallback_reason, None);
}

#[test]
fn projection_diagnostics_persist_the_six_runtime_safety_invariants() {
    let manifest = manifest(vec![evidence(
        "message:current",
        ProjectionEvidenceRole::CurrentInput,
    )]);
    let diagnostics = ProjectionDiagnostics::new(
        HistorySelector::WorkItemScoped,
        "scoped",
        Some("work-1".into()),
        1,
        1,
        1,
    )
    .with_manifest_invariants(&manifest);
    let invariant_results = diagnostics.invariant_results.unwrap();
    assert_eq!(invariant_results.len(), 6);
    assert!(!invariant_results
        .iter()
        .any(|result| result.code == "activation_binding_consistent"));
    assert!(DIAGNOSTIC_INVARIANTS.iter().all(|expected| {
        invariant_results
            .iter()
            .any(|result| result.code == *expected)
    }));
}

#[test]
fn selector_comparison_requires_same_activation() {
    let recent = manifest(vec![evidence(
        "message:current",
        ProjectionEvidenceRole::CurrentInput,
    )]);
    let scorecard = compare_history_selectors(&recent, &recent).unwrap();
    assert!(scorecard.passed);

    let mut different = recent.clone();
    different.turn_id = Some("turn-2".into());
    let scorecard = compare_history_selectors(&recent, &different).unwrap();
    assert!(!scorecard.passed);
    assert!(scorecard.assertions.iter().any(|assertion| {
        assertion.code == "selector_comparison_same_activation"
            && assertion.status == ProjectionInvariantStatus::Fail
    }));
}

#[test]
fn canonical_json_is_byte_stable() {
    let manifest = manifest(vec![evidence(
        "message:current",
        ProjectionEvidenceRole::CurrentInput,
    )]);

    assert_eq!(
        manifest.canonical_json().unwrap(),
        manifest.canonical_json().unwrap()
    );
    assert_eq!(
        manifest.byte_sha256().unwrap(),
        manifest.byte_sha256().unwrap()
    );
}

#[test]
fn restart_rebuild_is_byte_equivalent() {
    let before = manifest(vec![
        evidence("message:current", ProjectionEvidenceRole::CurrentInput),
        evidence("message:prior", ProjectionEvidenceRole::DirectPredecessor),
    ]);
    let rebuilt: ProjectionManifest =
        serde_json::from_str(&before.canonical_json().unwrap()).unwrap();

    assert_eq!(
        before.canonical_json().unwrap(),
        rebuilt.canonical_json().unwrap()
    );
    assert_eq!(
        before.byte_sha256().unwrap(),
        rebuilt.byte_sha256().unwrap()
    );
}

#[test]
fn self_test_detects_duplicate_ref() {
    let current = evidence("message:current", ProjectionEvidenceRole::CurrentInput);
    let manifest = manifest(vec![current.clone(), current]);

    assert!(manifest.invariant_results.iter().any(|result| {
        result.code == "canonical_evidence_unique"
            && result.status == ProjectionInvariantStatus::Fail
    }));
}

#[test]
fn self_test_detects_wrong_owner() {
    let mut current = evidence("message:current", ProjectionEvidenceRole::CurrentInput);
    current.owner = ProjectionOwner::WorkItem {
        work_item_id: "other".into(),
    };
    let manifest = manifest(vec![current]);

    assert!(manifest.invariant_results.iter().any(|result| {
        result.code == "evidence_owner_consistent"
            && result.status == ProjectionInvariantStatus::Fail
    }));
}

#[test]
fn self_test_detects_owner_binding_mismatch() {
    let mut manifest = manifest(vec![evidence(
        "message:current",
        ProjectionEvidenceRole::CurrentInput,
    )]);
    manifest.activation_binding = Some(ProjectionBindingSummary {
        source_message_id: "message:current".into(),
        turn_id: "turn-1".into(),
        owner: None,
        work_item_id: Some("work-1".into()),
        claimed_work_revision: Some(1),
    });
    let manifest = manifest.evaluate();

    assert!(manifest.invariant_results.iter().any(|result| {
        result.code == "activation_binding_consistent"
            && result.status == ProjectionInvariantStatus::Fail
    }));
}

#[test]
fn self_test_detects_missing_current_input() {
    let manifest = manifest(vec![evidence("turn:prior", ProjectionEvidenceRole::Turn)]);

    assert!(manifest.invariant_results.iter().any(|result| {
        result.code == "current_input_retained" && result.status == ProjectionInvariantStatus::Fail
    }));
}

#[test]
fn self_test_detects_missing_direct_predecessor() {
    let manifest = omitted_manifest(vec![evidence(
        "message:prior",
        ProjectionEvidenceRole::DirectPredecessor,
    )]);

    assert!(manifest.invariant_results.iter().any(|result| {
        result.code == "direct_predecessor_retained"
            && result.status == ProjectionInvariantStatus::Fail
    }));
}

#[test]
fn self_test_detects_cross_owner_leakage() {
    let mut leaked = evidence("message:foreign", ProjectionEvidenceRole::Supporting);
    leaked.owner = ProjectionOwner::Conversation {
        interaction_id: "other-interaction".into(),
    };
    let manifest = manifest(vec![
        evidence("message:current", ProjectionEvidenceRole::CurrentInput),
        leaked,
    ]);

    assert!(manifest.invariant_results.iter().any(|result| {
        result.code == "evidence_owner_consistent"
            && result.status == ProjectionInvariantStatus::Fail
    }));
}

#[test]
fn self_test_detects_non_monotonic_budget_projection() {
    let larger = manifest(vec![evidence(
        "message:current",
        ProjectionEvidenceRole::CurrentInput,
    )]);
    let mut smaller = manifest(vec![
        evidence("message:current", ProjectionEvidenceRole::CurrentInput),
        evidence("brief:new", ProjectionEvidenceRole::Result),
    ]);
    smaller.prompt_budget_estimated_tokens = 50;

    assert_eq!(
        compare_budget_monotonicity(&larger, &smaller).status,
        ProjectionInvariantStatus::Fail
    );
}
