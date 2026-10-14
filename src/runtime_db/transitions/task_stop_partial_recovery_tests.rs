//! A real sparse Result ACK installs its ordinary domains without Task hydration.
use super::*;
use crate::runtime::{
    input_recovery::resolve_input_recovery,
    mutation_owner::{KnownCommit, MutationOwner, MutationReservation},
    mutation_witness::MutationWitness,
    resident_work::{CanonicalWorkRecord, WorkFamily, WorkKey},
};
use crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt;

fn incomplete(f: &Fixture) -> Result<()> {
    f.db.connection()?.execute("UPDATE task_domain_current SET coverage_json=json_set(coverage_json,'$.active_complete',json('false'),'$.unsettled_complete',json('false'),'$.stop.pending_complete',json('false')) WHERE agent_id=?1",[&f.scope.agent_id])?;
    Ok(())
}
async fn original_attempt(
    f: &Fixture,
    p: &PreparedInputTransition,
) -> Result<(MutationOwner, Vec<MutationReservation>, MutationWitness)> {
    let owner = MutationOwner::standalone(&f.scope.agent_id, f.scope.identity_incarnation);
    let mut reservation = owner.reserve().await?;
    let state = Arc::new(f.state()?);
    reservation.begin("partial-original-state".into())?;
    reservation.committed(state.clone(), None)?;
    reservation.publish(state)?;
    let mut reservations = vec![reservation];
    let witness = MutationWitness::input_mutation(
        p.descriptor().command_nonce().into(),
        OriginalInputMutationAttempt::new(p.descriptor().clone()),
        vec![],
    )?
    .with_task_stop_parent_observation(&reservations[0], p)?;
    MutationReservation::begin_all_with_witness(
        &mut reservations,
        p.descriptor().command_nonce().into(),
        witness.clone(),
    )?;
    Ok((owner, reservations, witness))
}
fn recover(
    f: &Fixture,
    reservations: &mut [MutationReservation],
    l: &InputEvidenceBudgetLedger,
) -> Result<bool> {
    resolve_input_recovery(
        &f.db,
        reservations,
        l,
        &crate::context::ContextConfig::default(),
        &crate::config::RuntimeModelCatalog::default(),
    )
}
#[tokio::test]
async fn incomplete_delivered_lost_ack_installs_actual_queue_work_input_but_no_task_domain(
) -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, false, true)?;
    incomplete(&f)?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    let (owner, mut reservations, witness) = original_attempt(&f, &p).await?;
    let InputMutationExecution::Committed { receipt, .. } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    assert_eq!(
        ack(&receipt).install_mode,
        stops::TaskStopPhaseInstallMode::Incomplete
    );
    witness.finish_original_attempt();
    assert!(
        owner.capture().is_err(),
        "an ACK without publication remains gated"
    );
    assert!(recover(&f, &mut reservations, &l)?);
    let known = MutationReservation::validate_group_known(&reservations)?;
    let KnownCommit::State {
        state,
        episodes: Some(_),
        input_evidence: Some(inputs),
        work: Some(work),
        tasks: None,
        scheduling: Some(scheduling),
        runtime: Some(runtime),
    } = known[0].as_ref().unwrap()
    else {
        panic!("partial ACK lost ordinary domains")
    };
    assert_eq!(state.pending, 1);
    assert_eq!(scheduling.scope(), &f.scope);
    let coverage = scheduling.coverage();
    assert!(
        coverage.active_timers_complete
            && coverage.pending_wakes_complete
            && coverage.active_triggers_complete,
        "partial recovery must retain the same-view complete Scheduling domain"
    );
    let (revision, stored_coverage): (u64, String) = f.db.connection()?.query_row(
        "SELECT revision,coverage_json FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![f.scope.agent_id, f.scope.identity_incarnation],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(coverage.revision, revision);
    assert_eq!(
        coverage,
        &serde_json::from_str::<crate::runtime::resident_scheduling::SchedulingCoverage>(
            &stored_coverage,
        )?,
        "Known Scheduling must be the current SQL coverage, not a previous publication"
    );
    let queue_guard = runtime
        .task_stop_queue_metadata_test_weak()
        .context("new Queue container guard missing")?;
    let held_runtime = runtime.clone();
    assert!(queue_guard.upgrade().is_some());
    let input = inputs.capture_for_request(
        &crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            mandatory_roots: std::collections::BTreeSet::from([
                crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                    crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                    "frozen-stop-feedback",
                ),
            ]),
            windows: vec![],
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit: 4096,
            byte_limit: 4 * 1024 * 1024,
        },
    )?;
    assert!(input
        .records
        .iter()
        .any(|r| r.locator.key.id == "frozen-stop-feedback"));
    let captured = work.capture(&crate::runtime::resident_work::WorkCaptureRequest::current())?;
    let wait = captured
        .lookup(&WorkKey::new(WorkFamily::Wait, "result-existing-wait"))?
        .context("actual Triggered Wait missing")?;
    let CanonicalWorkRecord::Wait(wait) = &wait.record else {
        unreachable!()
    };
    assert_eq!(wait.status, crate::types::WaitConditionStatus::Triggered);
    MutationReservation::publish_all_known(&mut reservations)?;
    let published = owner.capture()?.unwrap();
    assert!(published.tasks.is_none());
    assert!(Arc::ptr_eq(
        published
            .scheduling
            .as_ref()
            .context("published Scheduling missing")?,
        scheduling,
    ));
    drop(published);
    assert!(
        queue_guard.upgrade().is_some(),
        "saved facts must retain Queue metadata after publication"
    );
    drop(known);
    drop(held_runtime);
    assert!(
        queue_guard.upgrade().is_none(),
        "last saved facts releases Queue container guard"
    );
    let count: i64 = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",
        [p.descriptor().command_nonce()],
        |r| r.get(0),
    )?;
    assert_eq!(count, 1);
    assert!(!recover(&f, &mut reservations, &l)?);
    Ok(())
}
#[tokio::test]
async fn incomplete_delivered_bad_large_state_keeps_unknown_and_releases_failed_view_guards(
) -> Result<()> {
    let f = Fixture::new()?;
    let full = ledger()?;
    let source = source(&f, &full, false, false)?;
    incomplete(&f)?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &full)?;
    let (owner, mut reservations, witness) = original_attempt(&f, &p).await?;
    commit(&f, &p, &full)?;
    witness.finish_original_attempt();
    f.db.connection()?.execute(
        "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
        params!["{".repeat(512 * 1024), f.scope.agent_id],
    )?;
    let mut limits = StateMemoryLimits::default();
    limits.history_per_agent_bytes = 1024 * 1024;
    limits.pending_per_agent_bytes = 4096;
    let limited = InputEvidenceBudgetLedger::new(limits)?;
    let baseline = limited.report()?.observed_bytes;
    let error = recover(&f, &mut reservations, &limited)
        .err()
        .context("large State accepted")?;
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert!(owner.capture().is_err());
    assert!(reservations[0].recovery()?.unwrap().known.is_none());
    assert_eq!(limited.report()?.observed_bytes, baseline);
    Ok(())
}
#[tokio::test]
async fn observed_sparse_result_replay_installs_current_domains_without_old_notifications(
) -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source(&f, &l, false, false)?;
    incomplete(&f)?;
    let p = PreparedInputTransition::task_stop_result(&source.record, &request(), &l)?;
    commit(&f, &p, &l)?;
    let (_owner, mut reservations, witness) = original_attempt(&f, &p).await?;
    let InputMutationExecution::Replayed { .. } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    witness.mark_input_storage_replayed()?;
    witness.finish_original_attempt();
    assert!(recover(&f, &mut reservations, &l)?);
    let known = MutationReservation::validate_group_known(&reservations)?;
    let KnownCommit::State {
        tasks: None,
        runtime: Some(facts),
        ..
    } = known[0].as_ref().unwrap()
    else {
        unreachable!()
    };
    let effects = facts.external_effects();
    assert!(
        effects.audit_events.is_empty()
            && !effects.notify_scheduler
            && !effects.notify_memory_index
    );
    assert_eq!(facts.state.pending, 1);
    Ok(())
}

#[tokio::test]
async fn metadata_only_incomplete_phase_keeps_pre_sql_observation_without_reading_unknown_root(
) -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    let source = source_with_signal(&f, &l, false, true, stops::TaskStopSignal::Abort)?;
    let state = Arc::new(f.state()?);
    let input_request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        mandatory_roots: Default::default(),
        windows: vec![],
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    };
    let prepared = f.db.transaction(|tx| {
        crate::runtime_db::input_preparation::prepare_original_input_recovery_with_work_tx(
            tx,
            &f.scope,
            &state,
            &input_request,
            12,
            &l,
            &crate::runtime::resident_work::WorkCaptureRequest::current(),
            &crate::runtime::resident_task::TaskCaptureRequest::current(),
        )
    })?;
    let owner = MutationOwner::standalone(&f.scope.agent_id, 1);
    let mut reservation = owner.reserve().await?;
    reservation.begin("p1-original-domains".into())?;
    reservation.committed_prepared_with_domains(
        state.clone(),
        Some(Arc::new(prepared.episodes)),
        Some(Arc::new(prepared.inputs.with_budget(l.clone())?)),
        prepared.work,
        None,
    )?;
    reservation.publish(state)?;
    let old = owner.capture()?.unwrap();
    incomplete(&f)?;
    let phase = PreparedInputTransition::task_stop_safe_progress(
        &source.record,
        "partial-p1-original".into(),
        f.at + chrono::Duration::seconds(3),
        stops::TaskStopProgressAction::PhysicalAttention {
            reason: "original monitor still being drained".into(),
        },
        &request(),
        &l,
    )?;
    let witness = MutationWitness::input_mutation(
        phase.descriptor().command_nonce().into(),
        OriginalInputMutationAttempt::new(phase.descriptor().clone()),
        vec![],
    )?
    .with_task_stop_parent_observation(&reservation, &phase)?;
    let mut reservations = vec![reservation];
    MutationReservation::begin_all_with_witness(
        &mut reservations,
        phase.descriptor().command_nonce().into(),
        witness.clone(),
    )?;
    commit(&f, &phase, &l)?;
    witness.finish_original_attempt();
    assert!(owner.capture().is_err());
    // Metadata P1 must not acquire a new State body behind its Unknown gate.
    f.db.connection()?.execute(
        "UPDATE agent_states SET payload_json='{poisoned' WHERE agent_id=?1",
        [&f.scope.agent_id],
    )?;
    assert!(recover(&f, &mut reservations, &l)?);
    MutationReservation::publish_all_known(&mut reservations)?;
    let current = owner.capture()?.unwrap();
    assert!(Arc::ptr_eq(&old.state, &current.state));
    assert!(Arc::ptr_eq(
        old.episodes.as_ref().unwrap(),
        current.episodes.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        old.input_evidence.as_ref().unwrap(),
        current.input_evidence.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        old.work.as_ref().unwrap(),
        current.work.as_ref().unwrap()
    ));
    assert!(current.tasks.is_none());
    Ok(())
}
