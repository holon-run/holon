use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::runtime_db::task_snapshot::{self as tasks,read_preparation::TaskReadMaterial};
use crate::runtime::resident_input_evidence::InputEvidenceScope;
use crate::types::TaskStatus;

fn at()->DateTime<Utc> { "2026-10-01T00:00:00.123456789Z".parse().unwrap() }
fn scope()->InputEvidenceScope { InputEvidenceScope {agent_id:"task-agent".into(),identity_incarnation:1} }
fn task(id:&str,status:TaskStatus)->TaskRecord {
    TaskRecord {id:id.into(),agent_id:"task-agent".into(),kind:crate::types::TaskKind::CommandTask,status,
        created_at:at(),updated_at:at(),parent_message_id:None,work_item_id:None,summary:Some(id.into()),detail:None,recovery:None}
}
fn fresh()->Result<(tempfile::TempDir,crate::runtime_db::RuntimeDb)> {
    let home=tempfile::tempdir()?;
    let db=crate::runtime_db::RuntimeDb::open_and_migrate(home.path().join("runtime.sqlite"),home.path().join("runtime.lock"))?;
    db.transaction(|tx| {
        let identity=crate::types::AgentIdentityRecord::new("task-agent",crate::types::AgentKind::Named,
            crate::types::AgentVisibility::Public,crate::types::AgentOwnership::SelfOwned,
            crate::types::AgentProfilePreset::PublicNamed,None,None);
        crate::runtime_db::evidence::upsert_agent_identity_tx(tx,&identity)?;
        crate::runtime_db::evidence::upsert_agent_state_tx(tx,&crate::types::AgentState::new("task-agent"))?;
        tasks::prepare_task_domain_snapshot_tx(tx,&scope(),&policy(4*1024*1024,4096))?.require_current()?;
        Ok(())
    })?;
    Ok((home,db))
}

fn policy(bytes:usize,records:usize)->tasks::TaskDomainLoadRequest {
    tasks::TaskDomainLoadRequest {query:Default::default(),capture:crate::runtime::resident_task::TaskCaptureRequest::current(),byte_limit:bytes,record_limit:records}
}
fn seed(tx:&Transaction<'_>,index:usize)->Result<TaskResultSettlementRecord> {
    let id=format!("actual-selection-task-{index:02}");let mut task=task(&id,TaskStatus::Completed);
    task.detail=Some(serde_json::json!({"rejoin_obligation_id":id,"rejoin_generation":1,"parent_turn_id":"actual-selection-parent"}));
    task.work_item_id=Some("actual-selection-work".into());
    let keys=std::collections::BTreeSet::from([crate::runtime::resident_task::TaskKey::new(crate::runtime::resident_task::TaskFamily::Task,&task.id)]);
    let proof=tasks::prepare_task_write_tx(tx,&scope(),&keys)?;
    crate::runtime_db::repositories::upsert_task_tx(tx,&task)?;
    let ack=tasks::register_task_writes_tx(tx,&proof,&keys,&crate::types::AgentState::new("task-agent"),&policy(64*1024*1024,65536))?;drop(ack);
    let record=TaskResultSettlementRecord {result_identity:format!("actual-selection-result-{index:02}"),agent_id:scope().agent_id,task_id:task.id,
        message_id:format!("actual-selection-message-{index:02}"),work_item_id:task.work_item_id,rejoin_generation:1,parent_turn_id:"actual-selection-parent".into(),
        task_status:"completed".into(),state:TaskResultSettlementState::PersistedPending,activation_id:None,disposition:None,
        created_at:at()+chrono::Duration::seconds(index as i64),updated_at:at(),admitted_at:None,settled_at:None,deferred_reason:None,deferred_at:None,next_recheck_at:None};
    assert!(super::super::upsert_pending_tx(tx,&record)?);Ok(record)
}

#[test]
fn original_result_selection_raw_guard_precedes_poisoned_body_and_integer_affinity_projection()->Result<()> {
    let (_home,db)=fresh()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let row=db.transaction(|tx|seed(tx,0))?;
    for task_projection in [false,true] {
        let mut connection=db.connection()?;let tx=connection.transaction()?;
        if task_projection {
            tx.execute("UPDATE tasks SET revision=?1,payload_json='!' WHERE task_id=?2",params!["x".repeat(256*1024),row.task_id])?;
        } else {
            tx.execute("UPDATE task_result_settlements SET deferred_reason=?1,payload_json='!' WHERE result_identity=?2",params!["x".repeat(256*1024),row.result_identity])?;
        }
        let actual_scope=scope();let request=policy(if task_projection {2*1024*1024} else {512*1024},65536);
        let mut material=TaskReadMaterial::new(&actual_scope,&request,Some(&ledger));let mut calls=0;
        let error=admit_original_selection_material_tx(&tx,TaskResultSelectionRead::Message {message:&row.message_id},
            &mut |bytes,nodes,copies|{calls+=1;material.charge_writer_material(bytes,nodes,copies)})
            .err().context("selected normalized material reached JSON before its actual guard")?;
        assert!(matches!(error.downcast_ref::<tasks::TaskSnapshotError>(),Some(tasks::TaskSnapshotError::ResourceExhausted(_))),"{error:#}");
        assert_eq!(calls,if task_projection {4} else {2},"raw scalar gate, not decoder or post-clone check");
        tx.rollback()?;drop(material);assert_eq!(ledger.report()?.observed_bytes,0);
        let actual=db.task_result_settlements().latest_for_message(&row.message_id)?.unwrap();assert_eq!(actual,row);
    }
    Ok(())
}

#[test]
fn original_result_complete_native_keys_reject_before_typed_decode_and_actual_guards_keep_last_holder()->Result<()> {
    let (_home,db)=fresh()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let row=db.transaction(|tx|seed(tx,0))?;let extension=serde_json::to_string(&serde_json::Value::Object(
        (0..512).map(|n|(format!("actual-result-key-{n}"),serde_json::Value::Null)).collect()))?;
    let mut connection=db.connection()?;let tx=connection.transaction()?;
    tx.execute("UPDATE task_result_settlements SET payload_json=json_set(payload_json,'$.ignored_extension',json(?1)) WHERE result_identity=?2",params![extension,row.result_identity])?;
    let (values,nodes):(usize,usize)=tx.query_row("SELECT COUNT(*),COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM task_result_settlements r,json_tree(r.payload_json) j WHERE r.result_identity=?1",[&row.result_identity],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert!(nodes>values+512);
    let actual_scope=scope();let full=policy(64*1024*1024,65536);let mut material=TaskReadMaterial::new(&actual_scope,&full,Some(&ledger));let mut charged=Vec::new();
    admit_original_selection_material_tx(&tx,TaskResultSelectionRead::Message {message:&row.message_id},
        &mut |bytes,nodes,copies|{charged.push((bytes,nodes,copies));material.charge_writer_material(bytes,nodes,copies)})?;
    assert_eq!(charged[2],(0,nodes,12));
    let selected=select_admit_message_tx(&tx,&actual_scope.agent_id,&row.message_id,"actual-selection-activation",at(),TaskResultWriteLimits::default())?
        .materialize(TaskResultWriteLimits::default())?;
    assert_eq!(selected.candidate_result_ids(),&BTreeSet::from([row.result_identity.clone()]));
    let first=charged.iter().take(2).map(|(_,nodes,copies)|nodes*copies).sum::<usize>();
    let tight=first+values*12+1;assert!(tight<first+nodes*12);
    let mut guards=material.into_guards();let last=guards.pop().context("actual selected material guard missing")?;let weak=std::sync::Arc::downgrade(&last);
    drop((selected,guards));assert!(weak.upgrade().is_some());drop(last);assert!(weak.upgrade().is_none());assert_eq!(ledger.report()?.observed_bytes,0);
    tx.execute("UPDATE task_result_settlements SET payload_json=json_set(payload_json,'$.state','invalid-result-state') WHERE result_identity=?1",[&row.result_identity])?;
    let limited=policy(64*1024*1024,tight);let mut material=TaskReadMaterial::new(&actual_scope,&limited,Some(&ledger));let mut calls=0;
    let error=admit_original_selection_material_tx(&tx,TaskResultSelectionRead::Message {message:&row.message_id},
        &mut |bytes,nodes,copies|{calls+=1;material.charge_writer_material(bytes,nodes,copies)})
        .err().context("Result object keys omitted before invalid typed decoder")?;
    assert_eq!(calls,3);assert!(matches!(error.downcast_ref::<tasks::TaskSnapshotError>(),Some(tasks::TaskSnapshotError::ResourceExhausted("records"))),"{error:#}");
    tx.rollback()?;drop(material);assert_eq!(ledger.report()?.observed_bytes,0);Ok(())
}

#[test]
fn original_result_owner_selection_keeps_exact_eight_order_and_does_not_parse_foreign_opaque_task()->Result<()> {
    let (_home,db)=fresh()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let rows=db.transaction(|tx|(0..10).map(|n|seed(tx,n)).collect::<Result<Vec<_>>>())?;
    let actual_scope=scope();let full=policy(64*1024*1024,65536);
    db.transaction(|tx| {
        tx.execute("UPDATE tasks SET owner_agent_id='foreign-original-target',payload_json='!' WHERE task_id=?1",[&rows[0].task_id])?;
        let mut material=TaskReadMaterial::new(&actual_scope,&full,Some(&ledger));
        admit_original_selection_material_tx(tx,TaskResultSelectionRead::Owner {agent:&actual_scope.agent_id,work:Some("actual-selection-work")},
            &mut |bytes,nodes,copies|material.charge_writer_material(bytes,nodes,copies))?;
        let actual=select_admit_unsettled_tx(tx,&actual_scope.agent_id,Some("actual-selection-work"),"actual-owner-activation",at(),TaskResultWriteLimits::default())?
            .materialize(TaskResultWriteLimits::default())?;
        assert_eq!(actual.candidate_result_ids(),&rows.iter().take(8).map(|row|row.result_identity.clone()).collect());
        assert_eq!(actual.dependency_task_ids(),&rows.iter().take(8).map(|row|row.task_id.clone()).collect());
        assert_eq!(tx.query_row("SELECT payload_json FROM tasks WHERE task_id=?1",[&rows[0].task_id],|r|r.get::<_,String>(0))?,"!");
        assert!(material.material_estimate().records>0);
        drop((actual,material));assert_eq!(ledger.report()?.observed_bytes,0);Ok(())
    })
}
