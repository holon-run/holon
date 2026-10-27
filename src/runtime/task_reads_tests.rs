use super::*;
use crate::{
    domain::execution_protocol::{
        admit_execution, AdmitExecution, AdmittedFences, ExecutionAttempt, ExecutionAttemptState,
        ExecutionBinding, ExecutionOrigin, ExecutionPriority, ExecutionProtocolState,
        ExecutionProvenance, ExecutionSource, ExecutionSourceIdentity, ExecutionTrust,
    },
    runtime_db::{
        context_inputs::ContextDurableRead,
        evidence::{append_message_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        repositories,
        task_result_settlement::upsert_pending_tx,
        task_snapshot::{
            prepare_task_domain_snapshot_tx, prepare_task_write_tx, register_task_writes_tx,
            TaskDomainLoadRequest,
        },
        RuntimeDb,
    },
    types::{AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};
use chrono::{DateTime, Duration, Utc};

fn at() -> DateTime<Utc> {
    "2026-10-01T00:00:00.123Z".parse().unwrap()
}
fn scope() -> super::super::resident_input_evidence::InputEvidenceScope {
    super::super::resident_input_evidence::InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: 1,
    }
}
fn load_request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn fixture() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                "default",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &AgentState::new("default"))?;
        prepare_task_domain_snapshot_tx(tx, &scope(), &load_request())?.require_current()?;
        Ok(())
    })?;
    Ok((dir, db))
}
fn task(id: &str, status: TaskStatus) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status,
        created_at: at(),
        updated_at: at(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some(id.into()),
        detail: None,
        recovery: None,
    }
}

#[test]
fn active_task_order_and_count_survive_without_durable_task_tables() -> Result<()> {
    let (_dir, db) = fixture()?;
    let mut capture = db.transaction(|tx| {
        let mut queued = task("a-queued", TaskStatus::Queued);
        queued.updated_at = at() + Duration::seconds(1);
        let mut running = task("b-running", TaskStatus::Running);
        running.updated_at = at() + Duration::seconds(2) + Duration::nanoseconds(700);
        let mut cancelling = task("c-cancelling", TaskStatus::Cancelling);
        cancelling.updated_at = at() + Duration::seconds(2) + Duration::nanoseconds(100);
        cancelling.created_at = at() + Duration::milliseconds(1);
        // Same-agent active selection replaces the removed Task shadow cache.
        let mut foreign = task("foreign-task", TaskStatus::Running);
        foreign.agent_id = "other".into();
        foreign.updated_at = at() + Duration::seconds(100);
        repositories::upsert_task_tx(tx, &foreign)?;
        let values = [
            queued,
            running,
            cancelling,
            task("terminal", TaskStatus::Completed),
        ];
        let keys = values
            .iter()
            .map(|row| TaskKey::new(TaskFamily::Task, &row.id))
            .collect();
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        for row in &values {
            repositories::upsert_task_tx(tx, row)?;
        }
        register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("default"),
            &load_request(),
        )?;
        let resident = prepare_task_domain_snapshot_tx(tx, &scope(), &load_request())?
            .require_current()?
            .resident;
        let captured = resident.capture(&active_task_request())?;
        let durable = ContextDurableRead::new(tx, "default").active_tasks("default")?;
        assert_eq!(
            active_task_records(&captured)?
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            durable
        );
        Ok(captured)
    })?;
    db.transaction(|tx| {
        tx.execute_batch("DROP TABLE task_result_settlements; DROP TABLE tasks;")?;
        Ok(())
    })?;
    assert_eq!(active_task_count(&capture)?, 3);
    let selected = active_task_records(&capture)?;
    assert_eq!(
        selected
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec!["c-cancelling", "b-running", "a-queued"]
    );
    assert_eq!(selected.into_iter().take(1).count(), 1);
    assert!(
        task_record(&capture, "terminal").is_err(),
        "unselected history is not absent"
    );
    assert!(active_task_updated_after(
        &capture,
        at() + Duration::seconds(2)
    )?);
    assert!(!active_task_updated_after(
        &capture,
        at() + Duration::seconds(2) + Duration::nanoseconds(800)
    )?);
    capture.coverage.task_metadata.remove("c-cancelling");
    assert!(
        active_task_updated_after(&capture, at() + Duration::seconds(10)).is_err(),
        "missing complete active metadata is not a false signal"
    );
    Ok(())
}

/// Actual Task/result first writes and a real Open protocol activation. The
/// context test borrows this capture and substitutes a database with no tables.
pub(crate) fn admitted_fixture() -> Result<(CapturedTasks, Vec<MessageEnvelope>)> {
    let (_dir, db) = fixture()?;
    db.transaction(|tx| {
        let mut reply = MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "original reply outside the history window".into(),
            },
        );
        reply.id = "reply-outside-window".into();
        reply.created_at = at();
        let (reply, _) = append_message_tx(tx, &reply)?;
        let mut messages = Vec::new();
        let mut records = Vec::new();
        let mut values = Vec::new();
        for (index, id, reply_id) in [
            (0, "current-task", None),
            (1, "skip-task", Some("result-current-task")),
            (2, "first-task", Some(reply.id.as_str())),
            (3, "duplicate-task", Some(reply.id.as_str())),
        ] {
            let clock = at() + Duration::milliseconds(index);
            let mut value = task(id, TaskStatus::Completed);
            value.kind = TaskKind::AgentMessageWait;
            value.detail = Some(serde_json::json!({
                "rejoin_obligation_id": id, "rejoin_generation": 1,
                "parent_turn_id": "parent-turn", "reply_content_source": "original_message",
                "message_id": reply_id,
            }));
            let mut source = MessageEnvelope::new(
                "default",
                MessageKind::TaskResult,
                MessageOrigin::Task { task_id: id.into() },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "task terminal summary".into(),
                },
            )
            .with_admission(
                crate::types::MessageDeliverySurface::TaskRejoin,
                crate::types::AdmissionContext::RuntimeOwned,
            );
            source.id = format!("result-{id}");
            source.task_id = Some(id.into());
            source.created_at = clock;
            source.metadata = Some(serde_json::json!({"task_id":id,
                "task_kind":crate::types::AGENT_MESSAGE_WAIT_TASK_KIND,
                "task_status":"completed", "task_detail":value.detail}));
            let (source, _) = append_message_tx(tx, &source)?;
            value.parent_message_id = Some(source.id.clone());
            let mut record = TaskResultSettlementRecord::pending(&value, &source, clock)?
                .context("actual result")?;
            record.state = TaskResultSettlementState::CallerAdmitted;
            record.activation_id = Some("original-open-activation".into());
            record.admitted_at = Some(clock);
            values.push(value);
            records.push(record);
            messages.push(source);
        }
        let keys: BTreeSet<_> = values
            .iter()
            .map(|row| TaskKey::new(TaskFamily::Task, &row.id))
            .chain(
                records
                    .iter()
                    .map(|row| TaskKey::new(TaskFamily::ResultSettlement, &row.result_identity)),
            )
            .collect();
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        for row in &values {
            repositories::upsert_task_tx(tx, row)?;
        }
        for row in &records {
            upsert_pending_tx(tx, row)?;
        }
        let source = &messages[0];
        let sequence = source.message_seq.context("actual source sequence")?;
        let admitted = admit_execution(
            &ExecutionProtocolState::empty("default"),
            &AdmitExecution {
                attempt: ExecutionAttempt {
                    input_projection: None,
                    attempt_id: "original-open-activation".into(),
                    agent_id: "default".into(),
                    source_message_id: Some(source.id.clone()),
                    source: ExecutionSource {
                        identity: ExecutionSourceIdentity::QueueMessage {
                            message_id: source.id.clone(),
                        },
                        generation: sequence,
                    },
                    binding: ExecutionBinding::Conversation {
                        interaction_id: source.id.clone(),
                    },
                    provenance: ExecutionProvenance {
                        origin: ExecutionOrigin::Task,
                        trust: ExecutionTrust::RuntimeInstruction,
                        priority: ExecutionPriority::Normal,
                        correlation_id: None,
                        causation_id: None,
                    },
                    admitted_fences: AdmittedFences {
                        source_revision: sequence,
                        work_item_source_revision: None,
                        work_item_generation: None,
                        rejoin: None,
                        agent_control_revision: 1,
                        host_registry_revision: 1,
                    },
                    state: ExecutionAttemptState::Open,
                    run_id: None,
                    turn_id: None,
                    recovery_of_attempt_id: None,
                    terminal_outcome_id: None,
                    admitted_at: at().to_rfc3339(),
                    terminal_at: None,
                },
            },
        )
        .map_err(anyhow::Error::msg)?;
        crate::runtime_db::transitions::persist_state_tx(tx, &admitted.state)?;
        let mut state = AgentState::new("default");
        state.total_message_count = 5;
        upsert_agent_state_tx(tx, &state)?;
        register_task_writes_tx(tx, &proof, &keys, &state, &load_request())?;
        let resident = prepare_task_domain_snapshot_tx(tx, &scope(), &load_request())?
            .require_current()?
            .resident;
        let captured = resident.capture(&TaskCaptureRequest::current())?;
        messages.push(reply);
        Ok((captured, messages))
    })
}

#[test]
fn captured_task_result_message_read_requires_complete_original_facts() -> Result<()> {
    let (captured, messages) = admitted_fixture()?;
    let expected = (*admitted_results(&captured, "original-open-activation")?[0]).clone();
    let index = captured
        .records
        .iter()
        .position(|row| row.locator.key == TaskKey::new(TaskFamily::Task, &expected.task_id))
        .context("selected original Task")?;
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let selector = ledger.register_owned_bytes(
        &captured.scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: 128,
        },
    )?;
    let mut read = CurrentTaskRecordRead {
        tasks: captured,
        index: Some(index),
        _selector: selector,
    };
    assert_eq!(read.result_for_message(&messages[0].id)?, Some(&expected));
    assert!(read.result_for_message(&messages[1].id)?.is_none());
    assert!(read.result_for_message("not-a-result")?.is_none());

    let metadata = read
        .tasks
        .coverage
        .result_metadata
        .remove(&expected.result_identity)
        .context("original Result metadata")?;
    assert!(read.result_for_message(&messages[0].id).is_err());
    assert!(read.result_for_message("not-a-result").is_err());
    read.tasks
        .coverage
        .result_metadata
        .insert(expected.result_identity.clone(), metadata);

    let ids = read
        .tasks
        .coverage
        .task_result_ids
        .remove(&expected.task_id)
        .context("original Task Result index")?;
    assert!(read.result_for_message(&messages[0].id).is_err());
    read.tasks
        .coverage
        .task_result_ids
        .insert(expected.task_id.clone(), ids);

    read.tasks.scope.identity_incarnation += 1;
    assert!(read.result_for_message(&messages[0].id).is_err());
    read.tasks.scope.identity_incarnation -= 1;
    read.tasks.records.retain(|row| {
        row.locator.key != TaskKey::new(TaskFamily::ResultSettlement, &expected.result_identity)
    });
    assert!(read.result_for_message(&messages[0].id).is_err());
    Ok(())
}

#[test]
fn context_admitted_selection_keeps_open_activation_and_exact_body_selection() -> Result<()> {
    let (full, _) = admitted_fixture()?;
    let rows = admitted_results(&full, "original-open-activation")?;
    assert_eq!(rows.len(), 4);
    assert!(
        full.unsettled_for_owner(None, false)?.is_empty(),
        "retry view excludes Open"
    );
    assert!(rows
        .iter()
        .all(|row| full.coverage.result_metadata[&row.result_identity].activation_open));
    assert!(
        admitted_results(&full, "unobserved-activation")?.is_empty(),
        "complete index proves empty"
    );
    let mut unavailable = full;
    unavailable
        .records
        .retain(|row| row.locator.key.family != TaskFamily::ResultSettlement);
    assert!(
        admitted_results(&unavailable, "original-open-activation").is_err(),
        "metadata without selected bodies cannot become empty/SQL fallback"
    );
    Ok(())
}

#[test]
fn context_task_selector_is_charged_before_clone_and_outlives_source_capture() -> Result<()> {
    let (full, messages) = admitted_fixture()?;
    let mut state = AgentState::new("default");
    state.current_execution_binding = Some(crate::types::WorkItemExecutionBinding {
        activation_id: Some("original-open-activation".into()),
        admission_provenance: None,
        source_message_id: messages[0].id.clone(),
        turn_id: "selector-current-turn".into(),
        owner: None,
        work_item_id: None,
        claimed_work_revision: None,
    });
    let ledger = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
        crate::config::StateMemoryLimits::default(),
    )?;
    assert!(context_task_request(&full, &state, &ledger, 15, 128 * 1024).is_err());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert!(context_task_request(&full, &state, &ledger, 4096, 128).is_err());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let (request, guard) = context_task_request(&full, &state, &ledger, 4096, 128 * 1024)?;
    assert_eq!(request.required_keys.len(), 4);
    assert_eq!(
        request.required_keys,
        full.coverage.activation_result_ids["original-open-activation"]
            .iter()
            .map(|id| TaskKey::new(TaskFamily::ResultSettlement, id))
            .collect()
    );
    let charged = ledger.report()?.observed_bytes;
    assert!(charged > 0);
    drop(full);
    assert_eq!(
        ledger.report()?.observed_bytes,
        charged,
        "request metadata owns its guard independently of the captured root"
    );
    drop(request);
    drop(guard);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn result_deadline_and_retry_order_match_normalized_sql_without_durable_tables() -> Result<()> {
    let (_dir, db) = fixture()?;
    let (capture, expected_order, deadline) = db.transaction(|tx| {
        let mut rows = Vec::new();
        let mut tasks = Vec::new();
        for id in ["retry-a", "retry-b"] {
            let mut task = task(id, TaskStatus::Completed);
            task.detail = Some(serde_json::json!({"rejoin_obligation_id":id,
                "rejoin_generation":1,"parent_turn_id":"original-parent"}));
            let mut source = MessageEnvelope::new("default", MessageKind::TaskResult,
                MessageOrigin::Task{task_id:id.into()}, AuthorityClass::RuntimeInstruction,
                Priority::Normal, MessageBody::Text{text:"actual terminal Result".into()});
            source.id=format!("result-{id}");source.task_id=Some(id.into());source.created_at=at();
            source.metadata=Some(serde_json::json!({"task_id":id,"task_kind":task.kind.as_str(),
                "task_status":"completed","task_detail":task.detail}));
            let (source,_) = append_message_tx(tx,&source)?;
            task.parent_message_id=Some(source.id.clone());
            rows.push(TaskResultSettlementRecord::pending(&task,&source,at())?.context("actual result")?);
            tasks.push(task);
        }
        rows.sort_by(|a,b|a.result_identity.cmp(&b.result_identity));
        // Raw nanos deliberately oppose canonical SQL's same-millisecond ID tie.
        for (index, row) in rows.iter_mut().enumerate() {
            row.created_at=at()+Duration::nanoseconds(if index==0 {900} else {100});
            row.updated_at=row.created_at;
            row.next_recheck_at=Some(at()+Duration::seconds(30)+Duration::nanoseconds(700));
        }
        let keys=tasks.iter().map(|task|TaskKey::new(TaskFamily::Task,&task.id))
            .chain(rows.iter().map(|row|TaskKey::new(TaskFamily::ResultSettlement,&row.result_identity))).collect();
        let proof=prepare_task_write_tx(tx,&scope(),&keys)?;
        for task in &tasks {repositories::upsert_task_tx(tx,task)?;}
        for row in &rows {upsert_pending_tx(tx,row)?;}
        let mut state=AgentState::new("default");state.total_message_count=2;upsert_agent_state_tx(tx,&state)?;
        register_task_writes_tx(tx,&proof,&keys,&state,&load_request())?;
        let current=prepare_task_domain_snapshot_tx(tx,&scope(),&load_request())?.require_current()?.resident;
        let mut query=tx.prepare("SELECT result_identity FROM task_result_settlements WHERE agent_id='default' AND state='persisted_pending' ORDER BY created_at ASC,result_identity ASC")?;
        let order=query.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let deadline:String=tx.query_row("SELECT MIN(next_recheck_at) FROM task_result_settlements WHERE agent_id='default' AND state='persisted_pending'",[],|r|r.get(0))?;
        Ok((current.capture(&TaskCaptureRequest::current())?,order,
            DateTime::parse_from_rfc3339(&deadline)?.with_timezone(&Utc)))
    })?;
    db.transaction(|tx| {
        tx.execute_batch("DROP TABLE task_result_settlements; DROP TABLE tasks")?;
        Ok(())
    })?;
    assert_eq!(capture.next_recheck_at()?, Some(deadline));
    assert_eq!(
        capture
            .unsettled_for_owner(None, false)?
            .into_iter()
            .map(|row| row.result_identity.clone())
            .collect::<Vec<_>>(),
        expected_order
    );
    let (open, _) = admitted_fixture()?;
    assert!(
        open.next_recheck_at()?.is_none(),
        "Open CallerAdmitted stays resident but is excluded from retries"
    );
    Ok(())
}

#[test]
fn due_recovery_selection_matches_sql_limit_and_millis_without_tables() -> Result<()> {
    let (_dir, db) = fixture()?;
    let now = at() + Duration::seconds(30);
    let (mut captured,expected)=db.transaction(|tx| {
        let mut values=Vec::new();let mut rows=Vec::new();
        for index in 0..12 {
            let id=format!("due-{index:02}");
            let mut value=task(&id,TaskStatus::Completed);
            value.detail=Some(serde_json::json!({"rejoin_obligation_id":id,"rejoin_generation":1,"parent_turn_id":"original-parent"}));
            let mut source=MessageEnvelope::new("default",MessageKind::TaskResult,
                MessageOrigin::Task{task_id:id.clone()},AuthorityClass::RuntimeInstruction,
                Priority::Normal,MessageBody::Text{text:"actual due Result".into()});
            source.id=format!("result-{id}");source.task_id=Some(id.clone());source.created_at=at();
            source.metadata=Some(serde_json::json!({"task_id":id,"task_kind":value.kind.as_str(),"task_status":"completed","task_detail":value.detail}));
            let (source,_)=append_message_tx(tx,&source)?;
            value.parent_message_id=Some(source.id.clone());
            let mut result=TaskResultSettlementRecord::pending(&value,&source,at())?.context("actual due Result")?;
            // Deliberately later nanos within SQL's due millisecond remain due.
            result.next_recheck_at=Some(now+Duration::nanoseconds(900-index*10));
            result.created_at=at()+Duration::nanoseconds(900-index*10);result.updated_at=result.created_at;
            values.push(value);rows.push(result);
        }
        let keys=values.iter().map(|t|TaskKey::new(TaskFamily::Task,&t.id))
            .chain(rows.iter().map(|r|TaskKey::new(TaskFamily::ResultSettlement,&r.result_identity))).collect();
        let proof=prepare_task_write_tx(tx,&scope(),&keys)?;
        for value in &values {repositories::upsert_task_tx(tx,value)?;}
        for result in &rows {upsert_pending_tx(tx,result)?;}
        let mut state=AgentState::new("default");state.total_message_count=12;upsert_agent_state_tx(tx,&state)?;
        register_task_writes_tx(tx,&proof,&keys,&state,&load_request())?;
        let mut query=tx.prepare("SELECT result_identity FROM task_result_settlements WHERE agent_id=?1 AND next_recheck_at IS NOT NULL AND next_recheck_at<=?2 AND (state='persisted_pending' OR (state='caller_admitted' AND NOT EXISTS(SELECT 1 FROM execution_protocol_attempts a WHERE a.agent_id=task_result_settlements.agent_id AND a.attempt_id=task_result_settlements.activation_id AND a.lifecycle_state='open'))) ORDER BY next_recheck_at ASC,created_at ASC,result_identity ASC LIMIT 8")?;
        let expected=query.query_map(rusqlite::params!["default",crate::runtime_db::migrations::timestamp(now)],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let resident=prepare_task_domain_snapshot_tx(tx,&scope(),&load_request())?.require_current()?.resident;
        Ok((resident.capture(&TaskCaptureRequest::current())?,expected))
    })?;
    db.transaction(|tx| {
        tx.execute_batch("DROP TABLE task_result_settlements;DROP TABLE tasks")?;
        Ok(())
    })?;
    let actual = super::super::task_result_recovery::due_recovery_results(&captured, now)?;
    assert_eq!(
        actual
            .into_iter()
            .flatten()
            .map(|r| r.result_identity.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(super::super::task_result_recovery::due_recovery_results(
        &captured,
        now - Duration::milliseconds(1)
    )?
    .iter()
    .all(Option::is_none));
    captured.coverage.unsettled_complete = false;
    assert!(
        super::super::task_result_recovery::due_recovery_results(&captured, now)
            .unwrap_err()
            .downcast_ref::<TaskReadError>()
            .is_some()
    );
    let (open, _) = admitted_fixture()?;
    assert!(super::super::task_result_recovery::due_recovery_results(
        &open,
        now + Duration::days(1)
    )?
    .iter()
    .all(Option::is_none));
    Ok(())
}

#[tokio::test]
async fn current_task_monitor_reads_are_pure_and_missing_keys_do_not_fallback() -> Result<()> {
    let harness = crate::runtime::tests::support::LifecycleHarness::new();
    let runtime = harness.runtime();
    let value = task("current-monitor-task", TaskStatus::Running);
    runtime
        .persist_task_transition(&value, "current_monitor_fixture")
        .await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let before = ledger.report()?.observed_bytes;
    let read = runtime.capture_current_task_record(&value.id)?;
    assert_eq!(read.record(), Some(&value));
    assert!(ledger.report()?.observed_bytes > before);
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("DROP TABLE tasks")?;
        Ok(())
    })?;
    let fresh = runtime.capture_current_task_record(&value.id)?;
    assert_eq!(fresh.record(), Some(&value));
    let error = runtime
        .capture_current_task_record("unprepared-task-id")
        .err()
        .context("ordinary miss must be typed")?;
    assert!(
        matches!(error.downcast_ref::<TaskReadError>(), Some(TaskReadError::NeedsPreparation(key)) if key.id=="unprepared-task-id")
    );
    drop(fresh);
    drop(read);
    assert_eq!(ledger.report()?.observed_bytes, before);
    Ok(())
}

#[tokio::test]
async fn ordinary_task_list_captures_published_facts_without_owner_or_body_sql() -> Result<()> {
    let harness = crate::runtime::tests::support::LifecycleHarness::new();
    let runtime = harness.runtime();
    let value = task("pure-published-list-task", TaskStatus::Queued);
    runtime
        .persist_task_transition(&value, "task_queued")
        .await?;
    let before = runtime.active_task_list_snapshot(8).await?;
    assert_eq!(before.total_active, 1);
    assert_eq!(before.entries[0].id, value.id);
    let token = runtime.inner.mutation_owner.capture()?.unwrap().token;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE tasks RENAME TO tasks_ordinary_capture_forbidden;
             ALTER TABLE task_result_settlements RENAME TO results_ordinary_capture_forbidden;
             ALTER TABLE agent_states RENAME TO states_ordinary_capture_forbidden;
             ALTER TABLE agent_identities RENAME TO identities_ordinary_capture_forbidden;",
        )?;
        Ok(())
    })?;
    let repeated = runtime.active_task_list_snapshot(8).await?;
    assert_eq!(repeated.total_active, before.total_active);
    assert_eq!(repeated.entries, before.entries);
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    Ok(())
}

#[tokio::test]
async fn canonical_active_record_carrier_preserves_sql_order_limit_and_wire_lifetime_without_body_sql(
) -> Result<()> {
    use axum::response::IntoResponse;
    let harness = crate::runtime::tests::support::LifecycleHarness::new();
    let runtime = harness.runtime();
    let mut first = task("active-record-b", TaskStatus::Running);
    first.updated_at += Duration::nanoseconds(800);
    first.detail = Some(serde_json::json!({"canonical_detail":"preserved","custom":"detail-only"}));
    let mut second = task("active-record-a", TaskStatus::Cancelling);
    second.updated_at += Duration::nanoseconds(100);
    let queued = task("active-record-c", TaskStatus::Queued);
    let terminal = task("active-record-terminal", TaskStatus::Completed);
    for row in [&first, &second, &queued, &terminal] {
        runtime
            .persist_task_transition(row, "active_record_list_fixture")
            .await?;
    }
    let expected = runtime.storage().latest_active_task_records(2)?;
    assert_eq!(
        expected
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["active-record-a", "active-record-b"]
    );
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let read = runtime.active_task_records_read(2).await?;
    assert_eq!(read.records, expected);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let wire = axum::Json(&read.records).into_response();
    assert_eq!(wire.status(), axum::http::StatusCode::OK);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop(read);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE tasks RENAME TO active_list_tasks_forbidden;
            ALTER TABLE task_result_settlements RENAME TO active_list_results_forbidden;
            ALTER TABLE agent_states RENAME TO active_list_state_forbidden;
            ALTER TABLE agent_identities RENAME TO active_list_identity_forbidden;",
        )?;
        Ok(())
    })?;
    assert_eq!(runtime.active_task_records_read(2).await?.records, expected);
    assert!(runtime
        .active_task_records_read(0)
        .await?
        .records
        .is_empty());
    let tool = crate::tool::tools::task_list::execute(
        &runtime,
        "default",
        &AuthorityClass::RuntimeInstruction,
        &serde_json::json!({"limit":2}),
    )
    .await?;
    assert_eq!(tool.envelope.result.as_ref().unwrap()["returned"], 2);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    drop((wire, tool));
    Ok(())
}
