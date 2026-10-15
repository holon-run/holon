use super::*;
use crate::runtime_db::task_snapshot::read_preparation;

#[test]
fn task_material_gates_integer_affinity_projection_before_malformed_payload() -> Result<()> {
    let (_dir, db) = fresh()?;
    let key = TaskKey::new(TaskFamily::Task, "raw-normalized-revision");
    db.transaction(|tx| {
        write_task(tx, &task(&key.id, TaskStatus::Running))?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let before = db.transaction(|tx| Ok(current_coverage(tx, &scope())?))?;
    let mut tight = request();
    tight.byte_limit = 128 * 1024;
    let error = db
        .transaction(|tx| {
            tx.execute(
                "UPDATE tasks SET revision=?1,payload_json='!' WHERE task_id=?2",
                params!["x".repeat(256 * 1024), key.id],
            )?;
            assert_eq!(
                tx.query_row(
                    "SELECT typeof(revision) FROM tasks WHERE task_id=?1",
                    [&key.id],
                    |row| row.get::<_, String>(0)
                )?,
                "text"
            );
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &tight, &ledger)?;
            Ok(())
        })
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted("bytes"))
        ),
        "{error:#}"
    );
    assert_eq!(
        db.transaction(|tx| Ok(current_coverage(tx, &scope())?))?,
        before
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.transaction(|tx| {
        let scope = scope();
        let mut phase = read_preparation::TaskReadMaterial::new(&scope, &tight, Some(&ledger));
        let exact = phase
            .admit_stored_tx(tx, &scope.agent_id, &key)?
            .context("original Task missing")?;
        assert_eq!(Some(exact), checkpoint_material_size_on(tx, &key)?);
        assert!(stored_on(tx, &scope.agent_id, &key)?.is_some());
        assert!(phase.material_estimate().records > 8);
        let guards = phase.into_guards();
        assert!(ledger.report()?.observed_bytes > 0);
        let retained = guards.clone();
        drop(guards);
        assert!(ledger.report()?.observed_bytes > 0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })
}

#[test]
fn task_body_material_counts_all_object_keys_and_keeps_foreign_body_unread() -> Result<()> {
    let (_dir, db) = fresh()?;
    let key = TaskKey::new(TaskFamily::Task, "native-key-task");
    let mut value = task(&key.id, TaskStatus::Running);
    value.detail = Some(serde_json::Value::Object(
        (0..512)
            .map(|index| (format!("key-{index}"), serde_json::Value::Null))
            .collect(),
    ));
    db.transaction(|tx| {
        write_task(tx, &value)?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let (values,complete):(usize,usize)=tx.query_row("SELECT COUNT(*),COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM tasks t,json_tree(t.payload_json) j WHERE task_id=?1",[&key.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert!(complete>values+512);
        let mut tight=request();tight.byte_limit=64*1024*1024;
        tight.record_limit=(values+22+1)*8;
        let scope=scope();
        let mut phase=read_preparation::TaskReadMaterial::new(&scope,&tight,Some(&ledger));
        let error=phase.admit_stored_tx(tx,&scope.agent_id,&key).unwrap_err();
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ResourceExhausted("records"))),"{error:#}");
        drop(phase);assert_eq!(ledger.report()?.observed_bytes,0);
        let mut enough=request();enough.byte_limit=64*1024*1024;enough.record_limit=65_536;
        let mut phase=read_preparation::TaskReadMaterial::new(&scope,&enough,Some(&ledger));
        phase.admit_stored_tx(tx,&scope.agent_id,&key)?.context("original Task")?;
        assert_eq!(stored_on(tx,&scope.agent_id,&key)?.context("decoded Task")?.fact,CanonicalTaskFact::Task(repositories::slim_task_record_for_payload(&value)));
        assert_eq!(phase.material_estimate().records,(complete+22+1)*8);
        drop(phase);assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            tx.execute(
                "UPDATE tasks SET owner_agent_id='foreign',payload_json=?1 WHERE task_id=?2",
                params!["!".repeat(9 * 1024 * 1024), key.id],
            )?;
            let scope = scope();
            let policy = request();
            let mut phase = read_preparation::TaskReadMaterial::new(&scope, &policy, Some(&ledger));
            phase.admit_stored_tx(tx, &scope.agent_id, &key)?;
            Ok(())
        })
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ForeignRecord(actual)) if actual==&key),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn result_material_gates_normalized_generation_and_current_metadata_native_keys() -> Result<()> {
    let (_dir, db) = fresh()?;
    let result = TaskResultSettlementRecord {
        result_identity: "raw-result-generation".into(),
        agent_id: scope().agent_id,
        task_id: "original-result-task".into(),
        message_id: "original-result-message".into(),
        work_item_id: None,
        rejoin_generation: 1,
        parent_turn_id: "original-parent".into(),
        task_status: "completed".into(),
        state: TaskResultSettlementState::PersistedPending,
        activation_id: None,
        disposition: None,
        created_at: at(),
        updated_at: at(),
        admitted_at: None,
        settled_at: None,
        deferred_reason: None,
        deferred_at: None,
        next_recheck_at: None,
    };
    db.transaction(|tx| {
        upsert_pending_tx(tx, &result)?;
        Ok(())
    })?;
    let key = TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let mut tight = request();
    tight.byte_limit = 128 * 1024;
    let error=db.transaction(|tx| {
        tx.execute("UPDATE task_result_settlements SET rejoin_generation=?1,payload_json='!' WHERE result_identity=?2",params!["x".repeat(256*1024),key.id])?;
        let scope=scope();let mut phase=read_preparation::TaskReadMaterial::new(&scope,&tight,Some(&ledger));
        phase.admit_stored_tx(tx,&scope.agent_id,&key)?;Ok(())
    }).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted("bytes"))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.transaction(|tx| {
        let scope = scope();
        let policy = request();
        let mut phase = read_preparation::TaskReadMaterial::new(&scope, &policy, Some(&ledger));
        let exact = phase
            .admit_stored_tx(tx, &scope.agent_id, &key)?
            .context("original Result")?;
        assert_eq!(Some(exact), checkpoint_material_size_on(tx, &key)?);
        assert_eq!(
            stored_on(tx, &scope.agent_id, &key)?
                .context("decoded Result")?
                .fact,
            CanonicalTaskFact::ResultSettlement(result.clone())
        );
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })?;
    db.transaction(|tx| {
        let extra=serde_json::Value::Object((0..512).map(|index|(format!("metadata-{index}"),serde_json::Value::Null)).collect());
        tx.execute("UPDATE task_domain_current SET coverage_json=json_set(coverage_json,'$.ignored_extension',json(?1)) WHERE agent_id='task-agent'",[serde_json::to_string(&extra)?])?;
        let (values,nodes):(usize,usize)=tx.query_row("SELECT COUNT(*),COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM task_domain_current c,json_tree(c.coverage_json) j WHERE agent_id='task-agent'",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert!(nodes>values+512);
        let mut policy=request();policy.byte_limit=64*1024*1024;policy.record_limit=(values+1)*4;
        let scope=scope();let mut phase=read_preparation::TaskReadMaterial::new(&scope,&policy,Some(&ledger));
        let error=phase.read_current_metadata(tx).unwrap_err();
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ResourceExhausted("records"))),"{error:#}");
        drop(phase);assert_eq!(ledger.report()?.observed_bytes,0);
        policy.record_limit=65_536;
        let mut phase=read_preparation::TaskReadMaterial::new(&scope,&policy,Some(&ledger));
        assert!(phase.read_current_metadata(tx)?.is_some());
        assert_eq!(phase.material_estimate().records,(nodes+1)*4);
        drop(phase);assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })
}
