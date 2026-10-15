use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadError, CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
};
use crate::runtime_db::{
    task_snapshot::TaskDomainLoadRequest,
    transitions::{InputMutationExecution, PreparedInputTransition},
    work_snapshot::WorkDomainLoadRequest,
};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "runtime-audit-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime))
}
fn request() -> CurrentReadRequest {
    CurrentReadRequest::current(64 * 1024 * 1024, 65536)
}
fn event(runtime: &RuntimeHandle, id: &str) -> AuditEvent {
    let mut event = AuditEvent::legacy("runtime_error", serde_json::json!({"exact":id}));
    event.id = id.into();
    event.created_at = runtime.now();
    event
}
fn prepared(
    runtime: &RuntimeHandle,
    nonce: &str,
    event: AuditEvent,
    fault: Option<TransitionFaultPoint>,
) -> Result<PreparedInputTransition> {
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Ready source")?;
    PreparedInputTransition::runtime_audit(
        crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: runtime.instance_agent_id().into(),
            identity_incarnation: root.token.identity_incarnation,
        },
        nonce.into(),
        event,
        root.state.clone(),
        fault,
    )
}
fn commit(
    runtime: &RuntimeHandle,
    p: &PreparedInputTransition,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let scope = &p.descriptor().scopes()[0];
    let work = WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    let tasks = TaskDomainLoadRequest {
        capture: crate::runtime::resident_task::TaskCaptureRequest {
            include_active: true,
            include_unsettled: true,
            terminal_limit: 0,
            required_keys: Default::default(),
        },
        query: Default::default(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    runtime
        .runtime_db()
        .transitions_in_scope(scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            p,
            &work,
            &runtime.inner.input_evidence_budget,
            &tasks,
        )
}
fn counts(runtime: &RuntimeHandle, id: &str) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events WHERE audit_event_id=?1),(SELECT COUNT(*) FROM current_read_members WHERE family='event' AND record_id=?1),(SELECT COUNT(*) FROM current_event_native_births WHERE event_id=?1),(SELECT COUNT(*) FROM input_mutation_receipts WHERE family='agent_posture')",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
}
#[tokio::test]
async fn original_runtime_audit_publishes_actual_current_body_without_state_task_or_queue_write(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let old = runtime.inner.mutation_owner.capture()?.unwrap();
    let state = old.state.clone();
    let before = old.current_reads.as_ref().unwrap().coverage().revision;
    let e = event(&runtime, "actual-runtime-audit-publication");
    runtime.append_runtime_audit_event(e.clone()).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.state, state);
    assert!(Arc::ptr_eq(
        root.tasks.as_ref().unwrap(),
        old.tasks.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        root.work.as_ref().unwrap(),
        old.work.as_ref().unwrap()
    ));
    let current = root.current_reads.as_ref().unwrap();
    assert_eq!(current.coverage().revision, before + 1);
    let selected = current.capture(&request())?;
    let actual = selected
        .events
        .iter()
        .find(|r| r.locator.key.id == e.id)
        .context("actual Event missing from its published source")?;
    assert!(
        matches!(&actual.fact,CurrentReadFact::Event(value) if value.event_seq>0 && value.id==e.id && value.data==e.data)
    );
    let sizes = counts(&runtime, &e.id)?;
    assert_eq!((sizes.0, sizes.1, sizes.2), (1, 1, 1));
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest(runtime.instance_agent_id())?
            .as_ref(),
        Some(state.as_ref())
    );
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO runtime_audit_events_forbidden; ALTER TABLE queue_entries RENAME TO runtime_audit_queue_forbidden")?;
    assert_eq!(
        current.capture(&request())?.events.len(),
        selected.events.len()
    );
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE runtime_audit_queue_forbidden RENAME TO queue_entries; ALTER TABLE runtime_audit_events_forbidden RENAME TO audit_events")?;
    drop((selected, root, old, state));
    Ok(())
}
#[tokio::test]
async fn original_runtime_audit_fault_rolls_back_member_birth_and_receipt_then_exact_replays_before_body_sql(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let e = event(&runtime, "runtime-audit-whole-rollback");
    let before = counts(&runtime, &e.id)?;
    let p = prepared(
        &runtime,
        "runtime-audit-whole-nonce",
        e.clone(),
        Some(TransitionFaultPoint::AfterAuditWrites),
    )?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = commit(&runtime, &p)
        .err()
        .context("explicit Audit fault did not run")?;
    assert!(format!("{error:#}").contains("fault"), "{error:#}");
    assert_eq!(counts(&runtime, &e.id)?, before);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let retry = prepared(&runtime, "runtime-audit-whole-nonce", e, None)?;
    assert_eq!(p.descriptor(), retry.descriptor());
    let InputMutationExecution::Committed { value, receipt } = commit(&runtime, &retry)? else {
        unreachable!()
    };
    assert!(
        value.effects.agent_state.is_none()
            && value.effects.tasks.is_empty()
            && value.effects.queued_messages.is_empty()
    );
    assert_eq!(value.effects.audit_events.len(), 1);
    let actual = counts(&runtime, "runtime-audit-whole-rollback")?;
    assert_eq!((actual.0, actual.1, actual.2), (1, 1, 1));
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE audit_events RENAME TO runtime_audit_replay_forbidden")?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = commit(&runtime, &retry)?
    else {
        unreachable!()
    };
    assert_eq!(replay, receipt);
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE runtime_audit_replay_forbidden RENAME TO audit_events")?;
    drop((value, receipt, replay));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}
#[tokio::test]
async fn runtime_audit_new_no_state_retired_and_raw_late_are_not_generic_cleanup_or_adoption(
) -> Result<()> {
    for mode in ["no_state", "retired", "raw_late"] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let e = event(&runtime, &format!("runtime-audit-negative-{mode}"));
        let p = prepared(&runtime, &format!("audit-negative-{mode}"), e.clone(), None)?;
        let c = runtime.runtime_db().connection()?;
        let (identity,state):(String,String)=c.query_row("SELECT (SELECT payload_json FROM agent_identities WHERE agent_id=?1),(SELECT payload_json FROM agent_states WHERE agent_id=?1)",[runtime.instance_agent_id()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        match mode {
            "no_state" => {
                c.execute(
                    "DELETE FROM agent_states WHERE agent_id=?1",
                    [runtime.instance_agent_id()],
                )?;
            }
            "retired" => {
                c.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[runtime.instance_agent_id()])?;
                c.execute("UPDATE agent_states SET payload_json='poison-successor-State' WHERE agent_id=?1",[runtime.instance_agent_id()])?;
            }
            _ => {}
        }
        drop(c);
        if mode == "raw_late" {
            runtime.runtime_db().transaction(|tx| {
                crate::runtime_db::evidence::append_audit_event_tx(
                    tx,
                    Some(runtime.instance_agent_id()),
                    &e,
                )
                .map(|_| ())
            })?;
        }
        let before = counts(&runtime, &e.id)?;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let error = commit(&runtime, &p)
            .err()
            .with_context(|| format!("Audit mode={mode} incorrectly accepted"))?;
        match mode {
            "raw_late" => assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::NeedsPreparation)
                ),
                "mode={mode}: {error:#}"
            ),
            _ => assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::RetiredScope)
                ),
                "mode={mode}: {error:#}"
            ),
        }
        assert_eq!(counts(&runtime, &e.id)?, before);
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        let c = runtime.runtime_db().connection()?;
        c.execute(
            "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
            rusqlite::params![identity, runtime.instance_agent_id()],
        )?;
        if mode == "no_state" {
            drop(c);
            runtime.runtime_db().transaction(|tx| {
                crate::runtime_db::evidence::upsert_agent_state_tx(
                    tx,
                    &serde_json::from_str::<AgentState>(&state)?,
                )
            })?;
        } else {
            c.execute(
                "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
                rusqlite::params![state, runtime.instance_agent_id()],
            )?;
        }
    }
    Ok(())
}
#[tokio::test]
async fn qualified_runtime_audit_noop_keeps_original_epoch_and_raw_receipt_budget_precedes_json_decode(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let e = event(&runtime, "runtime-audit-qualified-noop");
    runtime.append_runtime_audit_event(e.clone()).await?;
    let p = prepared(&runtime, "runtime-audit-noop-new-nonce", e, None)?;
    let before = counts(&runtime, "runtime-audit-qualified-noop")?;
    let epoch = runtime.runtime_db().event_log_epoch()?;
    runtime.runtime_db().connection()?.execute(
        "UPDATE runtime_metadata SET value=?1 WHERE key='event_log_epoch'",
        ["x".repeat(9 * 1024 * 1024)],
    )?;
    let InputMutationExecution::Committed { value, .. } = commit(&runtime, &p)? else {
        unreachable!()
    };
    assert!(!value.applied && value.effects.audit_events.is_empty());
    let after = counts(&runtime, "runtime-audit-qualified-noop")?;
    assert_eq!((after.0, after.1, after.2), (before.0, before.1, before.2));
    runtime.runtime_db().connection()?.execute(
        "UPDATE runtime_metadata SET value=?1 WHERE key='event_log_epoch'",
        [epoch],
    )?;
    drop(value);
    runtime.runtime_db().connection()?.execute(
        "UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce=?2",
        rusqlite::params![
            "bad".repeat(3 * 1024 * 1024),
            p.descriptor().command_nonce()
        ],
    )?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = commit(&runtime, &p)
        .err()
        .context("large malformed original receipt decoded")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn original_runtime_audit_known_ack_recovers_the_actual_current_arc_and_never_reappends(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let e = event(&runtime, "runtime-audit-known-ack");
    let mut mutation = runtime.begin_mutation().await?;
    let p = prepared(
        &runtime,
        "runtime-audit-known-nonce",
        e.clone(),
        Some(TransitionFaultPoint::BeforeCacheUpdate),
    )?;
    let error = mutation
        .commit_prepared_input_transition(p)
        .await
        .err()
        .context("actual postcommit fault was not reached")?;
    drop(mutation);
    // The original post-commit wrapper reports reconciliation, not the fault's
    // Debug label. The typed gate and retained actual facts prove the boundary.
    assert!(
        matches!(
            runtime.inner.mutation_owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ),
        "actual ACK must leave its typed Known gate: {error:#}"
    );
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let saved = reservation.recovery()?.unwrap().known.unwrap();
    let crate::runtime::mutation_owner::KnownCommit::State {
        current_reads: Some(actual),
        state,
        ..
    } = &saved
    else {
        panic!("actual Audit ACK omitted CurrentRead source")
    };
    actual.validate_state(state)?;
    let actual = actual.clone();
    let before = counts(&runtime, &e.id)?;
    assert_eq!((before.0, before.1, before.2), (1, 1, 1));
    drop(reservation);
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO audit_known_body_forbidden; ALTER TABLE queue_entries RENAME TO audit_known_queue_forbidden")?;
    let recovered = runtime.begin_mutation().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(Arc::ptr_eq(root.current_reads.as_ref().unwrap(), &actual));
    assert_eq!(
        root.current_reads
            .as_ref()
            .unwrap()
            .capture(&request())?
            .events
            .iter()
            .filter(|r| r.locator.key.id == e.id)
            .count(),
        1
    );
    drop(recovered);
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_known_queue_forbidden RENAME TO queue_entries; ALTER TABLE audit_known_body_forbidden RENAME TO audit_events")?;
    assert_eq!(counts(&runtime, &e.id)?, before);
    Ok(())
}

fn assert_actual_current_event_kinds(runtime: &RuntimeHandle, kinds: &[&str]) -> Result<()> {
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Current root missing")?;
    let selected = root
        .current_reads
        .as_ref()
        .context("actual Current facts missing")?
        .capture(&request())?;
    for kind in kinds {
        let matching = selected
            .events
            .iter()
            .filter_map(|r| match &r.fact {
                CurrentReadFact::Event(e) if e.kind == *kind => Some(e),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            !matching.is_empty(),
            "actual {kind} Audit was not installed by its own ACK"
        );
        for event in matching {
            let actual:bool=runtime.runtime_db().connection()?.query_row("SELECT EXISTS(SELECT 1 FROM current_read_members m JOIN current_event_native_births b ON b.agent_id=m.agent_id AND b.identity_incarnation=m.identity_incarnation AND b.event_id=m.record_id AND b.birth_seq=m.sort_seq WHERE m.agent_id=?1 AND m.identity_incarnation=?2 AND m.family='event' AND m.record_id=?3 AND m.availability='present' AND m.sort_seq=?4)",rusqlite::params![root.state.id,root.token.identity_incarnation,event.id,event.event_seq],|r|r.get(0))?;
            assert!(actual, "{kind} missing exact accepted original sequence");
        }
    }
    Ok(())
}

#[tokio::test]
async fn submitted_runtime_audit_caller_cancellation_keeps_the_original_owned_continuation(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let e = event(&runtime, "runtime-audit-owned-caller-abort");
    let owner = runtime.begin_mutation().await?;
    let baseline = runtime.inner.mutation_tasks.lock().unwrap().handles.len();
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        let event = e.clone();
        async move { runtime.append_runtime_audit_event(event).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if runtime.inner.mutation_tasks.lock().unwrap().handles.len() > baseline {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("original Audit was not registered before waiting for the held owner")?;
    assert_eq!(
        counts(&runtime, &e.id)?.0,
        0,
        "held owner must prevent original Audit SQL"
    );
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    drop(owner);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.join_idle_owned_tasks(),
    )
    .await??;
    let before = counts(&runtime, &e.id)?;
    assert_eq!((before.0, before.1, before.2), (1, 1, 1));
    assert_actual_current_event_kinds(&runtime, &["runtime_error"])?;
    runtime.append_owned_runtime_audit_event(e).await?;
    assert_eq!(
        counts(&runtime, "runtime-audit-owned-caller-abort")?,
        before,
        "same original nonce/ID/time must exact replay without another receipt or event"
    );
    Ok(())
}

#[tokio::test]
async fn actual_trigger_create_reset_revoke_audits_follow_original_scheduling_acks() -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let before = runtime.agent_state().await?;
    let first = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
        .await?;
    let noop = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::EnqueueMessage)
        .await?;
    assert_eq!(first, noop);
    let reset = runtime.reset_external_trigger().await?;
    assert_ne!(first.external_trigger_id, reset.external_trigger_id);
    let revoked = runtime
        .revoke_external_trigger(&reset.external_trigger_id)
        .await?;
    assert_eq!(revoked.status, ExternalTriggerStatus::Revoked);
    assert_eq!(runtime.agent_state().await?, before);
    let (business,audits,messages,queue):(usize,usize,usize,usize)=runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM input_mutation_receipts WHERE family='external_trigger'),(SELECT COUNT(*) FROM audit_events WHERE agent_id=?1 AND kind IN ('external_trigger_created','external_trigger_revoked')),(SELECT COUNT(*) FROM messages WHERE agent_id=?1),(SELECT COUNT(*) FROM queue_entries WHERE agent_id=?1)",[runtime.instance_agent_id()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    assert_eq!((business, audits, messages, queue), (4, 4, 0, 0));
    assert_actual_current_event_kinds(
        &runtime,
        &["external_trigger_created", "external_trigger_revoked"],
    )?;
    Ok(())
}

#[tokio::test]
async fn actual_callback_two_phase_delivery_keeps_one_wake_and_installs_both_original_audits(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let cap = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
        .await?;
    let result = runtime
        .deliver_callback(
            &cap.external_trigger_id,
            CallbackDeliveryPayload {
                body: Some(MessageBody::Text {
                    text: "original callback wake".into(),
                }),
                content_type: None,
                correlation_id: Some("audit-callback-correlation".into()),
                causation_id: None,
            },
        )
        .await?;
    assert_eq!(result.external_trigger_id, cap.external_trigger_id);
    assert_eq!(result.disposition, CallbackIngressDisposition::Triggered);
    let state = runtime.agent_state().await?;
    assert_eq!(state.pending, 1);
    assert_eq!(runtime.inner.agent.lock().await.queue.len(), 1);
    assert_actual_current_event_kinds(&runtime, &["wake_hint_triggered", "callback_delivered"])?;
    runtime
        .revoke_external_trigger(&cap.external_trigger_id)
        .await?;
    let before = runtime.agent_state().await?;
    assert!(runtime
        .deliver_callback(
            &cap.external_trigger_id,
            CallbackDeliveryPayload {
                body: None,
                content_type: None,
                correlation_id: None,
                causation_id: None,
            }
        )
        .await
        .is_err());
    assert_eq!(runtime.agent_state().await?, before);
    assert_eq!(runtime.inner.agent.lock().await.queue.len(), 1);
    Ok(())
}

#[tokio::test]
async fn actual_timer_owned_create_fire_and_public_cancel_install_their_audits() -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let mut fired = runtime
        .schedule_timer(3_600_000, None, Some("original manual fire".into()))
        .await?;
    runtime.fire_timer_record(&mut fired).await?;
    assert_eq!(fired.status, TimerStatus::Completed);
    assert_eq!(fired.fire_count, 1);
    assert_eq!(runtime.agent_state().await?.pending, 1);
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("original cancel".into()))
        .await?;
    let cancelled = runtime.cancel_timer(&timer.id).await?;
    assert_eq!(cancelled.status, TimerStatus::Cancelled);
    assert_eq!(
        runtime.agent_state().await?.pending,
        1,
        "unrelated pending fire is retained"
    );
    assert_actual_current_event_kinds(
        &runtime,
        &["timer_created", "timer_fired", "timer_cancelled"],
    )?;
    runtime.inner.control_cancellations.cancel_local_producers();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.inner.control_cancellations.drain_retiring(),
    )
    .await??;
    Ok(())
}

async fn awake_idle_posture_source(runtime: &RuntimeHandle) -> Result<()> {
    let mut mutation = runtime.begin_mutation().await?;
    mutation.state.status = AgentStatus::AwakeIdle;
    mutation.persist_state(&runtime.inner.storage).await
}
fn posture_counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events WHERE agent_id=?1),(SELECT COUNT(*) FROM current_read_members WHERE agent_id=?1 AND family='event'),(SELECT COUNT(*) FROM current_event_native_births WHERE agent_id=?1),(SELECT COUNT(*) FROM input_mutation_receipts WHERE family='agent_posture')",[runtime.instance_agent_id()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
}
#[tokio::test]
async fn actual_sleep_posture_and_original_decision_join_one_current_revision_and_whole_ack(
) -> Result<()> {
    for idle in [false, true] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        awake_idle_posture_source(&runtime).await?;
        let old = runtime.inner.mutation_owner.capture()?.unwrap();
        let before = posture_counts(&runtime)?;
        let executor = crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(&runtime);
        if idle {
            assert!(
                executor
                    .transition_run_loop_idle_to_sleep(None)
                    .await?
                    .unwrap()
                    .posture_changed
            );
        } else {
            assert!(
                executor
                    .transition_to_sleep(
                        None,
                        crate::runtime::scheduler_executor::SleepTransitionBoundary::LifecycleSleep
                    )
                    .await?
                    .posture_changed
            );
        }
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(root.state.status, AgentStatus::Asleep);
        assert_eq!(
            root.current_reads.as_ref().unwrap().coverage().revision,
            old.current_reads.as_ref().unwrap().coverage().revision + 1
        );
        let after = posture_counts(&runtime)?;
        assert_eq!(
            after,
            (before.0 + 3, before.1 + 3, before.2 + 3, before.3 + 1),
            "original decision and actual State share one ACK"
        );
        let current = root.current_reads.as_ref().unwrap().capture(&request())?;
        let decision = current
            .events
            .iter()
            .find_map(|r| match &r.fact {
                CurrentReadFact::Event(e) if e.kind == "scheduler_posture_decision" => Some(e),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            decision.data["boundary"],
            if idle {
                "run_loop_idle"
            } else {
                "lifecycle_sleep"
            }
        );
        assert_eq!(decision.data["reason"], "sleep");
        assert_eq!(decision.data["previous_status"], "awake_idle");
        assert_eq!(decision.data["next_status"], "asleep");
        let state_event = current
            .events
            .iter()
            .find_map(|record| match &record.fact {
                CurrentReadFact::Event(event) if event.kind == "agent_state_changed" => Some(event),
                _ => None,
            })
            .context("original Sleep State notification was not accepted")?;
        assert_eq!(state_event.event_seq, decision.event_seq + 2);
        assert_eq!(
            state_event.data,
            serde_json::to_value(AgentStateChangedEvent::from_state(&root.state))?
        );
        assert!(current.events.iter().any(|r|matches!(&r.fact,CurrentReadFact::Event(e) if e.kind=="agent_state_mutated" && e.event_seq==decision.event_seq+1)),"original decision remains before its State audit sequence");
    }
    Ok(())
}

#[tokio::test]
async fn actual_sleep_decision_whole_rollback_and_known_ack_keep_the_original_state_boundary(
) -> Result<()> {
    for fault in [
        TransitionFaultPoint::AfterAuditWrites,
        TransitionFaultPoint::BeforeCacheUpdate,
    ] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        awake_idle_posture_source(&runtime).await?;
        let old = runtime.inner.mutation_owner.capture()?.unwrap();
        let before = posture_counts(&runtime)?;
        runtime.inject_next_transition_fault(fault);
        let error = crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .transition_to_sleep(
                None,
                crate::runtime::scheduler_executor::SleepTransitionBoundary::LifecycleSleep,
            )
            .await
            .err()
            .context("original Sleep fault failed to run")?;
        if fault == TransitionFaultPoint::AfterAuditWrites {
            assert_eq!(posture_counts(&runtime)?,before,"State, both audits, accepted members, native births and receipt roll back together");
            let root = runtime.inner.mutation_owner.capture()?.unwrap();
            assert!(Arc::ptr_eq(&root, &old));
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest(runtime.instance_agent_id())?
                    .as_ref(),
                Some(old.state.as_ref())
            );
        } else {
            assert!(
                matches!(
                    runtime.inner.mutation_owner.capture(),
                    Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
                ),
                "{error:#}"
            );
            let reservation = runtime.inner.mutation_owner.reserve().await?;
            let saved = reservation.recovery()?.unwrap().known.unwrap();
            let crate::runtime::mutation_owner::KnownCommit::State {
                current_reads: Some(actual),
                state,
                ..
            } = &saved
            else {
                panic!("original acknowledged Sleep lost actual Current facts")
            };
            assert_eq!(state.status, AgentStatus::Asleep);
            actual.validate_state(state)?;
            let actual = actual.clone();
            drop(reservation);
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO posture_known_audit_forbidden; ALTER TABLE queue_entries RENAME TO posture_known_queue_forbidden")?;
            let restored = runtime.begin_mutation().await?;
            let root = runtime.inner.mutation_owner.capture()?.unwrap();
            assert!(Arc::ptr_eq(root.current_reads.as_ref().unwrap(), &actual));
            assert_eq!(root.state.status, AgentStatus::Asleep);
            assert!(root.current_reads.as_ref().unwrap().capture(&request())?.events.iter().any(|r|matches!(&r.fact,CurrentReadFact::Event(e) if e.kind=="scheduler_posture_decision")));
            drop(restored);
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE posture_known_queue_forbidden RENAME TO queue_entries; ALTER TABLE posture_known_audit_forbidden RENAME TO audit_events")?;
            let after = posture_counts(&runtime)?;
            assert_eq!(
                after,
                (before.0 + 3, before.1 + 3, before.2 + 3, before.3 + 1)
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn original_posture_decision_unknown_receipt_recovers_frozen_audits_without_reexecution(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    awake_idle_posture_source(&runtime).await?;
    let before = posture_counts(&runtime)?;
    let mut mutation = runtime.begin_mutation().await?;
    let expected = mutation.last_persisted_state.clone();
    mutation.state.status = AgentStatus::Asleep;
    let mut decision = event(&runtime, "posture-original-lost-decision");
    decision.kind = "scheduler_posture_decision".into();
    decision.data = serde_json::json!({"boundary":"lifecycle_sleep","reason":"sleep","previous_status":"awake_idle","next_status":"asleep","evidence":[]});
    let mut state_audit = event(&runtime, "posture-original-lost-state");
    state_audit.kind = "agent_state_mutated".into();
    state_audit.data = serde_json::json!({"agent_id":runtime.instance_agent_id()});
    let original_time = decision.created_at;
    let original_state_audit_time = state_audit.created_at;
    let p = PreparedInputTransition::posture(
        mutation.input_evidence_scope(),
        "posture-original-lost-nonce".into(),
        original_time,
        crate::runtime_db::transitions::AgentPostureTransitionCommand {
            agent_id: runtime.instance_agent_id().into(),
            expected_incarnation: Some(runtime.instance_incarnation()),
            agent_state: crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(expected)),
                record: Box::new(mutation.state.clone()),
            },
            audit_events: vec![decision, state_audit],
            fault: None,
        },
        None,
    )?;
    let witness = crate::runtime::mutation_witness::MutationWitness::input_mutation(
        p.descriptor().command_nonce().into(),
        crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
            p.descriptor().clone(),
        ),
        Vec::new(),
    )?
    .with_input_runtime(vec![Arc::new(mutation.facts(Default::default())?)])?;
    let error = mutation
        .commit_transition_inner(p.descriptor().command_nonce().into(), Some(witness), || {
            let actual = commit(&runtime, &p)?;
            assert!(
                matches!(actual, InputMutationExecution::Committed { .. }),
                "original SQL must actually commit before its ACK is lost"
            );
            Err(anyhow!(
                "original prepared Posture ACK deliberately lost after real COMMIT"
            ))
        })
        .await
        .err()
        .context("original commit ACK was not lost")?;
    assert!(format!("{error:#}").contains("deliberately lost"));
    drop(mutation);
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let recovery = reservation.recovery()?.unwrap();
    assert!(
        recovery.known.is_none(),
        "this is an actual indeterminate ACK, not Known fault recovery"
    );
    drop(reservation);
    let after = posture_counts(&runtime)?;
    assert_eq!(
        after,
        (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 1)
    );
    let restored = runtime.begin_mutation().await?;
    assert_eq!(restored.state.status, AgentStatus::Asleep);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = root.current_reads.as_ref().unwrap().capture(&request())?;
    for (id, created_at) in [
        ("posture-original-lost-decision", original_time),
        ("posture-original-lost-state", original_state_audit_time),
    ] {
        assert!(current.events.iter().any(
            |r| matches!(&r.fact,CurrentReadFact::Event(e) if e.id==id && e.created_at==created_at)
        ));
    }
    drop(restored);
    assert_eq!(
        posture_counts(&runtime)?,
        after,
        "fresh original receipt must not repeat either append or the State command"
    );
    assert!(runtime
        .inner
        .mutation_owner
        .reserve()
        .await?
        .recovery()?
        .is_none());
    Ok(())
}

#[tokio::test]
async fn original_lifecycle_noop_audits_without_manufacturing_a_state_mutation() -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let executor = crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(&runtime);
    executor.apply_control(ControlAction::Stop).await?;
    let before = runtime.agent_state().await?;
    let counts = posture_counts(&runtime)?;
    executor.apply_control(ControlAction::Stop).await?;
    assert_eq!(runtime.agent_state().await?, before);
    let after = posture_counts(&runtime)?;
    assert_eq!(
        after,
        (counts.0 + 1, counts.1 + 1, counts.2 + 1, counts.3 + 1),
        "no-op adds only its original decision Audit and original whole receipt"
    );
    Ok(())
}

async fn actual_stale_scheduler_source(
    runtime: &RuntimeHandle,
    projected: Option<&str>,
) -> Result<()> {
    let mut mutation = runtime.begin_mutation().await?;
    mutation.state.status = AgentStatus::AwakeRunning;
    mutation.state.current_run_id = projected.map(str::to_owned);
    mutation.persist_state(&runtime.inner.storage).await
}

#[tokio::test]
async fn actual_stale_scheduler_recovery_qualifies_original_event_and_once_whole_ack() -> Result<()>
{
    use crate::runtime::scheduler_executor::{
        SchedulerDecisionExecutor, StaleRunProjectionBoundary, StaleRunProjectionReconciliation,
    };
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    actual_stale_scheduler_source(&runtime, Some("original-missing-run")).await?;
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let before = reservation.capture_ready_snapshot()?.unwrap();
    let session =
        runtime.register_current_event_birth_session_from_snapshot(&reservation, &before)?;
    drop(reservation);
    let counts = posture_counts(&runtime)?;
    assert_eq!(
        SchedulerDecisionExecutor::new(&runtime)
            .reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll)
            .await?,
        StaleRunProjectionReconciliation::Recovered
    );
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.state.status, AgentStatus::AwakeIdle);
    assert!(root.state.current_run_id.is_none());
    assert!(runtime.current_run_abort_token().await.is_none());
    assert_eq!(
        root.current_reads.as_ref().unwrap().coverage().revision,
        before.current_reads.as_ref().unwrap().coverage().revision + 1
    );
    assert_eq!(
        posture_counts(&runtime)?,
        (counts.0 + 1, counts.1 + 1, counts.2 + 1, counts.3 + 1)
    );
    let born = runtime.capture_current_event_birth_session_from_snapshot(&session, &root)?;
    assert_eq!(born.selected.events.len(), 1);
    let CurrentReadFact::Event(event) = &born.selected.events[0].fact else {
        panic!("original Event body absent")
    };
    assert_eq!(event.kind, "scheduler_stale_run_projection_recovered");
    assert_eq!(event.data["previous_run_id"], "original-missing-run");
    assert_eq!(event.data["handle_disposition"], "missing");
    let receipt: (String, String) = runtime.runtime_db().connection()?.query_row(
        "SELECT command_nonce,family FROM input_mutation_receipts WHERE command_nonce=?1",
        [&event.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(receipt, (event.id.clone(), "agent_posture".into()));
    let after = posture_counts(&runtime)?;
    assert_eq!(
        SchedulerDecisionExecutor::new(&runtime)
            .reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll)
            .await?,
        StaleRunProjectionReconciliation::NoChange
    );
    assert_eq!(
        posture_counts(&runtime)?,
        after,
        "a recovered projection cannot repeat its original Event/State command"
    );
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO scheduler_event_forbidden; ALTER TABLE queue_entries RENAME TO scheduler_queue_forbidden")?;
    let selected = root.current_reads.as_ref().unwrap().capture(&request())?;
    assert!(selected.events.iter().any(|r|matches!(&r.fact,CurrentReadFact::Event(actual) if actual.id==event.id && actual.event_seq==event.event_seq)));
    Ok(())
}

#[tokio::test]
async fn held_scheduler_mismatch_audit_keeps_exact_state_and_abort_handle_without_nested_owner(
) -> Result<()> {
    use crate::runtime::scheduler_executor::{
        SchedulerDecisionExecutor, StaleRunProjectionBoundary,
    };
    for projected in [Some("original-projected-run"), None] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        actual_stale_scheduler_source(&runtime, projected).await?;
        let token = CancellationToken::new();
        {
            let mut mutation = runtime.begin_mutation().await?;
            mutation.current_run_abort = Some(CurrentRunAbortHandle {
                run_id: "original-owned-run".into(),
                token: token.clone(),
                reason: Arc::new(StdMutex::new("original-reason".into())),
            });
            mutation.install_transient().await?;
        }
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        let counts = posture_counts(&runtime)?;
        runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCommit);
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            SchedulerDecisionExecutor::new(&runtime)
                .reconcile_stale_run_projection(StaleRunProjectionBoundary::WakeHintSubmission),
        )
        .await
        .context("held scheduler diagnostic attempted a nested owner")?
        .err()
        .context("mismatched original run must remain an error")?;
        assert!(format!("{error:#}").contains("scheduler run projection mismatch"));
        assert_eq!(
            runtime.take_transition_fault(),
            Some(TransitionFaultPoint::BeforeCommit),
            "diagnostic append does not consume another business command's fault"
        );
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(root.state, before.state);
        assert!(!token.is_cancelled());
        let handle = runtime
            .current_run_abort_token()
            .await
            .context("original handle disappeared")?;
        assert_eq!(handle.run_id, "original-owned-run");
        assert!(!handle.token.is_cancelled());
        assert_eq!(
            posture_counts(&runtime)?,
            (counts.0 + 1, counts.1 + 1, counts.2 + 1, counts.3 + 1)
        );
        assert_eq!(
            root.current_reads.as_ref().unwrap().coverage().revision,
            before.current_reads.as_ref().unwrap().coverage().revision + 1
        );
        let selected = root.current_reads.as_ref().unwrap().capture(&request())?;
        let event = selected
            .events
            .iter()
            .find_map(|r| match &r.fact {
                CurrentReadFact::Event(e)
                    if e.kind == "scheduler_run_projection_invariant_violation" =>
                {
                    Some(e)
                }
                _ => None,
            })
            .context("actual diagnostic not in original Current ACK")?;
        assert_eq!(
            event.data["projected_run_id"],
            serde_json::to_value(projected)?
        );
        assert_eq!(event.data["handle_run_id"], "original-owned-run");
        assert_eq!(event.data["boundary"], "wake_hint_submission");
    }
    Ok(())
}

#[tokio::test]
async fn actual_stale_scheduler_whole_ack_rollback_and_known_recovery_preserve_current(
) -> Result<()> {
    use crate::runtime::scheduler_executor::{
        SchedulerDecisionExecutor, StaleRunProjectionBoundary,
    };
    for fault in [
        TransitionFaultPoint::AfterAuditWrites,
        TransitionFaultPoint::BeforeCacheUpdate,
    ] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        actual_stale_scheduler_source(&runtime, Some("original-stale-fault-run")).await?;
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        let counts = posture_counts(&runtime)?;
        runtime.inject_next_transition_fault(fault);
        let error = SchedulerDecisionExecutor::new(&runtime)
            .reconcile_stale_run_projection(StaleRunProjectionBoundary::RunLoopPoll)
            .await
            .err()
            .context("actual stale command must reach its original fault")?;
        if fault == TransitionFaultPoint::AfterAuditWrites {
            assert!(
                format!("{error:#}").contains("AfterAuditWrites"),
                "{error:#}"
            );
            assert_eq!(posture_counts(&runtime)?, counts);
            assert!(Arc::ptr_eq(
                &runtime.inner.mutation_owner.capture()?.unwrap(),
                &before
            ));
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest(runtime.instance_agent_id())?
                    .as_ref(),
                Some(before.state.as_ref())
            );
        } else {
            assert!(
                matches!(
                    runtime.inner.mutation_owner.capture(),
                    Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
                ),
                "{error:#}"
            );
            let reservation = runtime.inner.mutation_owner.reserve().await?;
            let saved = reservation.recovery()?.unwrap().known.unwrap();
            let crate::runtime::mutation_owner::KnownCommit::State {
                current_reads: Some(actual),
                state,
                ..
            } = &saved
            else {
                panic!("actual stale ACK omitted Current facts")
            };
            assert_eq!(state.status, AgentStatus::AwakeIdle);
            actual.validate_state(state)?;
            let actual = actual.clone();
            drop(reservation);
            let after = posture_counts(&runtime)?;
            assert_eq!(
                after,
                (counts.0 + 1, counts.1 + 1, counts.2 + 1, counts.3 + 1)
            );
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO stale_known_audit_forbidden; ALTER TABLE queue_entries RENAME TO stale_known_queue_forbidden")?;
            let recovered = runtime.begin_mutation().await?;
            let root = runtime.inner.mutation_owner.capture()?.unwrap();
            assert!(Arc::ptr_eq(root.current_reads.as_ref().unwrap(), &actual));
            assert_eq!(root.state.status, AgentStatus::AwakeIdle);
            assert!(root.current_reads.as_ref().unwrap().capture(&request())?.events.iter().any(|r|matches!(&r.fact,CurrentReadFact::Event(e) if e.kind=="scheduler_stale_run_projection_recovered")));
            drop(recovered);
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE stale_known_queue_forbidden RENAME TO queue_entries; ALTER TABLE stale_known_audit_forbidden RENAME TO audit_events")?;
            assert_eq!(posture_counts(&runtime)?, after);
        }
    }
    Ok(())
}

#[tokio::test]
async fn original_continuation_audits_keep_current_source_valid_for_the_next_unchanged_state_ack(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let message = runtime
        .enqueue(MessageEnvelope::new(
            runtime.instance_agent_id(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "original continuation source".into(),
            },
        ))
        .await?;
    let trigger = crate::runtime::continuation::ContinuationTrigger::from_message(&message, None)
        .context("actual source did not classify")?;
    let prior = crate::types::ClosureDecision {
        outcome: crate::types::ClosureOutcome::Waiting,
        waiting_reason: Some(crate::types::WaitingReason::AwaitingOperatorInput),
        work_signal: None,
        runtime_posture: crate::types::RuntimePosture::Sleeping,
        evidence: Vec::new(),
    };
    let resolution = crate::runtime::continuation::resolve_continuation(&prior, &trigger);
    let old = runtime.inner.mutation_owner.capture()?.unwrap();
    let before = posture_counts(&runtime)?;
    runtime
        .record_continuation_trigger_received(&message, &trigger, &prior)
        .await?;
    runtime
        .record_continuation_resolution_event(&message, &resolution)
        .await?;
    let source = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(source.state, old.state);
    assert_eq!(
        source.current_reads.as_ref().unwrap().coverage().revision,
        old.current_reads.as_ref().unwrap().coverage().revision + 2
    );
    assert_eq!(
        posture_counts(&runtime)?,
        (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
    );
    let fresh: String = runtime.runtime_db().connection()?.query_row(
        "SELECT freshness FROM current_read_current WHERE agent_id=?1 AND identity_incarnation=?2",
        rusqlite::params![runtime.instance_agent_id(), runtime.instance_incarnation()],
        |row| row.get(0),
    )?;
    assert_eq!(
        fresh, "current",
        "accepted Audit cannot leave a stale State-only source"
    );
    let current = source.current_reads.as_ref().unwrap().capture(&request())?;
    for kind in ["continuation_trigger_received", "continuation_resolved"] {
        let event = current
            .events
            .iter()
            .find_map(|r| match &r.fact {
                CurrentReadFact::Event(e) if e.kind == kind => Some(e),
                _ => None,
            })
            .context("original continuation Event missing from its actual ACK")?;
        assert_eq!(event.data["message_id"], message.id);
        let birth: bool = runtime.runtime_db().connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM current_event_native_births WHERE agent_id=?1 AND identity_incarnation=?2 AND event_id=?3 AND birth_seq=?4)",
            rusqlite::params![runtime.instance_agent_id(),runtime.instance_incarnation(),event.id,event.event_seq],|row|row.get(0))?;
        assert!(birth);
    }
    let mut mutation = runtime.begin_mutation().await?;
    let expected = mutation.last_persisted_state.clone();
    let prepared = PreparedInputTransition::posture(
        mutation.input_evidence_scope(),
        "continuation-original-empty-footprint".into(),
        runtime.now(),
        crate::runtime_db::transitions::AgentPostureTransitionCommand {
            agent_id: runtime.instance_agent_id().into(),
            expected_incarnation: Some(runtime.instance_incarnation()),
            agent_state: crate::runtime_db::transitions::AgentStateMutation {
                expected: Some(Box::new(expected.clone())),
                record: Box::new(expected),
            },
            audit_events: Vec::new(),
            fault: None,
        },
        None,
    )?;
    // The original source fastpath must remain pure; do not rebuild Current to
    // forgive an unmigrated producer's real dirty marker.
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO continuation_state_audit_forbidden; ALTER TABLE queue_entries RENAME TO continuation_state_queue_forbidden")?;
    mutation.commit_prepared_input_transition(prepared).await?;
    drop(mutation);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.state, source.state);
    assert!(root.current_reads.as_ref().unwrap().capture(&request())?.events.iter().any(|r|matches!(&r.fact,CurrentReadFact::Event(e) if e.kind=="continuation_trigger_received")));
    Ok(())
}

#[tokio::test]
async fn actual_queue_claim_state_notification_joins_original_current_and_rollback() -> Result<()> {
    for fail in [true, false] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let message = MessageEnvelope::new(
            runtime.instance_agent_id(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "actual original claim".into(),
            },
        );
        let admitted = runtime.enqueue(message).await?;
        // Poll explicitly acquires this exact source before the Claim business
        // transaction; establish that same preparation before its rollback base.
        {
            use crate::runtime::resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey};
            let mut owner = runtime.begin_mutation().await?;
            owner
                .prepare_owner_work_request(&crate::runtime::work_reads::live_work_request())
                .await?;
            let read = owner
                .prepare_scheduler_read(
                    Some(&admitted),
                    std::collections::BTreeSet::from([InputEvidenceKey::new(
                        InputEvidenceFamily::Message,
                        &admitted.id,
                    )]),
                    std::collections::BTreeSet::from([admitted.id.clone()]),
                    runtime.now(),
                )
                .await?;
            drop(read);
        }
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        let before_receipts = runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts",
            [],
            |r| r.get::<_, usize>(0),
        )?;
        if fail {
            runtime.inject_next_transition_fault(TransitionFaultPoint::AfterAuditWrites);
        }
        let result = crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await;
        if fail {
            assert!(result.is_err());
            assert!(Arc::ptr_eq(
                &before,
                &runtime.inner.mutation_owner.capture()?.unwrap()
            ));
            assert_eq!(
                runtime.runtime_db().connection()?.query_row(
                    "SELECT COUNT(*) FROM input_mutation_receipts",
                    [],
                    |r| r.get::<_, usize>(0),
                )?,
                before_receipts
            );
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest(&before.state.id)?
                    .as_ref(),
                Some(before.state.as_ref())
            );
            assert_eq!(
                runtime.runtime_db().connection()?.query_row(
                    "SELECT COUNT(*) FROM audit_events WHERE kind='agent_state_changed'",
                    [],
                    |r| r.get::<_, usize>(0)
                )?,
                0
            );
            assert_eq!(
                runtime
                    .runtime_db()
                    .queue_entries()
                    .latest(&admitted.id)?
                    .unwrap()
                    .status,
                QueueEntryStatus::Queued
            );
        } else {
            let crate::runtime::scheduler_executor::RunLoopPoll::Message(scheduled) = result?
            else {
                anyhow::bail!("original admitted claim did not produce its Message");
            };
            let root = runtime.inner.mutation_owner.capture()?.unwrap();
            assert_eq!(
                root.current_reads.as_ref().unwrap().coverage().revision,
                before.current_reads.as_ref().unwrap().coverage().revision + 1
            );
            let current = root.current_reads.as_ref().unwrap().capture(&request())?;
            let events = current
                .events
                .iter()
                .filter_map(|record| match &record.fact {
                    CurrentReadFact::Event(event) if event.kind == "agent_state_changed" => {
                        Some(event)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(events.len(), 1);
            assert_eq!(
                events[0].data,
                serde_json::to_value(AgentStateChangedEvent::from_state(&scheduled.running_state))?
            );
            assert!(events[0].data.get("working_memory").is_none());
            assert_eq!(runtime.runtime_db().connection()?.query_row(
                "SELECT COUNT(*) FROM current_event_native_births WHERE agent_id=?1 AND identity_incarnation=?2 AND event_id=?3",
                rusqlite::params![root.state.id,root.token.identity_incarnation,events[0].id], |r|r.get::<_,usize>(0))?,1);
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE audit_events RENAME TO original_claim_events_forbidden",
            )?;
            assert_eq!(
                root.current_reads
                    .as_ref()
                    .unwrap()
                    .capture(&request())?
                    .events
                    .len(),
                current.events.len()
            );
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE original_claim_events_forbidden RENAME TO audit_events",
            )?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn original_state_notification_checks_borrowed_native_fields_before_first_payload_copy(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let mut state = root.state.as_ref().clone();
    state.current_run_id = Some("original-run".into());
    state.attached_workspaces = vec!["original-workspace".into()];
    state.working_memory.archived_episode_count = 1234;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let read = runtime.prepare_original_state_changed_audit(&state, &[])?;
    assert_eq!(
        read.event().data,
        serde_json::to_value(AgentStateChangedEvent::from_state(&state))?
    );
    assert!(read.event().data.get("working_memory").is_none());
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop(read);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    state.attached_workspaces = (0..20_000)
        .map(|i| format!("native-workspace-{i}"))
        .collect();
    let error = runtime
        .prepare_original_state_changed_audit(&state, &[])
        .err()
        .context("oversized State notification copied before its gate")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(Arc::ptr_eq(
        &root,
        &runtime.inner.mutation_owner.capture()?.unwrap()
    ));
    Ok(())
}

#[tokio::test]
async fn actual_started_turn_state_notification_shares_original_ack_rollback_and_known_recovery(
) -> Result<()> {
    for fault in [
        None,
        Some(TransitionFaultPoint::AfterAuditWrites),
        Some(TransitionFaultPoint::BeforeCacheUpdate),
    ] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        let counts = posture_counts(&runtime)?;
        if let Some(fault) = fault {
            runtime.inject_next_transition_fault(fault);
        }
        let result = runtime.begin_interactive_turn(None, None, None).await;
        if fault == Some(TransitionFaultPoint::AfterAuditWrites) {
            assert!(result.is_err());
            assert_eq!(posture_counts(&runtime)?, counts);
            assert!(Arc::ptr_eq(
                &before,
                &runtime.inner.mutation_owner.capture()?.unwrap()
            ));
            assert_eq!(
                runtime.runtime_db().connection()?.query_row(
                    "SELECT COUNT(*) FROM turn_records WHERE agent_id=?1",
                    [runtime.instance_agent_id()],
                    |r| r.get::<_, usize>(0)
                )?,
                0
            );
            continue;
        }
        let acknowledged = if fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            assert!(result.is_err());
            assert!(matches!(
                runtime.inner.mutation_owner.capture(),
                Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
            ));
            let reservation = runtime.inner.mutation_owner.reserve().await?;
            let known = reservation
                .recovery()?
                .context("original StartedTurn recovery missing")?
                .known
                .context("real StartedTurn ACK was not Known")?;
            let crate::runtime::mutation_owner::KnownCommit::State {
                current_reads: Some(actual),
                ..
            } = known
            else {
                anyhow::bail!("original StartedTurn omitted actual Current facts");
            };
            drop(reservation);
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO original_started_audit_forbidden; ALTER TABLE queue_entries RENAME TO original_started_queue_forbidden")?;
            let owner = runtime.begin_mutation().await?;
            let root = runtime.inner.mutation_owner.capture()?.unwrap();
            assert!(Arc::ptr_eq(root.current_reads.as_ref().unwrap(), &actual));
            drop(owner);
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE original_started_queue_forbidden RENAME TO queue_entries; ALTER TABLE original_started_audit_forbidden RENAME TO audit_events")?;
            root
        } else {
            result?;
            runtime.inner.mutation_owner.capture()?.unwrap()
        };
        assert_eq!(
            posture_counts(&runtime)?,
            (counts.0 + 2, counts.1 + 2, counts.2 + 2, counts.3 + 1)
        );
        assert_eq!(
            acknowledged
                .current_reads
                .as_ref()
                .unwrap()
                .coverage()
                .revision,
            before.current_reads.as_ref().unwrap().coverage().revision + 1
        );
        let selected = acknowledged
            .current_reads
            .as_ref()
            .unwrap()
            .capture(&request())?;
        let changed = selected
            .events
            .iter()
            .find_map(|r| match &r.fact {
                CurrentReadFact::Event(e) if e.kind == "agent_state_changed" => Some(e),
                _ => None,
            })
            .context("actual StartedTurn notification is missing")?;
        assert_eq!(
            changed.data,
            serde_json::to_value(AgentStateChangedEvent::from_state(&acknowledged.state))?
        );
        let mutated = selected
            .events
            .iter()
            .find_map(|r| match &r.fact {
                CurrentReadFact::Event(e) if e.kind == "agent_state_mutated" => Some(e),
                _ => None,
            })
            .context("actual StartedTurn mutation audit is missing")?;
        assert_eq!(changed.event_seq, mutated.event_seq + 1);
        assert!(acknowledged.state.current_turn_id.is_some());
        let after = posture_counts(&runtime)?;
        drop(runtime.begin_mutation().await?);
        assert_eq!(
            posture_counts(&runtime)?,
            after,
            "reconciliation must not append a second State notification"
        );
    }
    Ok(())
}

#[tokio::test]
async fn original_dispatch_notifications_freeze_lightweight_state_source_and_replay_in_original_order(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let mut state = root.state.as_ref().clone();
    state.current_turn_id = Some("original-replay-turn".into());
    state.current_run_id = Some("original-run".into());
    let message = MessageEnvelope::new(
        runtime.instance_agent_id(),
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original source".into(),
        },
    );
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let frozen = runtime.prepare_original_dispatch_audits(
        &state,
        Some(&message),
        Some(("prior-original-turn", None)),
    )?;
    let events = frozen.events();
    assert_eq!(
        events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["agent_state_changed", "turn_started", "turn_replay_started"]
    );
    assert_eq!(
        events[0].data,
        serde_json::to_value(AgentStateChangedEvent::from_state(&state))?
    );
    assert_eq!(
        events[1].data,
        serde_json::json!({
            "agent_id":message.agent_id,"message_id":message.id,
            "turn_id":state.current_turn_id,"message_kind":message.kind,
            "run_id":state.current_run_id,"turn_index":state.turn_index,
        })
    );
    assert_eq!(
        events[2].data,
        serde_json::json!({
            "agent_id":message.agent_id,"message_id":message.id,
            "source_turn_id":"prior-original-turn","replay_turn_id":state.current_turn_id,
            "reason":"interrupted_queue_claim_reentry","source_work_item_id":null,"prior_terminal":null,
        })
    );
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let ids = events
        .iter()
        .map(|e| e.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 3);
    drop(frozen);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(
        Arc::ptr_eq(&root, &runtime.inner.mutation_owner.capture()?.unwrap()),
        "material preparation creates no business authority or new Ready"
    );
    Ok(())
}

#[tokio::test]
async fn actual_terminal_state_notification_follows_memory_and_keeps_whole_rollback_and_known_ack(
) -> Result<()> {
    use crate::runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
    for mode in ["normal", "late_failure", "known"] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        // A real old snapshot makes the original Memory planner finalize an
        // ActiveWorkSwitched episode when this dispatch derives empty Work.
        {
            let mut setup = runtime.begin_mutation().await?;
            setup.state.working_memory.current_working_memory.objective =
                Some("actual prior terminal episode objective".into());
            setup.persist_state(&runtime.inner.storage).await?;
        }
        let message = runtime
            .enqueue(MessageEnvelope::new(
                runtime.instance_agent_id(),
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "original terminal notification".into(),
                },
            ))
            .await?;
        let RunLoopPoll::Message(scheduled) =
            SchedulerDecisionExecutor::new(&runtime).poll().await?
        else {
            anyhow::bail!("actual original terminal source was not claimed");
        };
        runtime
            .begin_dispatch_turn(
                &scheduled.message,
                None,
                None,
                scheduled
                    .dispatch_plan
                    .execution_admission_provenance
                    .clone(),
                &scheduled.dispatch_plan.prior_closure,
            )
            .await?;
        let terminal = runtime
            .build_reducer_only_terminal_transition("original terminal state notification", false)
            .await?;
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(
            before
                .state
                .working_memory
                .current_working_memory
                .objective
                .as_deref(),
            Some("actual prior terminal episode objective")
        );
        let original_queue = runtime
            .runtime_db()
            .queue_entries()
            .latest(&message.id)?
            .unwrap();
        assert_eq!(original_queue.status, QueueEntryStatus::Dequeued);
        let baseline: (usize,usize,usize,usize,u64)=runtime.runtime_db().connection()?.query_row(
            "SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_read_members),(SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts),COALESCE((SELECT MAX(event_seq) FROM audit_events),0)",[],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let checkpoint_before: String=runtime.runtime_db().connection()?.query_row(
            "SELECT payload_json FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND turn_id=?2 AND state='started'",
            rusqlite::params![before.state.id,terminal.turn_record.turn_id],|r|r.get(0))?;
        if mode == "late_failure" {
            // This fails after the actual final notification row and Memory
            // epilogue, when their original Current member is finalized.
            runtime.runtime_db().connection()?.execute_batch(
                "CREATE TRIGGER fail_original_terminal_state_member BEFORE INSERT ON current_read_members WHEN NEW.family='event' AND EXISTS(SELECT 1 FROM audit_events e WHERE e.audit_event_id=NEW.record_id AND e.kind='agent_state_changed') BEGIN SELECT RAISE(ABORT,'original terminal member fault'); END;")?;
        } else if mode == "known" {
            runtime.inject_next_terminal_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
        }
        let result = runtime
            .commit_queue_terminal_settlement_with_evidence(
                QueueEntryRecord {
                    status: QueueEntryStatus::Processed,
                    updated_at: runtime.now(),
                    ..original_queue.clone()
                },
                vec![AuditEvent::legacy(
                    "queue_entry_settled",
                    serde_json::json!({"message_id":message.id,"status":"processed"}),
                )],
                true,
                Some(&terminal),
                None,
                Vec::new(),
                Vec::new(),
            )
            .await;
        if mode == "late_failure" {
            assert!(
                result.is_err(),
                "late actual Current finalizer fault must roll back the whole terminal ACK"
            );
            runtime
                .runtime_db()
                .connection()?
                .execute_batch("DROP TRIGGER fail_original_terminal_state_member")?;
            assert!(Arc::ptr_eq(
                &before,
                &runtime.inner.mutation_owner.capture()?.unwrap()
            ));
            let after: (usize,usize,usize,usize,u64)=runtime.runtime_db().connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_read_members),(SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts),COALESCE((SELECT MAX(event_seq) FROM audit_events),0)",[],
                |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
            assert_eq!(after, baseline);
            assert_eq!(
                runtime
                    .runtime_db()
                    .queue_entries()
                    .latest(&message.id)?
                    .as_ref(),
                Some(&original_queue)
            );
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest(&before.state.id)?
                    .as_ref(),
                Some(before.state.as_ref())
            );
            assert_eq!(runtime.runtime_db().connection()?.query_row(
                "SELECT payload_json FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND turn_id=?2 AND state='started'",
                rusqlite::params![before.state.id,terminal.turn_record.turn_id],|r|r.get::<_,String>(0))?,checkpoint_before);
            continue;
        }
        let acknowledged = if mode == "known" {
            assert!(result.is_err());
            assert!(matches!(
                runtime.inner.mutation_owner.capture(),
                Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
            ));
            let reservation = runtime.inner.mutation_owner.reserve().await?;
            let known = reservation
                .recovery()?
                .context("original terminal recovery missing")?
                .known
                .context("terminal ACK was not Known")?;
            let crate::runtime::mutation_owner::KnownCommit::State {
                current_reads: Some(actual),
                ..
            } = known
            else {
                anyhow::bail!("actual terminal ACK omitted original Current facts");
            };
            drop(reservation);
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO original_terminal_audit_forbidden; ALTER TABLE queue_entries RENAME TO original_terminal_queue_forbidden")?;
            let owner = runtime.begin_mutation().await?;
            let root = runtime.inner.mutation_owner.capture()?.unwrap();
            assert!(Arc::ptr_eq(root.current_reads.as_ref().unwrap(), &actual));
            drop(owner);
            runtime.runtime_db().connection()?.execute_batch("ALTER TABLE original_terminal_queue_forbidden RENAME TO queue_entries; ALTER TABLE original_terminal_audit_forbidden RENAME TO audit_events")?;
            root
        } else {
            assert!(result?);
            runtime.inner.mutation_owner.capture()?.unwrap()
        };
        assert_eq!(
            acknowledged
                .current_reads
                .as_ref()
                .unwrap()
                .coverage()
                .revision,
            before.current_reads.as_ref().unwrap().coverage().revision + 1
        );
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(&message.id)?
                .unwrap()
                .status,
            QueueEntryStatus::Processed
        );
        assert_eq!(runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND turn_id=?2 AND state='done'",rusqlite::params![before.state.id,terminal.turn_record.turn_id],|r|r.get::<_,usize>(0))?,1);
        assert_eq!(
            runtime.runtime_db().connection()?.query_row(
                "SELECT COUNT(*) FROM input_mutation_receipts",
                [],
                |r| r.get::<_, usize>(0)
            )?,
            baseline.3 + 1
        );
        let selected = acknowledged
            .current_reads
            .as_ref()
            .unwrap()
            .capture(&request())?;
        let changed = selected
            .events
            .iter()
            .filter_map(|r| match &r.fact {
                CurrentReadFact::Event(e)
                    if e.kind == "agent_state_changed" && e.event_seq > baseline.4 =>
                {
                    Some(e)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(changed.len(), 1);
        let changed = changed[0];
        assert_eq!(
            changed.data,
            serde_json::to_value(AgentStateChangedEvent::from_state(&acknowledged.state))?
        );
        assert!(changed.data.get("working_memory").is_none());
        assert_eq!(
            runtime
                .runtime_db()
                .audit_events()
                .max_event_seq(Some(&before.state.id))?,
            changed.event_seq,
            "terminal notification must remain after the actual Memory epilogue"
        );
        assert!(selected.events.iter().any(|r|matches!(&r.fact,CurrentReadFact::Event(e) if e.kind=="episode_memory_finalized" && e.event_seq>baseline.4 && e.event_seq<changed.event_seq)));
        let finalized = selected
            .events
            .iter()
            .filter_map(|record| match &record.fact {
                CurrentReadFact::Event(event)
                    if event.kind == "episode_memory_finalized" && event.event_seq > baseline.4 =>
                {
                    Some(event)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(finalized.len(), 1);
        assert_eq!(finalized[0].data["boundary_reason"], "active_work_switched");
        assert!(finalized[0].event_seq < changed.event_seq);
        assert_eq!(
            acknowledged.state.working_memory.archived_episode_count,
            before.state.working_memory.archived_episode_count + 1
        );
        assert!(acknowledged
            .state
            .working_memory
            .current_working_memory
            .objective
            .is_none());
        assert_eq!(runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM current_event_native_births WHERE agent_id=?1 AND identity_incarnation=?2 AND event_id=?3 AND birth_seq=?4",rusqlite::params![before.state.id,before.token.identity_incarnation,changed.id,changed.event_seq],|r|r.get::<_,usize>(0))?,1);
        let after_count = runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM audit_events",
            [],
            |r| r.get::<_, usize>(0),
        )?;
        drop(runtime.begin_mutation().await?);
        assert_eq!(
            runtime.runtime_db().connection()?.query_row(
                "SELECT COUNT(*) FROM audit_events",
                [],
                |r| r.get::<_, usize>(0)
            )?,
            after_count,
            "original terminal reconciliation must not mint a new notification"
        );
        drop(scheduled);
    }
    Ok(())
}

#[tokio::test]
async fn frozen_terminal_notification_preserves_legacy_queue_digest_and_rejects_wrong_business_shape(
) -> Result<()> {
    use crate::runtime_db::transitions::{
        AgentStateMutation, PreparedQueueTransition, QueueMutation, QueueOperation,
        QueueTransitionCommand,
    };
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    runtime.begin_interactive_turn(None, None, None).await?;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let terminal = runtime
        .build_reducer_only_terminal_transition("frozen terminal notification shape", false)
        .await?;
    let mut state = before.state.as_ref().clone();
    state.current_run_id = None;
    state.current_turn_id = Some(terminal.terminal.turn_id.clone());
    state.last_turn_terminal = Some(terminal.terminal.clone());
    let command = QueueTransitionCommand {
        agent_id: state.id.clone(),
        operation: QueueOperation::Settle,
        mutation: QueueMutation::Upsert(QueueEntryRecord {
            agent_id: state.id.clone(),
            message_id: "material-only-terminal-address".into(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Processed,
            created_at: runtime.now(),
            updated_at: runtime.now(),
        }),
        scheduler_claim_work_item: None,
        agent_state: Some(AgentStateMutation {
            expected: Some(Box::new(before.state.as_ref().clone())),
            record: Box::new(state.clone()),
        }),
        message_evidence: Vec::new(),
        transcript_entries: Vec::new(),
        turn_record: Some(terminal.turn_record.clone()),
        audit_events: Vec::new(),
        notify_scheduler: false,
        fault: None,
        brief_evidence: Vec::new(),
    };
    let audit = runtime.prepare_original_state_changed_audit(&state, &[])?;
    let plain = PreparedQueueTransition::new(command.clone());
    assert!(serde_json::to_value(&plain)?
        .get("original_terminal_state_audit")
        .is_none());
    let frozen = PreparedQueueTransition::new(command.clone())
        .with_original_terminal_state_audit(audit.event().clone())?;
    assert_eq!(
        serde_json::to_value(&frozen)?["original_terminal_state_audit"]["id"],
        audit.event().id
    );
    assert_ne!(
        crate::runtime_db::input_mutation_receipts::canonical_command_digest(&plain)?,
        crate::runtime_db::input_mutation_receipts::canonical_command_digest(&frozen)?
    );
    let mut other = audit.event().clone();
    other.id.push_str("-different-original");
    let different =
        PreparedQueueTransition::new(command.clone()).with_original_terminal_state_audit(other)?;
    assert_ne!(
        crate::runtime_db::input_mutation_receipts::canonical_command_digest(&different)?,
        crate::runtime_db::input_mutation_receipts::canonical_command_digest(&frozen)?
    );
    assert!(frozen
        .with_original_terminal_state_audit(audit.event().clone())
        .is_err());
    for mode in [
        "no_state",
        "no_turn",
        "claim",
        "wrong_payload",
        "presequenced",
    ] {
        let mut command = command.clone();
        let mut event = audit.event().clone();
        match mode {
            "no_state" => command.agent_state = None,
            "no_turn" => command.turn_record = None,
            "claim" => command.operation = QueueOperation::Claim,
            "wrong_payload" => event.data["pending"] = serde_json::json!(state.pending + 1),
            "presequenced" => event.event_seq = 1,
            _ => unreachable!(),
        }
        assert!(
            PreparedQueueTransition::new(command)
                .with_original_terminal_state_audit(event)
                .is_err(),
            "{mode} must not construct original terminal notification"
        );
    }
    assert!(Arc::ptr_eq(
        &before,
        &runtime.inner.mutation_owner.capture()?.unwrap()
    ));
    Ok(())
}

#[tokio::test]
async fn original_terminal_notification_lost_ack_replays_its_frozen_id_after_actual_memory_finish(
) -> Result<()> {
    use crate::runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
    use crate::runtime_db::transitions::{
        AgentStateMutation, PreparedQueueTransition, QueueMutation, QueueOperation,
        QueueTransitionCommand,
    };
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let message = runtime
        .enqueue(MessageEnvelope::new(
            runtime.instance_agent_id(),
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original lost terminal source".into(),
            },
        ))
        .await?;
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime).poll().await?
    else {
        anyhow::bail!("actual lost terminal source was not claimed");
    };
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await?;
    let terminal = runtime
        .build_reducer_only_terminal_transition("original lost terminal notification", false)
        .await?;
    let mut mutation = runtime.begin_mutation().await?;
    let before = mutation.capture_published_root()?;
    let mut state = mutation.state.clone();
    state.current_run_id = None;
    state.current_turn_id = Some(terminal.terminal.turn_id.clone());
    state.last_turn_terminal = Some(terminal.terminal.clone());
    let queue = QueueEntryRecord {
        status: QueueEntryStatus::Processed,
        updated_at: runtime.now(),
        ..runtime
            .runtime_db()
            .queue_entries()
            .latest(&message.id)?
            .context("original claimed queue missing")?
    };
    let command = QueueTransitionCommand {
        agent_id: state.id.clone(),
        operation: QueueOperation::Settle,
        mutation: QueueMutation::Upsert(queue.clone()),
        scheduler_claim_work_item: None,
        agent_state: Some(AgentStateMutation {
            expected: Some(Box::new(mutation.last_persisted_state.clone())),
            record: Box::new(state.clone()),
        }),
        message_evidence: Vec::new(),
        transcript_entries: Vec::new(),
        turn_record: Some(terminal.turn_record.clone()),
        audit_events: vec![
            AuditEvent::legacy(
                "queue_entry_settled",
                serde_json::json!({"message_id":message.id,"status":"processed"}),
            ),
            AuditEvent::legacy("turn_terminal", serde_json::to_value(&terminal.terminal)?),
            AuditEvent::legacy(
                "agent_state_mutated",
                serde_json::json!({"agent_id":state.id}),
            ),
        ],
        notify_scheduler: true,
        fault: None,
        brief_evidence: Vec::new(),
    };
    let audit = runtime.prepare_original_state_changed_audit(&state, &command.audit_events)?;
    let original_id = audit.event().id.clone();
    let original_time = audit.event().created_at;
    let mut queue_command = PreparedQueueTransition::new(command)
        .with_original_terminal_state_audit(audit.event().clone())?;
    queue_command.execution_protocol = execution_protocol_settlement_transition_from_facts(
        &runtime.inner.storage,
        &runtime.inner.runtime_db,
        &queue,
        Some(&terminal.turn_record),
    )?;
    let prepared = PreparedInputTransition::queue(
        mutation.input_evidence_scope(),
        "original-terminal-lost-whole-nonce".into(),
        runtime.now(),
        queue_command,
    )?;
    let prepared = mutation
        .attach_actual_dispatch_memory(prepared)?
        .with_current_read_source(before.current_reads.as_ref().unwrap().clone())?;
    let witness = crate::runtime::mutation_witness::MutationWitness::input_mutation(
        prepared.descriptor().command_nonce().into(),
        crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
            prepared.descriptor().clone(),
        ),
        Vec::new(),
    )?
    .with_input_runtime(vec![Arc::new(mutation.facts(Default::default())?)])?;
    let error = mutation
        .commit_transition_inner(
            prepared.descriptor().command_nonce().into(),
            Some(witness),
            || {
                assert!(matches!(
                    commit(&runtime, &prepared)?,
                    InputMutationExecution::Committed { .. }
                ));
                Err(anyhow!(
                    "original terminal whole ACK deliberately lost after real COMMIT"
                ))
            },
        )
        .await
        .err()
        .context("actual original terminal ACK was not lost")?;
    assert!(format!("{error:#}").contains("deliberately lost"));
    drop(mutation);
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    assert!(reservation
        .recovery()?
        .context("lost terminal recovery missing")?
        .known
        .is_none());
    drop(reservation);
    let (audit_count, receipt_count): (usize, usize) = runtime
        .runtime_db()
        .connection()?
        .query_row(
        "SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM input_mutation_receipts)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(
        runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE audit_event_id=?1",
            [&original_id],
            |r| r.get::<_, usize>(0)
        )?,
        1
    );
    let owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    let selected = root.current_reads.as_ref().unwrap().capture(&request())?;
    let actual = selected
        .events
        .iter()
        .find_map(|r| match &r.fact {
            CurrentReadFact::Event(e) if e.id == original_id => Some(e),
            _ => None,
        })
        .context("fresh original receipt omitted its frozen terminal notification")?;
    assert_eq!(actual.created_at, original_time);
    assert_eq!(actual.data, audit.event().data);
    assert_eq!(
        actual.data,
        serde_json::to_value(AgentStateChangedEvent::from_state(&root.state))?
    );
    assert_eq!(
        root.current_reads.as_ref().unwrap().coverage().revision,
        before.current_reads.as_ref().unwrap().coverage().revision + 1
    );
    assert_eq!(runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND turn_id=?2 AND state='done'",rusqlite::params![state.id,terminal.turn_record.turn_id],|r|r.get::<_,usize>(0))?,1);
    drop(owner);
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO original_terminal_exact_receipt_forbidden; ALTER TABLE queue_entries RENAME TO original_terminal_exact_queue_forbidden")?;
    let InputMutationExecution::Replayed { receipt, .. } = commit(&runtime, &prepared)? else {
        anyhow::bail!("exact original terminal receipt did not replay before business SQL");
    };
    assert_eq!(receipt.prepared, *prepared.descriptor());
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE original_terminal_exact_queue_forbidden RENAME TO queue_entries; ALTER TABLE original_terminal_exact_receipt_forbidden RENAME TO audit_events")?;
    assert_eq!(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM input_mutation_receipts)",[],|r|Ok((r.get::<_,usize>(0)?,r.get::<_,usize>(1)?)))?,(audit_count,receipt_count));
    drop((selected, root, before, prepared, audit, scheduled));
    Ok(())
}
