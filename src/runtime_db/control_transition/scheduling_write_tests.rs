//! Actual Control SQL acknowledgments, raw old cleanup, sparse and rollback.
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
        resident_scheduling::{
            CanonicalSchedulingFact, SchedulingCaptureRequest, SchedulingFamily, SchedulingKey,
        },
        resident_work::WorkCaptureRequest,
    },
    runtime_db::{scheduling_snapshot as scheduling, work_snapshot::WorkDomainLoadRequest},
};
fn scope(f: &Fixture) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: f.key.agent_id.clone(),
        identity_incarnation: f.key.identity_incarnation,
    }
}
fn request() -> scheduling::SchedulingDomainLoadRequest {
    scheduling::SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    }
}
fn work_request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 16 * 1024 * 1024,
    }
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn active_timer(f: &Fixture) -> TimerRecord {
    TimerRecord {
        calendar: None,
        id: "original-active-timer".into(),
        agent_id: f.key.agent_id.clone(),
        created_at: f.time,
        duration_ms: 1000,
        interval_ms: Some(1000),
        repeat: true,
        status: TimerStatus::Active,
        summary: Some("original timer".into()),
        next_fire_at: Some(f.time + chrono::Duration::seconds(1)),
        last_fired_at: Some(f.time),
        fire_count: 4,
    }
}
fn completed_timer(f: &Fixture) -> TimerRecord {
    TimerRecord {
        id: "original-completed-timer".into(),
        status: TimerStatus::Completed,
        repeat: false,
        interval_ms: None,
        next_fire_at: None,
        fire_count: 9,
        ..active_timer(f)
    }
}
fn trigger(f: &Fixture) -> ExternalTriggerRecord {
    ExternalTriggerRecord {
        external_trigger_id: "original-scheduling-trigger".into(),
        target_agent_id: f.key.agent_id.clone(),
        scope: ExternalTriggerScope::Agent,
        delivery_mode: CallbackDeliveryMode::EnqueueMessage,
        token: Some("original-token".into()),
        token_hash: "original-hash".into(),
        status: ExternalTriggerStatus::Active,
        created_at: f.time,
        revoked_at: None,
        last_delivered_at: None,
        delivery_count: 2,
    }
}
fn keys() -> BTreeSet<SchedulingKey> {
    BTreeSet::from([
        SchedulingKey::new(SchedulingFamily::Timer, "original-active-timer"),
        SchedulingKey::new(SchedulingFamily::Timer, "original-completed-timer"),
        SchedulingKey::wake("original-completed-timer", "original-completed-wake"),
        SchedulingKey::new(
            SchedulingFamily::ExternalTrigger,
            "original-scheduling-trigger",
        ),
    ])
}
fn seed(f: &Fixture, ledger: &InputEvidenceBudgetLedger, qualified: bool) -> Result<()> {
    f.db.transaction(|tx| {
        scheduling::prepare_scheduling_domain_snapshot_tx(tx, &scope(f), &request(), ledger)?
            .require_current()?;
        let proof = if qualified { Some(scheduling::prepare_scheduling_write_tx(tx, &scope(f), &keys(), &request(), ledger)?) } else { None };
        upsert_timer_tx(tx, &active_timer(f))?;
        upsert_timer_tx(tx, &completed_timer(f))?;
        upsert_external_trigger_tx(tx, &trigger(f))?;
        let mut message = MessageEnvelope::new(&f.key.agent_id, MessageKind::TimerTick,
            MessageOrigin::Timer { timer_id: "original-completed-timer".into() },
            AuthorityClass::RuntimeInstruction, Priority::Next,
            MessageBody::Text { text: "original one-shot wake".into() });
        message.id = "original-completed-wake".into(); message.created_at = f.time;
        append_message_tx(tx, &message)?;
        let stored: MessageEnvelope = payload_optional(tx, "SELECT payload_json FROM messages WHERE evidence_id=?1", &message.id)?.context("actual source missing")?;
        super::super::super::input_evidence_snapshot::register_committed_input_evidence_tx(tx, &scope(f),
            &[super::super::super::input_evidence_snapshot::CanonicalInputEvidence::Message(stored)])?;
        upsert_queue_entry_tx(tx, &QueueEntryRecord { message_id: message.id.clone(),
            agent_id: f.key.agent_id.clone(), priority: message.priority, status: QueueEntryStatus::Queued,
            created_at: f.time, updated_at: f.time })?;
        // A Completed Timer may retain its original earlier fire-count Wake.
        tx.execute("INSERT INTO timer_wakes(timer_id,message_id,fire_count,status,created_at,updated_at) VALUES(?1,?2,3,'pending',?3,?3)",
            params!["original-completed-timer",message.id,f.time.to_rfc3339()])?;
        let mut state = f.db_state(tx)?; state.pending = 1; state.total_message_count = 1;
        super::super::super::evidence::upsert_agent_state_tx(tx, &state)?;
        if let Some(proof) = proof {
            scheduling::register_scheduling_writes_tx(tx, &proof, &keys(), &state, &request())?;
        }
        Ok(())
    })
}
fn fingerprint(f: &Fixture) -> Result<String> {
    let mut text = f.fingerprint();
    let connection = f.db.connection()?;
    for table in [
        "scheduling_domain_current",
        "scheduling_domain_records",
        "work_domain_current",
        "work_domain_records",
        "input_evidence_current",
        "task_message_order_headers",
    ] {
        text.push_str(table);
        let mut statement = connection.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
        let columns = statement.column_count();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            for column in 0..columns {
                text.push_str(&format!("{:?}", row.get_ref(column)?));
            }
        }
    }
    Ok(text)
}
fn commit(
    f: &Fixture,
    command: &PreparedControl,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ControlCommit> {
    f.db.commit_control_with_scheduling_request(command, &work_request(), &request(), ledger)
}

#[test]
fn control_stop_cancels_completed_owner_wake_and_publishes_one_final_scheduling_version(
) -> Result<()> {
    let mut f = Fixture::new();
    f.time = "2026-10-01T00:00:00.123456789Z".parse()?;
    let ledger = ledger()?;
    seed(&f, &ledger, true)?;
    let before =
        f.db.transaction(|tx| {
            scheduling::load_scheduling_domain_snapshot_tx(tx, &scope(&f), &request(), &ledger)?
                .require_current()
        })?
        .resident;
    let command = f.command(ControlAction::Stop);
    assert_eq!(command.expected.timers.len(), 1);
    assert_eq!(command.expected.pending_timer_wakes.len(), 1);
    assert_eq!(command.expected.pending_timer_wakes[0].fire_count, 3);
    let ControlCommit::Applied { receipt, delta } = commit(&f, &command, &ledger)? else {
        anyhow::bail!("first Stop replayed");
    };
    let effects = &delta.transition.effects;
    assert!(effects.scheduling_delta_requires_prepared_domain);
    assert_eq!(effects.scheduling_write_keys.len(), 3);
    assert!(!effects.scheduling_write_keys.contains(&SchedulingKey::new(
        SchedulingFamily::Timer,
        "original-completed-timer"
    )));
    let change = effects
        .scheduling_delta
        .as_ref()
        .context("Scheduling delta missing")?;
    assert_eq!(change.revision, change.base_revision + 1);
    assert!(change.coverage.active_timers.is_empty());
    assert!(change.coverage.pending_wakes.is_empty());
    assert!(change.coverage.active_triggers.is_empty());
    let current = before.with_committed(change)?;
    let capture = current.capture(&request().capture)?;
    let CanonicalSchedulingFact::TimerWake(wake) = &capture
        .lookup(&SchedulingKey::wake(
            "original-completed-timer",
            "original-completed-wake",
        ))?
        .context("Wake missing")?
        .fact
    else {
        anyhow::bail!("wrong Wake family");
    };
    assert_eq!(wake.fire_count, 3);
    assert_eq!(
        wake.status,
        crate::runtime::resident_scheduling::SchedulingWakeStatus::Cancelled
    );
    assert_eq!(wake.cancelled_at, Some(command.prepared_at));
    assert_eq!(
        delta.timer_wakes[0].cancelled_at.as_deref(),
        Some(command.prepared_at.to_rfc3339().as_str())
    );
    let original = fingerprint(&f)?;
    let ControlCommit::Replayed { receipt: replay } = commit(&f, &command, &ledger)? else {
        anyhow::bail!("Stop did not replay");
    };
    assert_eq!(receipt, replay);
    assert_eq!(fingerprint(&f)?, original);
    drop((capture, current, before, delta, command, replay, receipt));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_stop_raw_existing_cleanup_never_adopts_scheduling_body() -> Result<()> {
    let f = Fixture::new();
    let ledger = ledger()?;
    seed(&f, &ledger, false)?;
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { delta, .. } = commit(&f, &command, &ledger)? else {
        anyhow::bail!("raw Stop replayed");
    };
    assert!(delta.transition.effects.scheduling_write_keys.is_empty());
    let change = delta
        .transition
        .effects
        .scheduling_delta
        .as_ref()
        .context("delta")?;
    assert_eq!(change.revision, change.base_revision + 1);
    assert!(change.records.is_empty());
    assert!(change.coverage.active_timers.is_empty());
    assert!(change.coverage.pending_wakes.is_empty());
    assert!(change.coverage.active_triggers.is_empty());
    let aliases: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM scheduling_domain_records",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(aliases, 0);
    assert_eq!(delta.timers[0].status, TimerStatus::Cancelled);
    assert_eq!(delta.timer_wakes[0].status, "cancelled");
    drop((delta, command));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_scheduling_finalizer_and_original_receipt_fault_roll_back_all_business_effects(
) -> Result<()> {
    for after_receipt in [false, true] {
        let f = Fixture::new();
        let ledger = ledger()?;
        seed(&f, &ledger, true)?;
        let mut command = f.command(ControlAction::Stop);
        if after_receipt {
            command = command.with_fault(ControlFaultPoint::AfterReceiptWrite);
        } else {
            f.db.connection()?.execute_batch("CREATE TRIGGER deny_final_scheduling BEFORE UPDATE ON scheduling_domain_current BEGIN SELECT RAISE(ABORT,'final Scheduling fault'); END;")?;
        }
        let before = fingerprint(&f)?;
        let error = commit(&f, &command, &ledger).unwrap_err();
        if !after_receipt {
            assert!(format!("{error:#}").contains("final Scheduling fault"));
        }
        assert_eq!(fingerprint(&f)?, before);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn control_scheduling_snapshot_pool_budget_precedes_concurrent_corrupt_body_decode() -> Result<()> {
    let f = Fixture::new();
    let ledger = ledger()?;
    seed(&f, &ledger, true)?;
    let command = f.command(ControlAction::Stop);
    // Corrupt concurrent restore: it was not in the frozen Control target set.
    f.db.transaction(|tx| {
        let mut late = active_timer(&f);
        late.id = "late-corrupt-timer".into();
        upsert_timer_tx(tx, &late)?;
        tx.execute(
            "UPDATE timers SET payload_json=?2 WHERE timer_id=?1",
            params![late.id, "{".repeat(32 * 1024)],
        )?;
        Ok(())
    })?;
    let before = fingerprint(&f)?;
    let small = scheduling::SchedulingDomainLoadRequest {
        byte_limit: 8192,
        ..request()
    };
    let error =
        f.db.commit_control_with_scheduling_request(&command, &work_request(), &small, &ledger)
            .unwrap_err();
    assert!(format!("{error:#}").contains("ResourceExhausted"));
    assert_eq!(fingerprint(&f)?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_deleting_no_state_scheduling_maintenance_is_incomplete_and_existing_only() -> Result<()>
{
    let f = Fixture::new();
    let ledger = ledger()?;
    seed(&f, &ledger, true)?;
    let identity =
        f.db.agent_identities()
            .latest(&f.key.agent_id)?
            .context("identity")?;
    f.db.agent_deletions()
        .begin(&f.key.agent_id, identity.revision, "test", false)?;
    f.db.connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&f.key.agent_id],
    )?;
    let mut ack = f.db.transaction(|tx| {
        let proof = scheduling::prepare_existing_scheduling_maintenance_tx(
            tx,
            &scope(&f),
            &keys(),
            &request(),
            &ledger,
        )?;
        let mut timer = active_timer(&f);
        timer.status = TimerStatus::Cancelled;
        let changed = super::super::super::repositories::upsert_timer_with_write_tx(tx, &timer)?;
        assert!(changed);
        scheduling::register_existing_scheduling_maintenance_tx(
            tx,
            proof,
            &BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &timer.id)]),
            &request(),
        )
    })?;
    assert_eq!(ack._scope, scope(&f));
    assert_eq!(ack.qualified_actual_keys.len(), 1);
    let invalid = ack.invalidation.as_ref().context("invalidation")?;
    assert_eq!(invalid.revision, invalid.base_revision + 1);
    let complete:bool=f.db.connection()?.query_row("SELECT json_extract(coverage_json,'$.active_timers_complete') OR json_extract(coverage_json,'$.pending_wakes_complete') OR json_extract(coverage_json,'$.active_triggers_complete') FROM scheduling_domain_current WHERE agent_id=?1",[&f.key.agent_id],|r|r.get(0))?;
    assert!(!complete);
    let state_count: usize =
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM agent_states", [], |r| r.get(0))?;
    assert_eq!(state_count, 0);
    assert!(f
        .db
        .transaction(|tx| scheduling::prepare_existing_scheduling_maintenance_tx(
            tx,
            &scope(&f),
            &BTreeSet::from([SchedulingKey::new(
                SchedulingFamily::Timer,
                "never-admitted-new-timer"
            )]),
            &request(),
            &ledger
        )
        .map(|_| ()))
        .is_err());
    let guards = ack.take_budget_views();
    assert!(!guards.is_empty());
    drop((guards, ack));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_scheduling_maintenance_rejects_successor_before_poison_state_or_alias_writes(
) -> Result<()> {
    let f = Fixture::new();
    let ledger = ledger()?;
    seed(&f, &ledger, true)?;
    let before = fingerprint(&f)?;
    let error=f.db.transaction(|tx| {
        let proof=scheduling::prepare_existing_scheduling_maintenance_tx(tx,&scope(&f),&keys(),&request(),&ledger)?;
        // Explicit impossible restore, only to challenge the stale proof boundary.
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&f.key.agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='poison-successor-state' WHERE agent_id=?1",[&f.key.agent_id])?;
        scheduling::register_existing_scheduling_maintenance_tx(tx,proof,&BTreeSet::new(),&request())
    }).unwrap_err();
    assert!(format!("{error:#}").contains("Scheduling scope retired"));
    assert_eq!(fingerprint(&f)?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn control_exact_original_receipt_precedes_new_scope_and_scheduling_budget() -> Result<()> {
    let f = Fixture::new();
    let ledger = ledger()?;
    seed(&f, &ledger, true)?;
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = commit(&f, &command, &ledger)? else {
        anyhow::bail!("first replayed");
    };
    drop(delta);
    f.db.transaction(|tx| {
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&f.key.agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='poison-successor-state' WHERE agent_id=?1",[&f.key.agent_id])?;
        Ok(())
    })?;
    let before = fingerprint(&f)?;
    let zero = scheduling::SchedulingDomainLoadRequest {
        record_limit: 0,
        byte_limit: 0,
        ..request()
    };
    let ControlCommit::Replayed { receipt: replay } =
        f.db.commit_control_with_scheduling_request(&command, &work_request(), &zero, &ledger)?
    else {
        anyhow::bail!("replay reran business");
    };
    assert_eq!(replay, receipt);
    assert_eq!(fingerprint(&f)?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn existing_scheduling_maintenance_rejects_pruned_members_and_undeclared_body_changes() -> Result<()>
{
    for pruned in [false, true] {
        let f = Fixture::new();
        let ledger = ledger()?;
        seed(&f, &ledger, true)?;
        if pruned {
            // Explicit corruption: retain the canonical body behind a Pruned alias.
            f.db.connection()?.execute("UPDATE scheduling_domain_records SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE agent_id=?1 AND family='timer' AND record_id='original-active-timer'", [&f.key.agent_id])?;
        }
        let before = fingerprint(&f)?;
        let error =
            f.db.transaction(|tx| {
                let proof = scheduling::prepare_existing_scheduling_maintenance_tx(
                    tx,
                    &scope(&f),
                    &keys(),
                    &request(),
                    &ledger,
                )?;
                let mut timer = active_timer(&f);
                timer.status = TimerStatus::Cancelled;
                assert!(super::super::super::repositories::upsert_timer_with_write_tx(tx, &timer)?);
                scheduling::register_existing_scheduling_cleanup_tx(
                    tx,
                    proof,
                    &BTreeSet::new(),
                    &f.db_state(tx)?,
                    &request(),
                )
            })
            .unwrap_err();
        let text = format!("{error:#}");
        assert!(
            if pruned {
                text.contains("Pruned")
            } else {
                text.contains("undeclared actual target")
            },
            "{text}"
        );
        assert_eq!(fingerprint(&f)?, before);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
