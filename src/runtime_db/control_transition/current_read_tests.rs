//! Real Control ACKs maintain only accepted Queue/Event sources in one TX.
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{
        current_read_facts::{CurrentReadError, CurrentReadKey, CurrentReadRequest},
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
        resident_scheduling::SchedulingCaptureRequest,
        resident_work::WorkCaptureRequest,
    },
    runtime_db::{
        current_read_snapshot as reads, scheduling_snapshot::SchedulingDomainLoadRequest,
        work_snapshot::WorkDomainLoadRequest,
    },
};
fn scope(f: &Fixture) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: f.key.agent_id.clone(),
        identity_incarnation: f.key.identity_incarnation,
    }
}
fn request() -> CurrentReadRequest {
    CurrentReadRequest::current(64 * 1024 * 1024, 65536)
}
fn budget() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn queue(f: &Fixture, id: &str) -> QueueEntryRecord {
    QueueEntryRecord {
        message_id: id.into(),
        agent_id: f.key.agent_id.clone(),
        priority: Priority::Next,
        status: QueueEntryStatus::Queued,
        created_at: f.time,
        updated_at: f.time,
    }
}
fn seed(f: &Fixture, ledger: &InputEvidenceBudgetLedger, qualified: bool) -> Result<()> {
    f.db.transaction(|tx| {
        let state = f.db_state(tx)?;
        reads::prepare_current_read_snapshot_tx(tx, &scope(f), &state, &request(), ledger)?;
        let row = queue(f, "original-current-queue");
        if qualified {
            let proof = reads::prepare_current_read_write_tx(
                tx,
                &scope(f),
                &state,
                &BTreeSet::from([CurrentReadKey::queue(&row.message_id)]),
                &request(),
                ledger,
            )?;
            let mut proof = proof;
            let actual = reads::upsert_current_read_queue_tx(&mut proof, &row)?;
            reads::register_current_read_writes_tx(proof, &[actual], &state)?;
        } else {
            upsert_queue_entry_tx(tx, &row)?;
        }
        Ok(())
    })
}
fn commit(
    f: &Fixture,
    command: &PreparedControl,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<ControlCommit> {
    let work = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    let scheduling = SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    f.db.commit_control_with_scheduling_request(command, &work, &scheduling, ledger)
}
fn version(f: &Fixture) -> Result<(u64, String, u64)> {
    Ok(f.db.connection()?.query_row("SELECT revision,freshness,(SELECT COUNT(*) FROM current_read_members) FROM current_read_current WHERE agent_id=?1 AND identity_incarnation=?2",params![f.key.agent_id,f.key.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
}
#[test]
fn control_queue_and_audit_join_one_current_revision_and_exact_receipt_replay() -> Result<()> {
    let f = Fixture::new();
    let ledger = budget()?;
    seed(&f, &ledger, true)?;
    let before = version(&f)?;
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = commit(&f, &command, &ledger)? else {
        panic!("first Control ACK")
    };
    let facts = delta
        .transition
        .effects
        .current_read_maintenance
        .as_ref()
        .context("actual maintenance ACK missing")?;
    assert!(delta.transition.effects.current_read_facts.is_none());
    assert_eq!(
        (facts.base_revision, facts.revision),
        (before.0, before.0 + 1)
    );
    let resident = facts
        .resident()
        .context("complete namespace stays Current")?;
    resident.validate_state(&delta.state)?;
    let capture = resident.capture(&request())?;
    assert!(!capture.has_queued());
    assert_eq!(capture.queue_counts["aborted"], 1);
    assert!(!capture.events.is_empty());
    assert_eq!(capture.events.len(), receipt.audit_bindings.len());
    assert_eq!(
        f.db.queue_entries()
            .latest("original-current-queue")?
            .unwrap()
            .status,
        QueueEntryStatus::Aborted
    );
    let after = version(&f)?;
    let fingerprint = f.fingerprint();
    let ControlCommit::Replayed { receipt: replay } = commit(&f, &command, &ledger)? else {
        panic!("exact replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(version(&f)?, after);
    assert_eq!(f.fingerprint(), fingerprint);
    assert_eq!(after.0, before.0 + 1);
    assert_eq!(after.1, "current");
    drop((capture, resident, delta));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn control_raw_cleanup_keeps_actual_ack_and_sparse_metadata_without_adopting_queue() -> Result<()> {
    let f = Fixture::new();
    let ledger = budget()?;
    seed(&f, &ledger, false)?;
    let before = version(&f)?;
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = commit(&f, &command, &ledger)? else {
        panic!("lawful raw cleanup ACK")
    };
    let facts = delta
        .transition
        .effects
        .current_read_maintenance
        .as_ref()
        .unwrap();
    assert!(facts.resident().is_none());
    assert_eq!(facts.revision, before.0 + 1);
    assert_eq!(
        f.db.queue_entries()
            .latest("original-current-queue")?
            .unwrap()
            .status,
        QueueEntryStatus::Aborted
    );
    let connection = f.db.connection()?;
    let member:u64=connection.query_row("SELECT COUNT(*) FROM current_read_members WHERE family='queue' AND record_id='original-current-queue'",[],|r|r.get(0))?;
    assert_eq!(member, 0);
    let (revision,freshness,complete):(u64,String,bool)=connection.query_row("SELECT revision,freshness,json_extract(coverage_json,'$.queue_complete') FROM current_read_current",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!(revision, before.0 + 1);
    assert_eq!(freshness, "records_invalidated");
    assert!(!complete);
    let audits: u64 = connection.query_row(
        "SELECT COUNT(*) FROM current_read_members WHERE family='event'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(audits, receipt.audit_bindings.len() as u64);
    drop(connection);
    let failed = f.db.transaction(|tx| {
        reads::prepare_current_read_snapshot_tx(tx, &scope(&f), &delta.state, &request(), &ledger)
    });
    assert!(matches!(
        failed.unwrap_err().downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::UnknownNamespace(_))
    ));
    let after = version(&f)?;
    assert!(matches!(
        commit(&f, &command, &ledger)?,
        ControlCommit::Replayed { .. }
    ));
    assert_eq!(version(&f)?, after);
    drop(delta);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn control_current_members_and_revision_rollback_with_original_receipt_faults() -> Result<()> {
    for fault in [
        ControlFaultPoint::AfterAuditWrites,
        ControlFaultPoint::AfterReceiptWrite,
        ControlFaultPoint::AfterObligationWrites,
    ] {
        let f = Fixture::new();
        let ledger = budget()?;
        seed(&f, &ledger, true)?;
        let before = version(&f)?;
        let fingerprint = f.fingerprint();
        let command = f.command(ControlAction::Stop).with_fault(fault);
        assert!(commit(&f, &command, &ledger).is_err());
        assert_eq!(version(&f)?, before);
        assert_eq!(f.fingerprint(), fingerprint);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
#[test]
fn control_current_preflight_budgets_raw_queue_columns_before_occ_body_decode() -> Result<()> {
    let f = Fixture::new();
    let ledger = budget()?;
    seed(&f, &ledger, false)?;
    let command = f.command(ControlAction::Stop);
    f.db.connection()?.execute("UPDATE queue_entries SET priority=printf('%.*c',?1,'x'),payload_json='{' WHERE message_id='original-current-queue'",[9*1024*1024])?;
    let before = version(&f)?;
    let error = commit(&f, &command, &ledger).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(version(&f)?, before);
    assert!(f
        .db
        .control_receipt(command.key(), command.invocation(), None)?
        .is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn original_maintenance_rejects_unregistered_and_omitted_actual_writes() -> Result<()> {
    let f = Fixture::new();
    let ledger = budget()?;
    seed(&f, &ledger, true)?;
    let before = version(&f)?;
    let failed: Result<()> = f.db.transaction(|tx| {
        let state = f.db_state(tx)?;
        let row = queue(&f, "new-original-queue");
        let keys = BTreeSet::from([CurrentReadKey::queue(&row.message_id)]);
        let mut first = reads::CurrentReadBusinessTransaction::prepare_original_maintenance_tx(
            tx,
            &scope(&f),
            &state,
            &keys,
            &request(),
            &ledger,
            (0, 0),
            vec![],
        )?;
        let second = reads::CurrentReadBusinessTransaction::prepare_original_maintenance_tx(
            tx,
            &scope(&f),
            &state,
            &keys,
            &request(),
            &ledger,
            (0, 0),
            vec![],
        )?;
        first.upsert_queue_tx(tx, &row)?;
        // A different proof cannot turn another actor's actual SQL into adoption.
        second.finish_original_maintenance(&state)?;
        Ok(())
    });
    assert!(failed.is_err());
    assert!(f.db.queue_entries().latest("new-original-queue")?.is_none());
    assert_eq!(version(&f)?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let failed: Result<()> = f.db.transaction(|tx| {
        let state = f.db_state(tx)?;
        let key = CurrentReadKey::queue("original-current-queue");
        let business = reads::CurrentReadBusinessTransaction::prepare_original_maintenance_tx(
            tx,
            &scope(&f),
            &state,
            &BTreeSet::from([key]),
            &request(),
            &ledger,
            (0, 0),
            vec![],
        )?;
        let mut row = queue(&f, "original-current-queue");
        row.status = QueueEntryStatus::Aborted;
        upsert_queue_entry_tx(tx, &row)?;
        business.finish_original_maintenance(&state)?;
        Ok(())
    });
    assert!(failed.is_err());
    assert_eq!(version(&f)?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn control_new_intent_rejects_retired_scope_but_original_receipt_still_replays() -> Result<()> {
    let f = Fixture::new();
    let ledger = budget()?;
    seed(&f, &ledger, true)?;
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { receipt, delta } = commit(&f, &command, &ledger)? else {
        panic!("first ACK")
    };
    drop(delta);
    let mut next_input = f.input(ControlAction::Stop);
    next_input.key.command_id = "new-control-after-deleting".into();
    let next = prepare_control(next_input)?;
    f.db.connection()?.execute("UPDATE agent_identities SET status='deleting',payload_json=json_set(payload_json,'$.status','deleting') WHERE agent_id=?1",[&f.key.agent_id])?;
    assert!(
        matches!(commit(&f,&command,&ledger)?,ControlCommit::Replayed{receipt:replay} if replay==receipt)
    );
    // This new command was frozen from the exact stopped State while Active;
    // cleanup maintenance cannot substitute for the original Control Active gate.
    let error = commit(&f, &next, &ledger).unwrap_err();
    assert!(
        format!("{error:#}").contains("Control identity scope is retired"),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn sparse_control_lost_ack_recovers_original_receipt_and_actual_domains_without_current_bodies(
) -> Result<()> {
    let f = Fixture::new();
    let ledger = budget()?;
    seed(&f, &ledger, false)?;
    let command = f.command(ControlAction::Stop);
    let lost = command
        .clone()
        .with_fault(ControlFaultPoint::AfterCommitBeforeAck);
    assert!(commit(&f, &lost, &ledger).is_err());
    let after = version(&f)?;
    let input = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        windows: vec![],
        mandatory_roots: BTreeSet::new(),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    let observation = f.db.load_control_recovery_with_domains(
        command.key(),
        command.invocation(),
        command.command_digest(),
        12,
        &input,
        &WorkCaptureRequest::current(),
        &ledger,
    )?;
    let ControlRecoveryObservation::Current {
        snapshot,
        domains: Some(domains),
        receipt: Some(receipt),
        facts,
        ..
    } = observation
    else {
        panic!("original positive sparse recovery")
    };
    assert_eq!(snapshot.state.status, AgentStatus::Stopped);
    assert!(domains.current_reads.is_none());
    assert_eq!(domains.work.scope(), &scope(&f));
    assert_eq!(domains.tasks.scope(), &scope(&f));
    assert_eq!(domains.scheduling.scope(), &scope(&f));
    assert!(domains.inputs.belongs_to(&scope(&f)));
    assert!(facts.current_queue_messages.is_empty());
    assert_eq!(receipt.key, *command.key());
    assert_eq!(receipt.command_digest, command.command_digest());
    assert_eq!(version(&f)?, after);
    assert!(matches!(
        commit(&f, &command, &ledger)?,
        ControlCommit::Replayed { .. }
    ));
    drop((snapshot, domains, facts));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn sparse_control_recovery_does_not_hide_corrupt_saved_current_coverage() -> Result<()> {
    let f = Fixture::new();
    let ledger = budget()?;
    seed(&f, &ledger, false)?;
    let command = f.command(ControlAction::Stop);
    let ControlCommit::Applied { delta, .. } = commit(&f, &command, &ledger)? else {
        panic!("first ACK")
    };
    drop(delta);
    f.db.connection()?
        .execute("UPDATE current_read_current SET coverage_json='{'", [])?;
    let input = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        windows: vec![],
        mandatory_roots: BTreeSet::new(),
        interrupted_sources: BTreeSet::new(),
        selected_completion_brief_ids: BTreeSet::new(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    assert!(f
        .db
        .load_control_recovery_with_domains(
            command.key(),
            command.invocation(),
            command.command_digest(),
            12,
            &input,
            &WorkCaptureRequest::current(),
            &ledger
        )
        .is_err());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn sparse_unknown_queue_never_masks_required_pruned_or_corrupt_accepted_material() -> Result<()> {
    for mode in ["pruned", "body", "saved", "oversized"] {
        let f = Fixture::new();
        let ledger = budget()?;
        seed(&f, &ledger, true)?;
        f.db.transaction(|tx| {
            upsert_queue_entry_tx(tx,&queue(&f,"raw-unknown-alongside-known"))?;
            match mode {
                "pruned"=> { tx.execute("UPDATE current_read_members SET availability='pruned',content_digest=NULL,projection_digest=NULL WHERE family='queue' AND record_id='original-current-queue'",[])?; }
                "body"=> { tx.execute("UPDATE queue_entries SET status='aborted',payload_json=json_set(payload_json,'$.status','aborted') WHERE message_id='original-current-queue'",[])?; }
                "saved"=> { tx.execute("UPDATE current_read_current SET coverage_json='{'",[])?; }
                "oversized"=> { tx.execute("UPDATE queue_entries SET payload_json=printf('%.*c',?1,'{') WHERE message_id='original-current-queue'",[9*1024*1024])?; }
                _=>unreachable!(),
            }
            Ok(())
        })?;
        let before = version(&f)?;
        let mut read = request();
        read.required_queue_ids
            .insert("original-current-queue".into());
        let error =
            f.db.transaction(|tx| {
                reads::prepare_original_current_read_recovery_tx(
                    tx,
                    &scope(&f),
                    &f.db_state(tx)?,
                    &read,
                    &ledger,
                )
            })
            .unwrap_err();
        match mode {
            "pruned" => assert!(
                matches!(error.downcast_ref::<CurrentReadError>(),Some(CurrentReadError::Pruned(key)) if key==&CurrentReadKey::queue("original-current-queue")),
                "{error:#}"
            ),
            "body" => assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::CanonicalChanged)
                ),
                "{error:#}"
            ),
            "saved" => assert!(
                !matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::UnknownNamespace(_))
                ),
                "{error:#}"
            ),
            "oversized" => assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::ResourceExhausted)
                ),
                "{error:#}"
            ),
            _ => unreachable!(),
        }
        assert_eq!(version(&f)?, before);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn actual_control_keeps_complete_live_event_birth_role_and_refuses_foreign_role_before_write(
) -> Result<()> {
    use crate::runtime::{
        current_event_queries::CurrentEventQueryRegistry, resident_task::TaskCaptureRequest,
    };
    use crate::runtime_db::task_snapshot::TaskDomainLoadRequest;
    let f = Fixture::new();
    let ledger = budget()?;
    seed(&f, &ledger, true)?;
    f.db.transaction(|tx| {
        let state = f.db_state(tx)?;
        let events = (0..70)
            .map(|n| {
                let mut e =
                    AuditEvent::legacy("actual_control_prior_role", serde_json::Value::Null);
                e.id = format!("actual-control-role-{n:03}");
                e
            })
            .collect::<Vec<_>>();
        let keys = events
            .iter()
            .map(|e| CurrentReadKey::event(&e.id))
            .collect();
        let mut proof = reads::prepare_current_read_write_tx(
            tx,
            &scope(&f),
            &state,
            &keys,
            &request(),
            &ledger,
        )?;
        let actual = events
            .iter()
            .map(|e| reads::append_current_read_event_tx(&mut proof, e).map(|(_, actual)| actual))
            .collect::<Result<Vec<_>>>()?;
        reads::register_current_read_writes_tx(proof, &actual, &state)?;
        Ok(())
    })?;
    let registry = CurrentEventQueryRegistry::default();
    let lease = registry.register(&scope(&f), 1, 0, &ledger, (64 * 1024 * 1024, 65536))?;
    let roles = registry
        .snapshot(&scope(&f), 1, &ledger, (64 * 1024 * 1024, 65536))?
        .context("actual role snapshot")?;
    let command = f.command(ControlAction::Stop);
    let work = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    let task = TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    let foreign = InputEvidenceScope {
        agent_id: "foreign-control-role-owner".into(),
        identity_incarnation: scope(&f).identity_incarnation,
    };
    let foreign_lease = registry.register(&foreign, 1, 0, &ledger, (64 * 1024 * 1024, 65536))?;
    let foreign_roles = registry
        .snapshot(&foreign, 1, &ledger, (64 * 1024 * 1024, 65536))?
        .unwrap();
    let before = version(&f)?;
    let observed = ledger.report()?.observed_bytes;
    let error =
        f.db.commit_control_with_domain_requests_and_event_roles(
            &command,
            &work,
            &task,
            &ledger,
            Some(&foreign_roles),
        )
        .err()
        .context("foreign Event role accepted by original Control")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::CanonicalChanged)
        ),
        "{error:#}"
    );
    assert_eq!(version(&f)?, before);
    assert_eq!(ledger.report()?.observed_bytes, observed);
    let ControlCommit::Applied { delta, receipt } =
        f.db.commit_control_with_domain_requests_and_event_roles(
            &command,
            &work,
            &task,
            &ledger,
            Some(&roles),
        )?
    else {
        unreachable!()
    };
    let resident = delta
        .transition
        .effects
        .current_read_maintenance
        .as_ref()
        .and_then(|f| f.resident())
        .context("actual original Control Current ACK")?;
    assert_eq!(resident.coverage().event_query.birth_after, Some(0));
    let captured = resident.capture_event_births(0, &request())?;
    assert_eq!(captured.events.len(), 70 + command.audit_events.len());
    let accepted_version = version(&f)?;
    let ControlCommit::Replayed { receipt: replayed } =
        f.db.commit_control_with_domain_requests_and_event_roles(
            &command,
            &work,
            &task,
            &ledger,
            Some(&roles),
        )?
    else {
        unreachable!()
    };
    assert_eq!(replayed, receipt);
    assert_eq!(version(&f)?, accepted_version);
    let root = std::sync::Arc::downgrade(&resident);
    let body = std::sync::Arc::downgrade(&captured.events[0]);
    drop((delta, captured, roles, lease, foreign_roles, foreign_lease));
    assert!(root.upgrade().is_some() && body.upgrade().is_some());
    assert!(
        ledger.report()?.observed_bytes > 0,
        "the explicitly retained actual resident must remain charged"
    );
    drop(resident);
    assert!(root.upgrade().is_none() && body.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
