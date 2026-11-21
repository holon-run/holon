use super::*;
use crate::runtime_db::input_mutation_receipts::{
    insert_input_mutation_receipt_tx, with_fresh_input_mutation_preparation_and_preflight,
    InputMutationOutcome, InputMutationResult, PreparedInputMutation,
};

struct Fixture {
    _directory: tempfile::TempDir,
    db: RuntimeDb,
    budget: InputEvidenceBudgetLedger,
    request: TaskDomainLoadRequest,
    attempt: OriginalInputMutationAttempt,
    witness: MutationWitness,
}

impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )?;
        let descriptor = PreparedInputMutation::new(
            InputMutationFamily::Task,
            "task-current-original".into(),
            vec![InputEvidenceScope {
                agent_id: "task-current-owner".into(),
                identity_incarnation: 1,
            }],
            "a".repeat(64),
            "2026-11-16T00:00:00Z".parse()?,
        )?;
        let attempt = OriginalInputMutationAttempt::new(descriptor);
        let witness = MutationWitness::input_mutation(
            "task-current-original".into(),
            attempt.clone(),
            Vec::new(),
        )?;
        Ok(Self {
            _directory: directory,
            db,
            budget: InputEvidenceBudgetLedger::new(Default::default())?,
            request: TaskDomainLoadRequest {
                recovery_at: None,
                query: Default::default(),
                capture: TaskCaptureRequest::current(),
                record_limit: 4096,
                byte_limit: 4 * 1024 * 1024,
            },
            attempt,
            witness,
        })
    }

    fn prepare(&self) -> Result<TaskCurrentPreparation> {
        prepare(
            &self.db,
            &self.attempt,
            &self.witness,
            &[],
            &self.request,
            &self.budget,
            &[],
        )
    }
}

#[test]
fn task_receipt_current_authority_requires_finished_before_receipt_or_reader_lookup() -> Result<()>
{
    let fixture = Fixture::new()?;
    fixture.db.transaction(|tx| {
        tx.execute_batch("DROP TABLE input_mutation_receipts")?;
        Ok(())
    })?;
    let error = fixture
        .prepare()
        .err()
        .context("unfinished attempt unexpectedly recovered")?;
    assert!(format!("{error:#}").contains("unfinished"), "{error:#}");
    Ok(())
}

#[test]
fn task_receipt_current_fresh_absence_does_not_initialize_or_execute_business() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.witness.finish_original_attempt();
    let prepared = fixture.prepare()?;
    assert!(prepared.receipt.is_none());
    assert!(prepared.candidates.is_empty());
    with_fresh_input_mutation_preparation_and_preflight(
        &fixture.db,
        &fixture.attempt,
        |_| Ok(()),
        |tx, resolution| {
            prepared.validate_receipt(&fixture.attempt, &resolution)?;
            assert_eq!(resolution, InputMutationResolution::Absent);
            let counts: (usize, usize) = tx.query_row(
                "SELECT (SELECT COUNT(*) FROM task_domain_current),(SELECT COUNT(*) FROM input_mutation_receipts)",
                [], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(counts, (0, 0));
            Ok(())
        },
    )?;
    Ok(())
}

#[test]
fn task_receipt_current_rechecks_the_exact_positive_receipt_in_original_writer() -> Result<()> {
    let fixture = Fixture::new()?;
    let descriptor = fixture.attempt.prepared();
    let receipt = fixture.db.transaction(|tx| {
        insert_input_mutation_receipt_tx(
            tx,
            descriptor,
            &InputMutationOutcome {
                result: InputMutationResult::Task {
                    task_id: "original-task".into(),
                    deletion: None,
                    stop: None,
                },
                applied: true,
                delivery_receipt: None,
                scopes: vec![InputMutationScopeOutcome {
                    scope: descriptor.scopes()[0].clone(),
                    install_keys: Default::default(),
                    audit_events: Vec::new(),
                    notify_memory_index: false,
                    notify_scheduler: false,
                }],
            },
            descriptor.prepared_at(),
        )
    })?;
    fixture.witness.finish_original_attempt();
    // The namespace is retired/absent. A positive receipt still resolves its
    // exact original outcome, without acquiring a successor's Task domain.
    let prepared = fixture.prepare()?;
    assert_eq!(prepared.receipt.as_ref(), Some(&receipt));
    assert!(prepared.candidates.is_empty());
    with_fresh_input_mutation_preparation_and_preflight(
        &fixture.db,
        &fixture.attempt,
        |_| Ok(()),
        |_, resolution| prepared.validate_receipt(&fixture.attempt, &resolution),
    )?;
    let mut different = receipt;
    different.outcome.applied = false;
    assert!(prepared
        .validate_receipt(
            &fixture.attempt,
            &InputMutationResolution::Present(different),
        )
        .is_err());
    Ok(())
}

#[test]
fn task_receipt_current_stale_qualification_retries_are_bounded() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.witness.finish_original_attempt();
    // A fresh absent result has no business command callback. Inject stale
    // qualification to exercise the same finite retry envelope independently.
    let mut qualifications = 0;
    let result: Result<()> = with_preparation(
        &fixture.db,
        &fixture.attempt,
        &fixture.witness,
        &[],
        &fixture.request,
        &fixture.budget,
        &[],
        |_| Ok(()),
        |_, resolution, _| {
            assert_eq!(resolution, InputMutationResolution::Absent);
            qualifications += 1;
            Err(TaskPreparationCandidateStale("injected current drift").into())
        },
    );
    assert!(result.unwrap_err().is::<TaskPreparationCandidateStale>());
    assert_eq!(qualifications, 3);
    let receipts: usize = fixture.db.read_for_preparation(|tx| {
        Ok(
            tx.query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |row| {
                row.get(0)
            })?,
        )
    })?;
    assert_eq!(receipts, 0);
    Ok(())
}
