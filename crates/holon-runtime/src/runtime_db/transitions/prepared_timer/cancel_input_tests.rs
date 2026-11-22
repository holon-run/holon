use super::*;
use crate::domain::execution_protocol::{
    ExecutionProtocolCommand, ExecutionProtocolState, RegisterWorkItemExecution,
    WorkItemExecutionRecord, WorkItemExecutionState,
};
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::runtime::resident_input_evidence::{
    InputEvidenceFamily, InputEvidenceKey, ResidentInputEvidence,
};
use crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest;
use crate::types::{TimerRecord, TimerStatus};

fn fixture() -> Result<(Fixture, PreparedInputTransition, String)> {
    let f = Fixture::new()?;
    let timer = TimerRecord {
        paused_remaining_ms: None,
        schedule_generation: 0,
        calendar: None,
        id: "cancel-input-timer".into(),
        agent_id: f.scope.agent_id.clone(),
        created_at: f.at,
        duration_ms: 60_000,
        interval_ms: None,
        repeat: false,
        status: TimerStatus::Active,
        content: holon_domain::TimerContent::default(),
        next_fire_at: Some(f.at + chrono::Duration::minutes(1)),
        last_fired_at: None,
        fire_count: 0,
    };
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&PreparedInputTransition::timer(
            f.scope.clone(),
            "cancel-input-create".into(),
            f.at,
            PreparedTimerTransition::create(timer.clone(), None),
        )?)?;
    let mut work = original_work_command(&f, None);
    let WorkItemMutation::Insert { record } = &mut work.mutation else {
        unreachable!()
    };
    let work_id = record.id.clone();
    let inserted = record.clone();
    let execution = WorkItemExecutionRecord {
        source_revision: record.revision,
        state: WorkItemExecutionState::Runnable {
            generation: 1,
            recovery_ref: None,
        },
    };
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&PreparedInputTransition::work(
            f.scope.clone(),
            "cancel-input-work".into(),
            f.at,
            work,
            ExecutionProtocolTransition {
                bootstrap: Some(ExecutionProtocolState::empty(&f.scope.agent_id)),
                commands: vec![ExecutionProtocolCommand::RegisterWorkItem(Box::new(
                    RegisterWorkItemExecution {
                        work_item_id: work_id.clone(),
                        record: execution,
                    },
                ))],
            },
            false,
        )?)?;
    let condition = serde_json::from_value(serde_json::json!({
        "id": "cancel-input-wait", "agent_id": f.scope.agent_id,
        "work_item_id": work_id, "status": "active", "kind": "timer",
        "source": "WaitFor", "subject_ref": timer.id, "waiting_for": "timer",
        "wake_sources": [{"kind": "timer", "wake_at": timer.next_fire_at}],
        "created_at": f.at, "updated_at": f.at
    }))?;
    let mut wait = pending_wait(
        &f.scope.agent_id,
        f.queue()?.command.message_evidence.remove(0),
    );
    wait.task_result_admission = None;
    wait.wait_conditions = vec![condition];
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&PreparedInputTransition::wait(
            f.scope.clone(),
            "cancel-input-wait".into(),
            f.at,
            wait,
            Default::default(),
            None,
        )?)?;
    let mut blocked = inserted.clone();
    blocked.revision = inserted
        .revision
        .checked_add(1)
        .context("work item revision overflow")?;
    blocked.blocked_by = Some("original timer wait".into());
    blocked.blocked_by_wait_id = Some("cancel-input-wait".into());
    blocked.updated_at = f.at;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&PreparedInputTransition::work(
            f.scope.clone(),
            "cancel-input-block".into(),
            f.at,
            WorkItemTransitionCommand {
                agent_id: f.scope.agent_id.clone(),
                mutation: WorkItemMutation::Update {
                    record: blocked,
                    expected_revision: inserted.revision,
                },
                agent_state: None,
                brief_evidence: Vec::new(),
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: false,
                fault: None,
            },
            ExecutionProtocolTransition::default(),
            true,
        )?)?;
    let mut cancelled = timer.clone();
    cancelled.status = TimerStatus::Cancelled;
    cancelled.next_fire_at = None;
    let prepared = PreparedInputTransition::timer(
        f.scope.clone(),
        "cancel-input-original".into(),
        f.at + chrono::Duration::seconds(1),
        PreparedTimerTransition::cancel(
            crate::runtime_db::TimerCancel {
                expected: timer,
                record: cancelled,
                cancelled_at: f.at + chrono::Duration::seconds(1),
                agent_state: None,
            },
            None,
            None,
        ),
    )?;
    Ok((f, prepared, work_id))
}

fn observed(
    f: &Fixture,
    ledger: &InputEvidenceBudgetLedger,
    work_id: &str,
) -> Result<(ResidentInputEvidence, InputEvidenceLoadRequest)> {
    let (owner, request) =
        crate::runtime_db::input_evidence_snapshot::prepare_complete_query_for_fixture(
            &f.db, &f.scope, ledger,
        )?;
    assert!(owner
        .capture_for_request(&request)?
        .lookup(&InputEvidenceKey::new(
            InputEvidenceFamily::WorkExecution,
            work_id
        ))?
        .is_some());
    Ok((owner, request))
}

fn commit(
    f: &Fixture,
    prepared: &PreparedInputTransition,
    ledger: &InputEvidenceBudgetLedger,
    owner: &ResidentInputEvidence,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    };
    let tasks = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: work.record_limit,
        byte_limit: work.byte_limit,
    };
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_input_query(
            prepared,
            &work,
            ledger,
            &tasks,
            owner.original_query_source()?.as_ref(),
            None,
        )
}

fn saved(f: &Fixture) -> Result<BTreeMap<String, Vec<String>>> {
    let mut rows = namespace_rows(f)?;
    let connection = f.db.connection()?;
    for table in [
        "timers",
        "timer_wakes",
        "execution_protocol_partitions",
        "execution_protocol_work_items",
        "execution_protocol_attempts",
        "execution_protocol_outcomes",
        "execution_protocol_command_results",
        "input_query_birth_cursors",
        "input_query_birth_members",
        "scheduling_domain_current",
        "scheduling_domain_records",
        "current_read_qualification",
    ] {
        let mut statement = connection.prepare(&format!("SELECT * FROM {table}"))?;
        let columns = statement.column_count();
        let mut records = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|values| format!("{values:?}"))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        records.sort();
        rows.insert(table.into(), records);
    }
    Ok(rows)
}

#[test]
fn timer_cancel_original_receipt_replays_lost_ack_without_rewriting_input() -> Result<()> {
    let (f, prepared, work_id) = fixture()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (owner, request) = observed(&f, &ledger, &work_id)?;
    let old = owner.capture_for_request(&request)?;
    let InputMutationExecution::Committed { value, receipt } =
        commit(&f, &prepared, &ledger, &owner)?
    else {
        panic!("original cancel must commit")
    };
    let key = InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, &work_id);
    assert!(value
        .effects
        .input_evidence_delta
        .records
        .iter()
        .any(|row| row.locator.key == key));
    let next = owner
        .with_committed(&value.effects.input_evidence_delta, Default::default())?
        .context("original cancellation publication")?;
    assert_ne!(
        old.lookup(&key)?,
        next.capture_for_request(&request)?.lookup(&key)?
    );
    // SQL really committed; discard all publication ACKs, then recover the same nonce.
    drop((value, next));
    let mut later = f.state()?;
    later.turn_index = 9;
    f.db.agent_states().upsert_for_fixture(&later)?;
    let before = saved(&f)?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?
    else {
        panic!("exact replay cannot return old resident effects")
    };
    assert_eq!(replay, receipt);
    assert_eq!(saved(&f)?, before);
    assert_eq!(f.state()?, later);
    Ok(())
}

#[test]
fn timer_cancel_input_or_receipt_failure_rolls_back_the_whole_original_transaction() -> Result<()> {
    for target in [
        "UPDATE ON input_evidence_current WHEN NEW.family='work_execution'",
        "INSERT ON input_mutation_receipts",
    ] {
        let (f, prepared, work_id) = fixture()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let (owner, _) = observed(&f, &ledger, &work_id)?;
        f.db.connection()?.execute_batch(&format!(
            "CREATE TRIGGER reject_cancel_input BEFORE {target}
             BEGIN SELECT RAISE(ABORT, 'injected original cancel failure'); END;"
        ))?;
        let before = saved(&f)?;
        let error = commit(&f, &prepared, &ledger, &owner).unwrap_err();
        assert!(
            format!("{error:#}").contains("injected original cancel failure"),
            "{error:#}"
        );
        assert_eq!(saved(&f)?, before, "{target}");
        f.db.connection()?
            .execute_batch("DROP TRIGGER reject_cancel_input")?;
        assert!(matches!(
            commit(&f, &prepared, &ledger, &owner)?,
            InputMutationExecution::Committed { .. }
        ));
    }
    Ok(())
}

#[test]
fn timer_cancel_work_execution_write_set_keeps_original_resource_budget_atomic() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;

    let (f, prepared, work_id) = fixture()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let (owner, request) = observed(&f, &ledger, &work_id)?;
    let key = InputEvidenceKey::new(InputEvidenceFamily::WorkExecution, &work_id);
    let old = owner.capture_for_request(&request)?.lookup(&key)?.cloned();
    f.db.transaction(|tx| {
        let mut addresses = Vec::new();
        native_input::visit_original(tx, &prepared, &mut |family, id| {
            addresses.push(InputEvidenceKey::new(family, id));
            Ok(())
        })?;
        assert_eq!(addresses, vec![key.clone()]);
        Ok(())
    })?;
    let before = saved(&f)?;
    let observed_bytes = ledger.report()?.observed_bytes;
    // Admit the phase header, but not the original transaction's material.
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &f.scope, 64 * 1024 * 1024, 64)?;
    {
        let _active = phase.enter();
        let error = commit(&f, &prepared, &ledger, &owner).unwrap_err();
        assert!(
            format!("{error:#}")
                .to_ascii_lowercase()
                .contains("resourceexhausted"),
            "{error:#}"
        );
        assert!(phase.finish().is_err());
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, observed_bytes);
    assert_eq!(saved(&f)?, before);
    assert_eq!(
        owner.capture_for_request(&request)?.lookup(&key)?.cloned(),
        old
    );
    assert!(matches!(
        commit(&f, &prepared, &ledger, &owner)?,
        InputMutationExecution::Committed { .. }
    ));
    Ok(())
}
