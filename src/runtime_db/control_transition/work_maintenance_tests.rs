use super::*;
use crate::runtime::{
    resident_input_evidence::InputEvidenceScope,
    resident_work::{
        CanonicalWorkRecord, WorkAvailability, WorkCaptureRequest, WorkFamily, WorkKey,
    },
};
use crate::runtime_db::work_snapshot::{self, WorkDomainLoadRequest};

fn scope(fixture: &Fixture) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: fixture.key.agent_id.clone(),
        identity_incarnation: fixture.key.identity_incarnation,
    }
}
fn request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 1024,
        byte_limit: 1024 * 1024,
    }
}
fn qualified_task_wait(fixture: &Fixture) -> Result<()> {
    let task = TaskRecord {
        id: "stop-work-task".into(),
        agent_id: fixture.key.agent_id.clone(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: fixture.time,
        updated_at: fixture.time,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("running task".into()),
        detail: Some(
            json!({"rejoin_obligation_id":"stop-work-task", "rejoin_generation":1,
            "parent_turn_id":"stop-parent"}),
        ),
        recovery: None,
    };
    let wait = WaitConditionRecord {
        id: "stop-work-wait".into(),
        agent_id: fixture.key.agent_id.clone(),
        work_item_id: None,
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::Task,
        source: Some("task".into()),
        subject_ref: Some(task.id.clone()),
        waiting_for: "TaskResult".into(),
        wake_sources: vec![WakeSource::TaskResult {
            task_id: task.id.clone(),
        }],
        continuation: None,
        created_at: fixture.time,
        updated_at: fixture.time,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: None,
        trigger_message_id: None,
        triggered_at: None,
    };
    fixture.db.transaction(|tx| {
        work_snapshot::install_work_domain_test_schema(tx)?;
        work_snapshot::prepare_work_domain_snapshot_tx(tx, &scope(fixture), &request())?
            .require_current()?;
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Wait, &wait.id)]);
        let proof = work_snapshot::prepare_work_write_tx(tx, &scope(fixture), &keys)?;
        upsert_task_tx(tx, &task)?;
        let actual = upsert_wait_condition_with_writes_tx(tx, &wait)?;
        ensure!(actual.wait_ids == BTreeSet::from([wait.id.clone()]));
        let state = fixture.db_state(tx)?;
        work_snapshot::register_work_writes_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &state,
            &request(),
        )?;
        Ok(())
    })
}

#[test]
fn control_stop_publishes_only_the_final_cancelled_work_wait_and_replays_once() -> Result<()> {
    let fixture = Fixture::new();
    qualified_task_wait(&fixture)?;
    let before = fixture
        .db
        .transaction(|tx| {
            work_snapshot::load_work_domain_snapshot_tx(tx, &scope(&fixture), &request())?
                .require_current()
        })?
        .resident;
    let command = fixture.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = fixture
        .db
        .commit_control_with_work_request(&command, &request())?
    else {
        anyhow::bail!("first Stop replayed");
    };
    let work = delta
        .transition
        .effects
        .work_delta
        .as_ref()
        .context("Stop omitted Work delta")?;
    ensure!(delta.transition.effects.work_delta_requires_prepared_domain);
    assert_eq!(work.base_revision + 1, work.revision);
    assert_eq!(work.records.len(), 1);
    let CanonicalWorkRecord::Wait(wait) = &work.records[0].record else {
        anyhow::bail!("unexpected Work record");
    };
    assert_eq!(wait.status, WaitConditionStatus::Cancelled);
    assert_eq!(
        wait.trigger_message_id.as_deref(),
        Some("result:stop-work-task")
    );
    assert_eq!(
        work.coverage.observations.get(&work.records[0].key),
        Some(&WorkAvailability::Present)
    );
    assert!(work.coverage.unresolved_wait_ids.is_empty());
    let current = before.with_committed(work)?;
    let captured = current.capture(&WorkCaptureRequest {
        mandatory_roots: BTreeSet::from([WorkKey::new(WorkFamily::Wait, "stop-work-wait")]),
        ..WorkCaptureRequest::current()
    })?;
    assert!(captured
        .records
        .iter()
        .any(|record| matches!(&record.record,
        CanonicalWorkRecord::Wait(wait) if wait.status == WaitConditionStatus::Cancelled)));
    let exact = fixture.db.transaction(|tx| {
        work_snapshot::prepare_work_domain_snapshot_tx(
            tx,
            &scope(&fixture),
            &WorkDomainLoadRequest {
                capture: WorkCaptureRequest {
                    mandatory_roots: BTreeSet::from([WorkKey::new(
                        WorkFamily::Wait,
                        "stop-work-wait",
                    )]),
                    ..WorkCaptureRequest::current()
                },
                ..request()
            },
        )?
        .require_current()
    })?;
    assert_eq!(exact.resident.coverage().revision, work.revision);
    let ControlCommit::Replayed { receipt: replay } = fixture
        .db
        .commit_control_with_work_request(&command, &request())?
    else {
        anyhow::bail!("Stop did not replay");
    };
    assert_eq!(replay, receipt);
    let state = fixture.db.load_control_snapshot(&fixture.key)?;
    assert!(state.waits.is_empty());
    assert_eq!(state.message_count, 1);
    Ok(())
}

#[test]
fn control_stop_work_failure_rolls_back_trigger_cancel_alias_and_receipt() -> Result<()> {
    let fixture = Fixture::new();
    qualified_task_wait(&fixture)?;
    let before = fixture.db.transaction(|tx| {
        work_snapshot::load_work_domain_snapshot_tx(tx, &scope(&fixture), &request())?
            .require_current()
    })?;
    let command = fixture
        .command(ControlAction::Stop)
        .with_fault(ControlFaultPoint::AfterReceiptWrite);
    assert!(fixture
        .db
        .commit_control_with_work_request(&command, &request())
        .is_err());
    let after = fixture.db.transaction(|tx| {
        work_snapshot::load_work_domain_snapshot_tx(tx, &scope(&fixture), &request())?
            .require_current()
    })?;
    assert_eq!(after.facts.state, before.facts.state);
    assert_eq!(after.resident.coverage(), before.resident.coverage());
    assert_eq!(
        fixture
            .db
            .load_control_snapshot(&fixture.key)?
            .message_count,
        0
    );
    assert!(fixture
        .db
        .control_receipt(
            &fixture.key,
            &command.invocation,
            Some(&command.command_digest)
        )?
        .is_none());
    let captured = after.resident.capture(&WorkCaptureRequest::current())?;
    assert!(captured.records.iter().any(|record| matches!(&record.record,
        CanonicalWorkRecord::Wait(wait) if wait.status == WaitConditionStatus::Active && wait.trigger_message_id.is_none())));
    Ok(())
}

fn input_request() -> crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
    crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: BTreeSet::new(),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: 1024,
        byte_limit: 1024 * 1024,
    }
}

#[test]
fn control_lost_ack_prepares_current_work_inputs_and_episodes_in_one_view() -> Result<()> {
    let fixture = Fixture::new();
    qualified_task_wait(&fixture)?;
    let command = fixture
        .command(ControlAction::Stop)
        .with_fault(ControlFaultPoint::AfterCommitBeforeAck);
    assert!(fixture
        .db
        .commit_control_with_work_request(&command, &request())
        .is_err());
    // A later canonical writer advances the current view. Recovery returns
    // this view while retaining the original Stop outcome in the receipt.
    fixture.db.transaction(|tx| {
        let mut wait = load_wait_by_id(tx, "stop-work-wait")?;
        wait.id = "later-current-wait".into();
        wait.status = WaitConditionStatus::Active;
        wait.trigger_message_id = None;
        wait.triggered_at = None;
        wait.cancelled_at = None;
        wait.created_at += chrono::Duration::seconds(20);
        wait.updated_at = wait.created_at;
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Wait, &wait.id)]);
        let proof = work_snapshot::prepare_work_write_tx(tx, &scope(&fixture), &keys)?;
        upsert_wait_condition_with_writes_tx(tx, &wait)?;
        let mut state = fixture.db_state(tx)?;
        state.status = AgentStatus::AwakeIdle;
        upsert_agent_state_tx(tx, &state)?;
        work_snapshot::register_work_writes_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &state,
            &request(),
        )?;
        Ok(())
    })?;
    let budget =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let ControlRecoveryObservation::Current {
        snapshot,
        episodes,
        domains,
        receipt,
        facts,
        ..
    } = fixture.db.load_control_recovery_with_domains(
        command.key(),
        command.invocation(),
        command.command_digest(),
        12,
        &input_request(),
        &WorkCaptureRequest::current(),
        &budget,
    )?
    else {
        anyhow::bail!("same scope unexpectedly retired");
    };
    assert_eq!(snapshot.state.status, AgentStatus::AwakeIdle);
    assert_eq!(receipt.unwrap().outcome.status, AgentStatus::Stopped);
    assert!(episodes.belongs_to(&fixture.key.agent_id, fixture.key.identity_incarnation));
    let domains = domains.context("positive recovery has no prepared domains")?;
    assert_eq!(domains.work.scope(), &scope(&fixture));
    assert_eq!(domains.inputs.scope(), &scope(&fixture));
    assert!(!domains.inputs.mandatory_roots().contains(
        &crate::runtime::resident_input_evidence::InputEvidenceKey::new(
            crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
            "result:stop-work-task"
        )
    ));

    let waits = domains.work.capture(&WorkCaptureRequest {
        mandatory_roots: BTreeSet::from([WorkKey::new(WorkFamily::Wait, "stop-work-wait")]),
        ..WorkCaptureRequest::current()
    })?;
    assert!(waits.records.iter().any(|record| matches!(&record.record,
        CanonicalWorkRecord::Wait(wait) if wait.id == "later-current-wait" && wait.status == WaitConditionStatus::Active)));
    assert!(waits.records.iter().any(|record| matches!(&record.record,
        CanonicalWorkRecord::Wait(wait) if wait.id == "stop-work-wait" && wait.status == WaitConditionStatus::Cancelled)));
    assert!(facts.current_queue_messages.is_empty());
    Ok(())
}

#[test]
fn control_positive_recovery_reads_original_deleting_scope_but_never_a_successor() -> Result<()> {
    let fixture = Fixture::new();
    qualified_task_wait(&fixture)?;
    let command = fixture.command(ControlAction::Stop);
    fixture
        .db
        .commit_control_with_work_request(&command, &request())?;
    let mut identity = fixture
        .db
        .agent_identities()
        .latest(&fixture.key.agent_id)?
        .unwrap();
    identity.status = AgentRegistryStatus::Deleting;
    fixture.db.agent_identities().upsert(&identity)?;
    let budget =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let current = fixture.db.load_control_recovery_with_domains(
        command.key(),
        command.invocation(),
        command.command_digest(),
        12,
        &input_request(),
        &WorkCaptureRequest::current(),
        &budget,
    )?;
    assert!(matches!(
        current,
        ControlRecoveryObservation::Current {
            domains: Some(_),
            ..
        }
    ));
    identity.incarnation += 1;
    identity.status = AgentRegistryStatus::Active;
    fixture.db.agent_identities().upsert(&identity)?;
    let successor = fixture.db.load_control_recovery_with_domains(
        command.key(),
        command.invocation(),
        command.command_digest(),
        12,
        &input_request(),
        &WorkCaptureRequest::current(),
        &budget,
    )?;
    assert!(matches!(
        successor,
        ControlRecoveryObservation::Retired {
            receipt: Some(_),
            ..
        }
    ));
    Ok(())
}

#[test]
fn control_receipt_absence_does_not_prepare_an_unrequested_work_domain() -> Result<()> {
    let fixture = Fixture::new();
    let budget =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let command = fixture.command(ControlAction::Stop);
    let mut input = input_request();
    input.record_limit = 0;
    input.byte_limit = 0;
    let observation = fixture.db.load_control_recovery_with_domains(
        command.key(),
        command.invocation(),
        command.command_digest(),
        12,
        &input,
        &WorkCaptureRequest::current(),
        &budget,
    )?;
    assert!(matches!(
        observation,
        ControlRecoveryObservation::Absent { obligations } if obligations.is_empty()
    ));
    Ok(())
}

#[test]
fn control_stop_cleans_unqualified_historical_wait_without_adopting_it() -> Result<()> {
    let fixture = Fixture::new();
    let wait: WaitConditionRecord = serde_json::from_value(json!({
        "id":"unqualified-stop-wait","agent_id":fixture.key.agent_id,
        "status":"active","kind":"task","waiting_for":"legacy task",
        "wake_sources":[{"kind":"task_result","task_id":"missing-old-task"}],
        "created_at":fixture.time,"updated_at":fixture.time
    }))?;
    fixture.db.transaction(|tx| {
        work_snapshot::install_work_domain_test_schema(tx)?;
        work_snapshot::prepare_work_domain_snapshot_tx(tx, &scope(&fixture), &request())?
            .require_current()?;
        upsert_wait_condition_with_writes_tx(tx, &wait)?;
        Ok(())
    })?;
    let command = fixture.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = fixture
        .db
        .commit_control_with_work_request(&command, &request())?
    else {
        anyhow::bail!("new Stop replayed");
    };
    assert!(receipt.outcome.cancelled_wait_ids.contains(&wait.id));
    assert_eq!(
        fixture
            .db
            .transaction(|tx| load_wait_by_id(tx, &wait.id))?
            .status,
        WaitConditionStatus::Cancelled
    );
    assert!(delta.transition.effects.work_delta.is_none());
    fixture.db.transaction(|tx| {
        let alias: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_domain_records WHERE record_id=?1)",
            [&wait.id],
            |r| r.get(0),
        )?;
        let edge: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_domain_wait_edges WHERE wait_id=?1)",
            [&wait.id],
            |r| r.get(0),
        )?;
        assert!(!alias && !edge);
        let mut selected = request();
        selected
            .capture
            .mandatory_roots
            .insert(WorkKey::new(WorkFamily::Wait, &wait.id));
        let error = work_snapshot::prepare_work_domain_snapshot_tx(tx, &scope(&fixture), &selected)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<work_snapshot::WorkSnapshotError>(),
            Some(work_snapshot::WorkSnapshotError::NeedsPreparation(_))
        ));
        Ok(())
    })?;
    assert!(matches!(
        fixture
            .db
            .commit_control_with_work_request(&command, &request())?,
        ControlCommit::Replayed { .. }
    ));
    Ok(())
}
