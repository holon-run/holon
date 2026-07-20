#[path = "support/scheduler_intent_mvp.rs"]
mod model;

use std::path::{Path, PathBuf};

use model::{
    assert_complete_run, score, structural_baseline, validate, Corpus, Proposal, ValidationError,
};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scheduler_intent_mvp")
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
    assert_eq!(corpus.schema_version, 1);
    assert_complete_run(&corpus.cases, &gold);
    for (case, proposal) in corpus.cases.iter().zip(&gold) {
        assert_eq!(proposal.case_id(), case.id);
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
        case_id: case.id.clone(),
        snapshot_revision: case.snapshot_revision - 1,
        wait_id: "wait-plan-1".into(),
        generation: 3,
    };
    assert_eq!(
        validate(case, &stale_snapshot),
        Err(ValidationError::StaleSnapshotRevision)
    );

    let stale_generation = Proposal::BindWait {
        case_id: case.id.clone(),
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
fn recorded_shadow_runs_are_scored_without_granting_scheduler_authority() {
    let corpus: Corpus = read_json(fixture_dir().join("inputs.json"));
    let gold: Vec<Proposal> = read_json(fixture_dir().join("gold.json"));
    let baseline: Vec<_> = corpus.cases.iter().map(structural_baseline).collect();
    let baseline_score = score(&corpus.cases, &gold, &baseline);

    for run in ["run-01.json", "run-02.json", "run-03.json"] {
        let proposals: Vec<Proposal> = read_json(fixture_dir().join(run));
        assert_complete_run(&corpus.cases, &proposals);
        let run_score = score(&corpus.cases, &gold, &proposals);
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
}
