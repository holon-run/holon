use super::*;
use crate::{
    runtime::{
        mutation_owner::MutationReservation,
        mutation_witness::MutationWitness,
        tests::support::{context_config, StubProvider},
        RuntimeHandle,
    },
    runtime_db::{
        input_mutation_receipts::OriginalInputMutationAttempt,
        transitions::{AgentPostureTransitionCommand, AgentStateMutation, PreparedInputTransition},
    },
    types::AgentStatus,
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "live-query-recovery".into(),
        identity_incarnation: 1,
    }
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(Default::default())?)
}
#[test]
fn recovery_roles_are_exact_live_instance_observations_and_own_no_registry() -> Result<()> {
    let registry = TaskQueryRegistry::default();
    let budget = ledger()?;
    let required = [TaskKey::new(
        TaskFamily::ResultSettlement,
        "original-result",
    )];
    let role = registry.register(
        &scope(),
        7,
        TaskQuerySpec {
            required_keys: &required,
            output_task_ids: &["output-target"],
            reply_message_ids: &["reply-target"],
            parent_turn_ids: &[],
            birth_after: Some(19),
        },
        &budget,
        4096,
        1024 * 1024,
    )?;
    assert!(registry
        .recovery_snapshot("live-query-recovery", 2, 7, &budget, 4096, 1024 * 1024)?
        .is_none());
    assert!(registry
        .recovery_snapshot("live-query-recovery", 1, 8, &budget, 4096, 1024 * 1024)?
        .is_none());
    let frozen = registry
        .recovery_snapshot("live-query-recovery", 1, 7, &budget, 4096, 1024 * 1024)?
        .unwrap();
    assert_eq!(frozen.selection_for(&scope(), 7), Some(role.selection()));
    assert!(frozen.selection_for_owner("foreign", 1, 7).is_none());
    assert!(frozen.selection_for(&scope(), 8).is_none());
    let original_selection = role.selection().clone();
    drop(role);
    assert_eq!(frozen.selection_for(&scope(), 7), Some(&original_selection));
    let weak = Arc::downgrade(&registry.0);
    drop(registry);
    assert!(weak.upgrade().is_none());
    assert!(budget.report()?.observed_bytes > 0);
    drop(frozen);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn recovery_roles_do_not_resurrect_dead_slots_or_allocate_for_cold_instances() -> Result<()> {
    let registry = TaskQueryRegistry::default();
    let budget = ledger()?;
    let role = registry.register(
        &scope(),
        3,
        TaskQuerySpec {
            birth_after: Some(0),
            ..Default::default()
        },
        &budget,
        4096,
        1024 * 1024,
    )?;
    let lock = registry.0.slots.lock().unwrap();
    drop(role);
    assert!(budget.report()?.observed_bytes > 0);
    drop(lock);
    assert!(registry
        .recovery_snapshot("live-query-recovery", 1, 3, &budget, 0, 0)?
        .is_none());
    assert_eq!(budget.report()?.observed_bytes, 0);
    let cold = TaskQueryRegistry::default();
    assert!(cold
        .recovery_snapshot("live-query-recovery", 1, 3, &budget, 0, 0)?
        .is_none());
    assert!(cold.0.slots.lock().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn original_input_receipt_recovery_preserves_only_actual_live_queries() -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new(
        "live-query-recovery",
        home.path().to_path_buf(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "live-query-recovery".into(),
        context_config(),
    )?;
    drop(runtime.begin_mutation().await?);
    let role = runtime.register_task_query(TaskQuerySpec {
        birth_after: Some(0),
        reply_message_ids: &["original-empty-reply"],
        ..Default::default()
    })?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let original = root.state.as_ref().clone();
    let mut final_state = original.clone();
    final_state.status = AgentStatus::Stopped;
    let command = PreparedInputTransition::posture(
        scope(),
        "query-original-receipt".into(),
        chrono::Utc::now(),
        AgentPostureTransitionCommand {
            agent_id: scope().agent_id,
            expected_incarnation: Some(1),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(original)),
                record: Box::new(final_state),
            },
            audit_events: Vec::new(),
            fault: None,
        },
        None,
    )?;
    let witness = MutationWitness::input_mutation(
        "query-original-receipt".into(),
        OriginalInputMutationAttempt::new(command.descriptor().clone()),
        Vec::new(),
    )?;
    let mut reservation = runtime.inner.mutation_owner.try_reserve()?;
    MutationReservation::begin_all_with_witness(
        std::slice::from_mut(&mut reservation),
        "query-original-receipt".into(),
        witness.clone(),
    )?;
    runtime
        .inner
        .runtime_db
        .transitions_in_scope(&scope())
        .commit_prepared_transition(&command)?;
    witness.finish_original_attempt();
    drop((reservation, root));
    assert!(runtime.inner.mutation_owner.capture().is_err());
    let recovered = runtime.begin_mutation().await?;
    let tasks =
        recovered.capture_tasks(&crate::runtime::resident_task::TaskCaptureRequest::current())?;
    assert_eq!(tasks.coverage.query.birth_after, Some(0));
    assert_eq!(
        tasks
            .coverage
            .query
            .reply_task_ids
            .get("original-empty-reply"),
        Some(&Default::default())
    );
    assert_eq!(tasks.task_birth_cursor()?, 0);
    drop((tasks, recovered, role));
    // A second exact receipt after release must not inherit durable query roles.
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let original = root.state.as_ref().clone();
    let command = PreparedInputTransition::posture(
        scope(),
        "query-after-release".into(),
        chrono::Utc::now(),
        AgentPostureTransitionCommand {
            agent_id: scope().agent_id,
            expected_incarnation: Some(1),
            agent_state: AgentStateMutation {
                expected: Some(Box::new(original.clone())),
                record: Box::new(original),
            },
            audit_events: Vec::new(),
            fault: None,
        },
        None,
    )?;
    let witness = MutationWitness::input_mutation(
        "query-after-release".into(),
        OriginalInputMutationAttempt::new(command.descriptor().clone()),
        Vec::new(),
    )?;
    let mut reservation = runtime.inner.mutation_owner.try_reserve()?;
    MutationReservation::begin_all_with_witness(
        std::slice::from_mut(&mut reservation),
        "query-after-release".into(),
        witness.clone(),
    )?;
    runtime
        .inner
        .runtime_db
        .transitions_in_scope(&scope())
        .commit_prepared_transition(&command)?;
    witness.finish_original_attempt();
    drop((reservation, root));
    let recovered = runtime.begin_mutation().await?;
    let tasks =
        recovered.capture_tasks(&crate::runtime::resident_task::TaskCaptureRequest::current())?;
    assert_eq!(tasks.coverage.query.birth_after, None);
    assert!(tasks.coverage.query.reply_task_ids.is_empty());
    Ok(())
}
