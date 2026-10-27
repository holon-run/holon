//! A finite activation lookup still reserves the bytes of its actual header.
use super::*;

#[test]
fn result_activation_header_budget_precedes_oversized_projection_decode() -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let mut source = MessageEnvelope::new(
            "task-agent", MessageKind::TaskResult,
            MessageOrigin::Task {task_id:"header-task".into()},
            AuthorityClass::RuntimeInstruction,Priority::Normal,
            MessageBody::Text {text:"actual original source".into()},
        );
        source.id="header-source".into();source.task_id=Some("header-task".into());source.created_at=at();
        let (source,_)=append_message_tx(tx,&source)?;
        let mut value=task("header-task",TaskStatus::Completed);
        value.parent_message_id=Some(source.id.clone());
        value.detail=Some(serde_json::json!({"rejoin_obligation_id":value.id,"rejoin_generation":1,"parent_turn_id":"parent-turn"}));
        let mut result=TaskResultSettlementRecord::pending(&value,&source,at())?.context("original pending")?;
        result.state=TaskResultSettlementState::CallerAdmitted;
        result.activation_id=Some("header-activation".into());result.admitted_at=Some(at());
        let sequence=source.message_seq.context("actual source sequence")?;
        let admitted=admit_execution(&ExecutionProtocolState::empty("task-agent"),&AdmitExecution {attempt:ExecutionAttempt {
    input_projection: None,
            attempt_id:"header-activation".into(),agent_id:"task-agent".into(),source_message_id:Some(source.id.clone()),
            source:ExecutionSource {identity:ExecutionSourceIdentity::QueueMessage {message_id:source.id.clone()},generation:sequence},
            binding:ExecutionBinding::Conversation {interaction_id:source.id.clone()},
            provenance:ExecutionProvenance {origin:ExecutionOrigin::Task,trust:ExecutionTrust::RuntimeInstruction,priority:ExecutionPriority::Normal,correlation_id:None,causation_id:None},
            admitted_fences:AdmittedFences {source_revision:sequence,work_item_source_revision:None,work_item_generation:None,rejoin:None,agent_control_revision:1,host_registry_revision:1},
            state:ExecutionAttemptState::Open,run_id:None,turn_id:None,recovery_of_attempt_id:None,terminal_outcome_id:None,
            admitted_at:at().to_rfc3339(),terminal_at:None,
        }}).map_err(anyhow::Error::msg)?;
        crate::runtime_db::transitions::persist_state_tx(tx,&admitted.state)?;
        let keys=BTreeSet::from([TaskKey::new(TaskFamily::Task,&value.id),TaskKey::new(TaskFamily::ResultSettlement,&result.result_identity)]);
        let proof=prepare_task_write_tx(tx,&scope(),&keys)?;
        repositories::upsert_task_tx(tx,&value)?;upsert_pending_tx(tx,&result)?;
        let mut state=AgentState::new("task-agent");state.total_message_count=1;upsert_agent_state_tx(tx,&state)?;
        register_task_writes_tx(tx,&proof,&keys,&state,&request())?;
        Ok(())
    })?;
    let original:String=db.connection()?.query_row(
        "SELECT payload_json FROM execution_protocol_attempts WHERE agent_id='task-agent' AND attempt_id='header-activation'",[],|r|r.get(0))?;
    let revision: u64 = db.connection()?.query_row(
        "SELECT revision FROM task_domain_current WHERE agent_id='task-agent'",
        [],
        |r| r.get(0),
    )?;
    // Result body is small. Only the actual Attempt's projected String is large;
    // its additional invalid status would otherwise fail during the header read.
    db.connection()?.execute("UPDATE execution_protocol_attempts SET payload_json=json_set(payload_json,'$.agent_id',?1,'$.state',7) WHERE agent_id='task-agent' AND attempt_id='header-activation'",
        ["x".repeat(512*1024)])?;
    let mut tiny = request();
    tiny.byte_limit = 1024 * 1024;
    let error = db
        .transaction(|tx| prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &tiny, &ledger))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.connection()?.query_row(
            "SELECT revision FROM task_domain_current WHERE agent_id='task-agent'",
            [],
            |r| r.get::<_, u64>(0)
        )?,
        revision
    );
    db.connection()?.execute("UPDATE execution_protocol_attempts SET payload_json=?1 WHERE agent_id='task-agent' AND attempt_id='header-activation'",[original])?;
    let prepared = db.transaction(|tx| {
        prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request(), &ledger)?
            .require_current()
    })?;
    let captured = prepared.resident.capture(&request().capture)?;
    assert!(captured
        .coverage
        .result_metadata
        .values()
        .any(|result| result.activation_open));
    assert!(ledger.report()?.observed_bytes > 0);
    drop(captured);
    drop(prepared);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
