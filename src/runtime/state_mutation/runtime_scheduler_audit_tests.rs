use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadFact, CurrentReadRequest},
    scheduler::{self, SchedulerDecision, SchedulerDecisionKind},
    tests::support::context_config,
};
use crate::runtime_db::{
    task_snapshot::TaskDomainLoadRequest,
    transitions::{InputMutationExecution, PreparedInputTransition, TransitionFaultPoint},
    work_snapshot::WorkDomainLoadRequest,
};
async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "runtime-scheduler-pair",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime))
}
fn decision(boundary: &str, reentry: bool) -> SchedulerDecision {
    SchedulerDecision::new(
        if reentry {
            SchedulerDecisionKind::EmitSystemTick
        } else {
            SchedulerDecisionKind::StayIdle
        },
        if reentry { "work_available" } else { "waiting" },
    )
    .boundary(boundary)
    .model_reentry(reentry)
    .liveness_only(!reentry)
    .evidence("original decision proof")
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts WHERE family='agent_posture')",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?)
}
fn commit(
    runtime: &RuntimeHandle,
    p: &PreparedInputTransition,
) -> Result<InputMutationExecution<TransitionCommit>> {
    runtime
        .runtime_db()
        .transitions_in_scope(&p.descriptor().scopes()[0])
        .commit_prepared_transition_with_work_request_and_memory_budget(
            p,
            &WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 65536,
                byte_limit: 64 * 1024 * 1024,
            },
            &runtime.inner.input_evidence_budget,
            &TaskDomainLoadRequest {
                capture: crate::runtime::resident_task::TaskCaptureRequest {
                    include_active: true,
                    include_unsettled: true,
                    terminal_limit: 0,
                    required_keys: Default::default(),
                },
                query: Default::default(),
                record_limit: 65536,
                byte_limit: 64 * 1024 * 1024,
            },
        )
}
#[tokio::test]
async fn actual_scheduler_pair_is_atomic_once_current_and_original_order_with_pure_duplicate_read(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    // The original single-event prepared JSON must keep its old shape.
    let single = AuditEvent::legacy("single-before-pair", serde_json::Value::Null);
    let id = single.id.clone();
    runtime.append_runtime_audit_event(single).await?;
    let raw: String = runtime.runtime_db().connection()?.query_row(
        "SELECT prepared_json FROM input_mutation_receipts WHERE command_nonce=?1",
        [format!("runtime-audit:{id}")],
        |row| row.get(0),
    )?;
    assert!(!raw.contains("scheduler_legacy_event"));
    let old = runtime.inner.mutation_owner.capture()?.unwrap();
    let before = counts(&runtime)?;
    let command = decision("original-pair", false);
    assert!(runtime.append_owned_scheduler_decision(&command).await?);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.state, old.state);
    assert_eq!(
        root.current_reads.as_ref().unwrap().coverage().revision,
        old.current_reads.as_ref().unwrap().coverage().revision + 1
    );
    let after = counts(&runtime)?;
    assert_eq!(after, (before.0 + 2, before.1 + 2, before.2 + 1));
    let selected = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    let pair = selected
        .events
        .iter()
        .filter_map(|record| match &record.fact {
            CurrentReadFact::Event(event)
                if event.kind == "scheduler_diagnostic" || event.kind == "scheduler_decision" =>
            {
                Some(event)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(pair.len(), 2);
    assert_eq!(pair[0].kind, "scheduler_diagnostic");
    assert_eq!(pair[1].kind, "scheduler_decision");
    assert!(pair[0].event_seq < pair[1].event_seq);
    let before_duplicate = counts(&runtime)?;
    let token = root.token;
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO scheduler_duplicate_audit_forbidden; ALTER TABLE queue_entries RENAME TO scheduler_duplicate_queue_forbidden")?;
    assert!(
        !runtime.append_owned_scheduler_decision(&command).await?,
        "same signature must not reread canonical Audit/Queue or append"
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE scheduler_duplicate_queue_forbidden RENAME TO queue_entries; ALTER TABLE scheduler_duplicate_audit_forbidden RENAME TO audit_events")?;
    assert_eq!(counts(&runtime)?, before_duplicate);
    Ok(())
}
#[tokio::test]
async fn actual_scheduler_pair_keeps_boundary_alternation_and_real_reentry_reset() -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let idle = decision("idle_tick", false);
    let loop_idle = decision("run_loop_idle", false);
    let model = decision("idle_tick", true);
    let before = counts(&runtime)?;
    assert!(runtime.append_owned_scheduler_decision(&idle).await?);
    assert!(runtime.append_owned_scheduler_decision(&loop_idle).await?);
    assert!(!runtime.append_owned_scheduler_decision(&idle).await?);
    assert!(!runtime.append_owned_scheduler_decision(&loop_idle).await?);
    assert!(runtime.append_owned_scheduler_decision(&model).await?);
    assert!(
        runtime.append_owned_scheduler_decision(&idle).await?,
        "real model reentry must reset prior idle duplicate suppression"
    );
    assert!(!runtime.append_owned_scheduler_decision(&idle).await?);
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 8, before.1 + 8, before.2 + 4)
    );
    Ok(())
}
#[tokio::test]
async fn actual_scheduler_pair_whole_transaction_fault_replays_before_body_sql_and_keeps_original_ids(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let events = scheduler::scheduler_decision_events(
        runtime.instance_agent_id(),
        &decision("fault-pair", false),
    )?;
    let ids = [events[0].id.clone(), events[1].id.clone()];
    let times = [events[0].created_at, events[1].created_at];
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: runtime.instance_agent_id().into(),
        identity_incarnation: root.token.identity_incarnation,
    };
    let before = counts(&runtime)?;
    let p = PreparedInputTransition::runtime_scheduler_audit(
        scope.clone(),
        "pair-rollback-original".into(),
        events.clone(),
        root.state.clone(),
        Some(TransitionFaultPoint::AfterAuditWrites),
    )?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(commit(&runtime, &p).is_err());
    assert_eq!(counts(&runtime)?, before);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let retry = PreparedInputTransition::runtime_scheduler_audit(
        scope,
        "pair-rollback-original".into(),
        events,
        root.state.clone(),
        None,
    )?;
    assert_eq!(p.descriptor(), retry.descriptor());
    let InputMutationExecution::Committed { value, receipt } = commit(&runtime, &retry)? else {
        unreachable!()
    };
    assert_eq!(value.effects.audit_events.len(), 2);
    for (index, event) in value.effects.audit_events.iter().enumerate() {
        assert_eq!(event.id, ids[index]);
        assert_eq!(event.created_at, times[index]);
    }
    assert!(
        value.effects.agent_state.is_none()
            && value.effects.tasks.is_empty()
            && value.effects.queued_messages.is_empty()
    );
    let committed = counts(&runtime)?;
    assert_eq!(committed, (before.0 + 2, before.1 + 2, before.2 + 1));
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE audit_events RENAME TO scheduler_pair_replay_forbidden")?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = commit(&runtime, &retry)?
    else {
        unreachable!()
    };
    assert_eq!(receipt, replay);
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE scheduler_pair_replay_forbidden RENAME TO audit_events")?;
    assert_eq!(counts(&runtime)?, committed);
    drop((value, receipt, replay));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}
