//! Deliberately malformed in-memory closures exercise the pure read boundary.
use super::*;
use crate::domain::execution_protocol::*;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "execution-reader".into(),
        identity_incarnation: 1,
    }
}
fn token() -> PublicationToken {
    PublicationToken {
        identity_incarnation: 1,
        owner_epoch: 1,
        generation: 1,
    }
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    InputEvidenceBudgetLedger::new(Default::default()).map_err(Into::into)
}
fn empty() -> ResidentInputEvidence {
    ResidentInputEvidence {
        output_recall_roots: Default::default(),
        output_recall_task: None,
        scope: scope(),
        records: Default::default(),
        budget: None,
        budget_views: Vec::new(),
        coverage: InputEvidenceCoverage {
            query: Default::default(),
            selected_completion_brief_ids: Default::default(),
            completion_source_observations: Vec::new(),
            recovery_source_absences: Default::default(),
            recovery_lineage_messages: Default::default(),
            execution: Default::default(),
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            turn_transcript_keys: Default::default(),
            message_turn_keys: Default::default(),
            unavailable: Default::default(),
            record_limit: 65_536,
            byte_limit: 64 * 1024 * 1024,
        },
    }
}
fn qualified(record: PreparedInputEvidence) -> Arc<QualifiedInputEvidence> {
    let bytes = serialized_bytes(&record).unwrap() as usize;
    Arc::new(QualifiedInputEvidence {
        locator: EvidenceLocator {
            scope: scope(),
            key: record.key(),
            revision: 1,
            content_digest: "unit-qualified".into(),
        },
        record,
        created_at: "2026-10-14T00:00:00Z".into(),
        order_sequence: None,
        turn_index: None,
        owner: None,
        resident_bytes: bytes,
        attempt_turn_keys: Some(Default::default()),
        tool_run_report: None,
    })
}
fn open(binding: ExecutionBinding) -> Arc<QualifiedInputEvidence> {
    qualified(PreparedInputEvidence::Attempt(ExecutionAttempt {
        attempt_id: "actual-open".into(),
        agent_id: scope().agent_id,
        source_message_id: Some("accepted-source".into()),
        source: ExecutionSource {
            identity: ExecutionSourceIdentity::QueueMessage {
                message_id: "accepted-source".into(),
            },
            generation: 1,
        },
        binding,
        provenance: ExecutionProvenance {
            origin: ExecutionOrigin::Operator,
            trust: ExecutionTrust::OperatorInstruction,
            priority: ExecutionPriority::Normal,
            correlation_id: None,
            causation_id: None,
        },
        admitted_fences: AdmittedFences {
            source_revision: 1,
            work_item_source_revision: None,
            work_item_generation: None,
            rejoin: None,
            agent_control_revision: 0,
            host_registry_revision: 0,
        },
        state: ExecutionAttemptState::Open,
        run_id: None,
        turn_id: None,
        recovery_of_attempt_id: None,
        terminal_outcome_id: None,
        admitted_at: "2026-10-14T00:00:00Z".into(),
        terminal_at: None,
    }))
}
fn with_open() -> ResidentInputEvidence {
    let mut input = empty();
    let record = open(ExecutionBinding::Conversation {
        interaction_id: "accepted-source".into(),
    });
    input.coverage.execution.initialized = true;
    input
        .coverage
        .execution
        .open_attempt_ids
        .insert(record.locator.key.id.clone());
    input.records.insert(record.locator.key.clone(), record);
    input
}

#[test]
fn complete_open_cannot_become_empty_when_its_body_is_missing() -> Result<()> {
    let ledger = ledger()?;
    let mut input = with_open();
    input.records.clear();
    let error = input
        .execution_command_source(token(), &ledger, 65536, 64 * 1024 * 1024)
        .capture(None, &[])
        .unwrap_err();
    assert!(matches!(error.downcast_ref::<InputEvidenceReadError>(),
        Some(InputEvidenceReadError::NeedsPreparation(key)) if key.family == InputEvidenceFamily::Attempt));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    input.coverage.execution.open_attempt_ids = ["oversized-missing-open".repeat(1024)].into();
    let oversized = input
        .execution_command_source(token(), &ledger, 65536, 4096)
        .capture(None, &[])
        .unwrap_err();
    assert!(matches!(
        oversized.downcast_ref::<ExecutionCommandReadError>(),
        Some(ExecutionCommandReadError::ResourceExhausted)
    ));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let mut uninitialized = empty();
    assert!(!uninitialized
        .execution_command_source(token(), &ledger, 65536, 64 * 1024 * 1024)
        .capture(None, &[])?
        .is_initialized());
    let record = open(ExecutionBinding::Command);
    uninitialized
        .records
        .insert(record.locator.key.clone(), record);
    assert!(uninitialized
        .execution_command_source(token(), &ledger, 65536, 64 * 1024 * 1024)
        .capture(None, &[])
        .unwrap_err()
        .to_string()
        .contains("uninitialized"));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn pruned_work_is_distinct_from_proven_absence_and_foreign_body() -> Result<()> {
    let ledger = ledger()?;
    let mut input = empty();
    input.coverage.execution.initialized = true;
    for presence in [ExecutionWorkPresence::Absent, ExecutionWorkPresence::Pruned] {
        input.coverage.execution.work_items.insert(
            "work".into(),
            ExecutionWorkObservation {
                revision: 1,
                presence,
            },
        );
        let read = input
            .execution_command_source(token(), &ledger, 65536, 64 * 1024 * 1024)
            .capture(Some("work"), &[])?;
        if presence == ExecutionWorkPresence::Absent {
            assert!(read.work_execution("work")?.is_none());
        } else {
            assert!(
                matches!(read.work_execution("work").unwrap_err().downcast_ref::<ExecutionCommandReadError>(),
            Some(ExecutionCommandReadError::PrunedWork(id)) if id == "work")
            );
        }
    }
    let mut record = qualified(PreparedInputEvidence::WorkExecution {
        work_item_id: "work".into(),
        record: WorkItemExecutionRecord {
            source_revision: 1,
            state: WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            },
        },
    });
    Arc::make_mut(&mut record).locator.scope.agent_id = "foreign".into();
    input
        .coverage
        .execution
        .work_items
        .get_mut("work")
        .unwrap()
        .presence = ExecutionWorkPresence::Present;
    input.records.insert(record.locator.key.clone(), record);
    assert!(input
        .execution_command_source(token(), &ledger, 65536, 64 * 1024 * 1024)
        .capture(Some("work"), &[])
        .unwrap_err()
        .to_string()
        .contains("locator differs"));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn selected_execution_retains_only_selected_arcs_after_domain_release() -> Result<()> {
    let ledger = ledger()?;
    let mut input = with_open();
    let unrelated = qualified(PreparedInputEvidence::Turn(crate::types::TurnRecord::new(
        "execution-reader",
        "unselected",
        9,
    )));
    let unrelated_weak = Arc::downgrade(&unrelated);
    input
        .records
        .insert(unrelated.locator.key.clone(), unrelated);
    let open_weak = Arc::downgrade(
        input
            .records
            .values()
            .find(|v| matches!(&v.record, PreparedInputEvidence::Attempt(_)))
            .unwrap(),
    );
    let read = input
        .execution_command_source(token(), &ledger, 65536, 64 * 1024 * 1024)
        .capture(None, &[])?;
    drop(input);
    assert!(unrelated_weak.upgrade().is_none());
    assert!(open_weak.upgrade().is_some());
    assert_eq!(read.open_attempt().unwrap().attempt_id, "actual-open");
    assert!(ledger.report()?.observed_bytes > 0);
    drop(read);
    assert!(open_weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn command_selector_and_selected_body_share_clamped_allowance_before_clone() -> Result<()> {
    let ledger = ledger()?;
    let input = with_open();
    let original = Arc::strong_count(input.records.values().next().unwrap());
    let error = input
        .execution_command_source(token(), &ledger, 16, 64 * 1024 * 1024)
        .capture(None, &[])
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ExecutionCommandReadError>(),
        Some(ExecutionCommandReadError::ResourceExhausted)
    ));
    assert_eq!(
        Arc::strong_count(input.records.values().next().unwrap()),
        original
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let large = "x".repeat(9 * 1024 * 1024);
    let error = input
        .execution_command_source(token(), &ledger, usize::MAX, usize::MAX)
        .capture(None, &[&large])
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ExecutionCommandReadError>(),
        Some(ExecutionCommandReadError::ResourceExhausted)
    ));
    assert_eq!(
        Arc::strong_count(input.records.values().next().unwrap()),
        original
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
