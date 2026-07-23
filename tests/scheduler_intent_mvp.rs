#[path = "support/scheduler_intent_mvp.rs"]
mod model;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use holon::domain::scheduler_protocol::{
    ActivationOrigin, ActivationSlot, ActivationTrust, AgentDispatchState, ProtocolCommand,
    Snapshot, WaitGenerationRecord, WaitRecord, WaitState, WorkDemand, WorkStatus,
};
use holon::domain::scheduler_semantic::{
    resolve_semantic_proposal, validate_semantic_decision_input, validate_semantic_decision_inputs,
    SemanticDecisionInput, SemanticProposalProviderConfig, SemanticProposalProviderIdentity,
    SemanticProposalResolution, SemanticProposalResponse, SemanticValidationPolicy,
    SEMANTIC_CONTRACT_REVISION,
};
use model::{
    assert_complete_run, score, structural_baseline, validate, Corpus, Proposal, ValidationError,
};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scheduler_intent_mvp")
}

#[test]
fn deterministic_resolver_degrades_low_confidence_or_invalid_binding_to_unresolved() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "explicit_wait_id")
        .expect("case");
    let provider = SemanticProposalProviderConfig {
        identity: SemanticProposalProviderIdentity {
            provider_id: "test-provider".into(),
            model_ref: "test/model".into(),
            contract_revision: SEMANTIC_CONTRACT_REVISION,
        },
    };
    let proposal = Proposal::BindWait {
        input_id: case.id.clone(),
        snapshot_revision: case.snapshot_revision,
        wait_id: "wait-plan-1".into(),
        generation: 3,
    };

    let low_confidence = resolve_semantic_proposal(
        case,
        provider.clone(),
        SemanticProposalResponse {
            proposal: proposal.clone(),
            confidence_bps: 8_999,
            latency_ms: Some(12),
        },
        SemanticValidationPolicy::default(),
    );
    assert!(matches!(
        low_confidence,
        SemanticProposalResolution::Unresolved {
            reason: ValidationError::LowConfidence,
            ..
        }
    ));
    assert!(low_confidence.effective_proposal().is_unresolved());
    assert_eq!(low_confidence.provenance(), &case.provenance);

    let stale = resolve_semantic_proposal(
        case,
        provider,
        SemanticProposalResponse {
            proposal: Proposal::BindWait {
                input_id: case.id.clone(),
                snapshot_revision: case.snapshot_revision,
                wait_id: "wait-plan-1".into(),
                generation: 2,
            },
            confidence_bps: 10_000,
            latency_ms: None,
        },
        SemanticValidationPolicy::default(),
    );
    assert!(matches!(
        stale,
        SemanticProposalResolution::Unresolved {
            reason: ValidationError::StaleWaitGeneration,
            ..
        }
    ));
    assert!(stale.effective_proposal().is_unresolved());
}

#[test]
fn resolver_preserves_canonical_provenance_and_rejects_unsupported_provider_contracts() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "explicit_wait_id")
        .expect("case");
    let proposal = Proposal::BindWait {
        input_id: case.id.clone(),
        snapshot_revision: case.snapshot_revision,
        wait_id: "wait-plan-1".into(),
        generation: 3,
    };
    let provider = SemanticProposalProviderConfig {
        identity: SemanticProposalProviderIdentity {
            provider_id: "configured-provider".into(),
            model_ref: "test/model".into(),
            contract_revision: SEMANTIC_CONTRACT_REVISION,
        },
    };

    let accepted = resolve_semantic_proposal(
        case,
        provider.clone(),
        SemanticProposalResponse {
            proposal: proposal.clone(),
            confidence_bps: 10_000,
            latency_ms: Some(7),
        },
        SemanticValidationPolicy::default(),
    );
    assert!(accepted.accepted());
    assert_eq!(accepted.provenance(), &case.provenance);
    assert!(matches!(
        &accepted,
        SemanticProposalResolution::Accepted {
            provider: resolved_provider,
            ..
        } if resolved_provider == &provider.identity
    ));

    let unsupported = resolve_semantic_proposal(
        case,
        SemanticProposalProviderConfig {
            identity: SemanticProposalProviderIdentity {
                contract_revision: SEMANTIC_CONTRACT_REVISION + 1,
                ..provider.identity
            },
        },
        SemanticProposalResponse {
            proposal,
            confidence_bps: 10_000,
            latency_ms: None,
        },
        SemanticValidationPolicy::default(),
    );
    assert!(matches!(
        unsupported,
        SemanticProposalResolution::Unresolved {
            reason: ValidationError::UnsupportedContractRevision,
            ..
        }
    ));
    assert_eq!(unsupported.provenance(), &case.provenance);
}

#[test]
fn canonical_input_validator_rejects_invalid_provenance_and_order_dependent_candidates() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "explicit_wait_id")
        .expect("case");

    let mut invalid_provenance = case.clone();
    invalid_provenance.provenance.source_id.clear();
    assert_eq!(
        validate_semantic_decision_input(&invalid_provenance),
        Err(ValidationError::InvalidProvenance)
    );

    let mut duplicate_wait = case.clone();
    duplicate_wait.waits.push(duplicate_wait.waits[0].clone());
    assert_eq!(
        validate_semantic_decision_input(&duplicate_wait),
        Err(ValidationError::DuplicateWait)
    );

    let mut empty_routing_key = case.clone();
    empty_routing_key.waits[0].routing_keys.push(String::new());
    assert_eq!(
        validate_semantic_decision_input(&empty_routing_key),
        Err(ValidationError::InvalidRoutingKey)
    );
}

#[test]
fn canonical_input_validator_rejects_authority_spoofing_and_cross_agent_candidates() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "explicit_wait_id")
        .expect("case");

    let mut spoofed = case.clone();
    spoofed.provenance.origin = ActivationOrigin::Webhook;
    spoofed.provenance.trust = ActivationTrust::OperatorInstruction;
    assert_eq!(
        validate_semantic_decision_input(&spoofed),
        Err(ValidationError::InvalidProvenance)
    );

    let mut cross_agent_wait = case.clone();
    cross_agent_wait.waits[0].agent_id = "other-agent".into();
    assert_eq!(
        validate_semantic_decision_input(&cross_agent_wait),
        Err(ValidationError::CandidateAgentMismatch)
    );

    let mut cross_agent_work_item = case.clone();
    cross_agent_work_item.work_items[0].agent_id = "other-agent".into();
    assert_eq!(
        validate_semantic_decision_input(&cross_agent_work_item),
        Err(ValidationError::CandidateAgentMismatch)
    );

    let mut mismatched_ingress_route = case.clone();
    mismatched_ingress_route.ingress_route.agent_id = "other-agent".into();
    assert_eq!(
        validate_semantic_decision_input(&mismatched_ingress_route),
        Err(ValidationError::IngressRouteMismatch)
    );
}

#[test]
fn canonical_input_validator_rejects_aliases_and_replayed_sources() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "explicit_wait_id")
        .expect("case");

    let mut mismatched_source = case.clone();
    mismatched_source.provenance.source_id = "different-message".into();
    assert_eq!(
        validate_semantic_decision_input(&mismatched_source),
        Err(ValidationError::InputSourceMismatch)
    );

    let mut wait_alias = case.clone();
    wait_alias.waits[0].wait_id = " wait-plan-1 ".into();
    assert_eq!(
        validate_semantic_decision_input(&wait_alias),
        Err(ValidationError::InvalidWaitCandidate)
    );

    let mut routing_alias = case.clone();
    routing_alias.waits[0].routing_keys = vec!["登录".into(), " 登录 ".into()];
    assert_eq!(
        validate_semantic_decision_input(&routing_alias),
        Err(ValidationError::InvalidRoutingKey)
    );

    let mut replay = case.clone();
    replay.id = case.id.clone();
    assert_eq!(
        validate_semantic_decision_inputs(&[case.clone(), replay]),
        Err(ValidationError::DuplicateInputSource)
    );

    let missing_route = serde_json::json!({
        "id": "missing-route",
        "target_agent_id": "agent-semantic-mvp",
        "provenance": {
            "origin": "operator",
            "trust": "operator_instruction",
            "source_id": "missing-route"
        },
        "snapshot_revision": 1,
        "operator_input": "continue",
        "waits": [],
        "work_items": []
    });
    assert!(
        serde_json::from_value::<SemanticDecisionInput>(missing_route).is_err(),
        "canonical semantic input must require a trusted ingress route"
    );

    let legacy_alias = serde_json::json!({
        "kind": "unresolved",
        "case_id": case.id,
        "snapshot_revision": case.snapshot_revision
    });
    assert!(
        serde_json::from_value::<Proposal>(legacy_alias).is_err(),
        "legacy case_id proposal alias must be rejected"
    );

    let duplicate_alias = serde_json::json!({
        "kind": "unresolved",
        "input_id": case.id,
        "case_id": case.id,
        "snapshot_revision": case.snapshot_revision
    });
    assert!(
        serde_json::from_value::<Proposal>(duplicate_alias).is_err(),
        "non-canonical proposal fields must be rejected"
    );
}

fn read_json<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> T {
    let path = path.as_ref();
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

#[test]
fn corpus_gold_and_structural_baseline_are_well_formed() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let gold: Vec<Proposal> = read_json(fixture_dir().join("gold.json"));
    assert_eq!(corpus.schema_version, SEMANTIC_CONTRACT_REVISION);
    assert_complete_run(&corpus.cases, &gold);
    for (case, proposal) in corpus.cases.iter().zip(&gold) {
        assert_eq!(proposal.input_id(), case.id);
        validate(case, proposal)
            .unwrap_or_else(|error| panic!("invalid gold proposal for {}: {error:?}", case.id));
        for wait in &case.waits {
            assert!(
                !wait.owner_work_item_id.is_empty() && !wait.summary.is_empty(),
                "{}: incomplete wait candidate",
                case.id
            );
        }
        for work_item in &case.work_items {
            assert!(
                !work_item.summary.is_empty(),
                "{}: incomplete work item candidate",
                case.id
            );
        }
    }

    let baseline: Vec<_> = corpus.cases.iter().map(structural_baseline).collect();
    let baseline_score = score(&corpus.cases, &gold, &baseline);
    assert_eq!(baseline_score.total, 24);
    assert!(
        baseline_score.wrong_target_bindings > 0,
        "the safety corpus must expose structural matching risk"
    );
    println!(
        "baseline exact={:.3} binding_precision={:.3} unresolved={:.3} invalid={}",
        baseline_score.exact_accuracy(),
        baseline_score.target_binding_precision(),
        baseline_score.unresolved_rate(),
        baseline_score.invalid
    );
}

#[test]
fn validator_rejects_stale_or_ineligible_targets() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "explicit_wait_id")
        .expect("case");
    let stale_snapshot = Proposal::BindWait {
        input_id: case.id.clone(),
        snapshot_revision: case.snapshot_revision - 1,
        wait_id: "wait-plan-1".into(),
        generation: 3,
    };
    assert_eq!(
        validate(case, &stale_snapshot),
        Err(ValidationError::StaleSnapshotRevision)
    );

    let stale_generation = Proposal::BindWait {
        input_id: case.id.clone(),
        snapshot_revision: case.snapshot_revision,
        wait_id: "wait-plan-1".into(),
        generation: 2,
    };
    assert_eq!(
        validate(case, &stale_generation),
        Err(ValidationError::StaleWaitGeneration)
    );

    let inactive_case = corpus
        .cases
        .iter()
        .find(|case| case.id == "explicit_inactive_wait")
        .expect("case");
    let inactive_wait = structural_baseline(inactive_case);
    assert_eq!(
        validate(inactive_case, &inactive_wait),
        Err(ValidationError::InactiveWait)
    );

    let terminal_case = corpus
        .cases
        .iter()
        .find(|case| case.id == "explicit_terminal_work_item")
        .expect("case");
    let terminal_work = structural_baseline(terminal_case);
    assert_eq!(
        validate(terminal_case, &terminal_work),
        Err(ValidationError::TerminalWorkItem)
    );
}

#[test]
fn validator_rejects_non_explicit_binding_when_multiple_targets_are_eligible() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "two_waits_ambiguous_continue")
        .expect("case");
    let arbitrary_wait = Proposal::BindWait {
        input_id: case.id.clone(),
        snapshot_revision: case.snapshot_revision,
        wait_id: "wait-docs".into(),
        generation: 1,
    };
    assert_eq!(
        validate(case, &arbitrary_wait),
        Err(ValidationError::AmbiguousBinding)
    );

    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "similar_work_items_ambiguous")
        .expect("case");
    let arbitrary_work_item = Proposal::BindWorkItem {
        input_id: case.id.clone(),
        snapshot_revision: case.snapshot_revision,
        work_item_id: "work-auth-timeout".into(),
        revision: 4,
    };
    assert_eq!(
        validate(case, &arbitrary_work_item),
        Err(ValidationError::AmbiguousBinding)
    );
}

#[test]
fn recorded_shadow_runs_are_scored_without_granting_scheduler_authority() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let gold: Vec<Proposal> = read_json(fixture_dir().join("gold.json"));
    let authoritative_snapshot = semantic_authority_snapshot();
    let before = authoritative_snapshot.clone();
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "explicit_wait_id")
        .expect("case");
    let accepted = resolve_semantic_proposal(
        case,
        SemanticProposalProviderConfig {
            identity: SemanticProposalProviderIdentity {
                provider_id: "shadow-provider".into(),
                model_ref: "shadow/model".into(),
                contract_revision: SEMANTIC_CONTRACT_REVISION,
            },
        },
        SemanticProposalResponse {
            proposal: Proposal::BindWait {
                input_id: case.id.clone(),
                snapshot_revision: case.snapshot_revision,
                wait_id: "wait-plan-1".into(),
                generation: 3,
            },
            confidence_bps: 10_000,
            latency_ms: None,
        },
        SemanticValidationPolicy::default(),
    );
    assert!(accepted.accepted());
    assert!(
        serde_json::from_value::<ProtocolCommand>(
            serde_json::to_value(&accepted).expect("semantic resolution wire")
        )
        .is_err(),
        "accepted semantic resolution must not be executable as a scheduler command"
    );

    let baseline: Vec<_> = corpus.cases.iter().map(structural_baseline).collect();
    let baseline_score = score(&corpus.cases, &gold, &baseline);

    for run in ["run-01.json", "run-02.json", "run-03.json"] {
        let proposals: Vec<Proposal> = read_json(fixture_dir().join(run));
        assert_complete_run(&corpus.cases, &proposals);
        let run_score = score(&corpus.cases, &gold, &proposals);
        assert_eq!(
            run_score.wrong_target_bindings, 0,
            "{run}: shadow proposals must not produce an accepted wrong binding"
        );
        assert!(
            run_score.target_binding_precision() >= 0.99,
            "{run}: accepted binding precision below gate"
        );
        assert!(
            run_score.exact_accuracy() >= 0.80,
            "{run}: exact accuracy below gate"
        );
        assert!(
            run_score.exact_accuracy() - baseline_score.exact_accuracy() >= 0.25,
            "{run}: insufficient gain over structural baseline"
        );
        assert!(
            run_score.target_bindings >= 8,
            "{run}: excessive unresolved fallback"
        );
        println!(
            "{run} exact={:.3} binding_precision={:.3} unresolved={:.3} invalid={} wrong_bindings={} exact_gain={:.3}",
            run_score.exact_accuracy(),
            run_score.target_binding_precision(),
            run_score.unresolved_rate(),
            run_score.invalid,
            run_score.wrong_target_bindings,
            run_score.exact_accuracy() - baseline_score.exact_accuracy(),
        );
    }

    assert_eq!(
        authoritative_snapshot, before,
        "semantic resolution and shadow scoring must not mutate authoritative scheduler state"
    );
    assert!(authoritative_snapshot.activation_authorities.is_empty());
    assert!(authoritative_snapshot.activation_admissions.is_empty());
    assert!(authoritative_snapshot.activations.is_empty());
    assert_eq!(authoritative_snapshot.dispatch_revision, 7);
    assert_eq!(
        authoritative_snapshot.waits["wait-plan-1"].generations[&3].state,
        WaitState::Active
    );
}

fn semantic_authority_snapshot() -> Snapshot {
    Snapshot {
        slot: ActivationSlot::Idle,
        dispatch: AgentDispatchState::Open,
        dispatch_revision: 7,
        focus: Some("work-design".into()),
        work: BTreeMap::from([(
            "work-design".into(),
            WorkDemand {
                metadata_revision: 8,
                scheduling_generation: 8,
                status: WorkStatus::Waiting {
                    wait_id: "wait-plan-1".into(),
                },
                capabilities: Default::default(),
                locks: Default::default(),
                locality: "agent:agent-semantic-mvp".into(),
                cost_class: "standard".into(),
            },
        )]),
        waits: BTreeMap::from([(
            "wait-plan-1".into(),
            WaitRecord {
                current_generation: 3,
                generations: BTreeMap::from([(
                    3,
                    WaitGenerationRecord {
                        owner_work_item_id: "work-design".into(),
                        state: WaitState::Active,
                        trigger: None,
                        consuming_activation_id: None,
                    },
                )]),
            },
        )]),
        activations: BTreeMap::new(),
        activation_authorities: BTreeMap::new(),
        activation_admissions: BTreeMap::new(),
        settlements: BTreeMap::new(),
        missing_settlements: BTreeMap::new(),
        rollout: Default::default(),
        admitted_generations: Default::default(),
        continuation_admissions: BTreeMap::new(),
        activation_inputs: BTreeMap::new(),
    }
}
