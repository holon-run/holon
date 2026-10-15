use super::*;
use crate::runtime_db::task_snapshot::tests::{fresh,scope,task};
#[test]
fn original_written_projection_measures_control_and_nul_columns_before_json_and_retires_its_sizing_workspace() -> Result<()> {
    let (_dir,db)=fresh()?;
    let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let scope=scope();
        let mut task=task("actual-written-projection",crate::types::TaskStatus::Running);
        let text="x\0\\\n".repeat(24*1024);
        task.detail=Some(serde_json::json!({"output_path":text}));
        repositories::upsert_task_tx(tx,&task)?;
        let key=TaskKey::new(TaskFamily::Task,&task.id);
        let request=TaskDomainLoadRequest {query:Default::default(),capture:TaskCaptureRequest::current(),byte_limit:64*1024*1024,record_limit:65536};
        let mut phase=TaskReadMaterial::new(&scope,&request,Some(&ledger));
        phase.charge_writer_material(1024,4,1)?;
        let before=phase.material_estimate();
        let mut actual=false;
        phase.with_original_written_row(tx,&scope.agent_id,&key,|row| {
            let row=row.context("actual written Task missing")?;
            let CanonicalTaskFact::Task(record)=row.fact else {anyhow::bail!("actual Task family changed")};
            assert_eq!(record.id,task.id);
            assert_eq!(repositories::task_detail_string(&record.detail,"output_path"),
                Some(repositories::truncate_task_payload_string(&text)));
            actual=true;Ok((true,false))
        })?;
        assert!(actual);
        let after=phase.material_estimate();
        assert_eq!((after.bytes,after.records),(before.bytes,before.records));
        let peak=phase.phase_peak_for_test();
        assert!(peak.bytes>before.bytes && peak.records>before.records);
        drop(phase);assert_eq!(ledger.report()?.observed_bytes,0);
        for (bytes,nodes) in [(peak.bytes-1,request.record_limit),(request.byte_limit,peak.records-1)] {
            let mut tight=request.clone();tight.byte_limit=bytes;tight.record_limit=nodes;
            let mut phase=TaskReadMaterial::new(&scope,&tight,Some(&ledger));
            phase.charge_writer_material(1024,4,1)?;
            let mut decoded=false;
            let error=phase.with_original_written_row(tx,&scope.agent_id,&key,|_|{decoded=true;Ok((true,false))}).unwrap_err();
            assert!(error.downcast_ref::<TaskSnapshotError>().is_some_and(|e|matches!(e,TaskSnapshotError::ResourceExhausted(_))),"{error:#}");
            assert!(!decoded);drop(phase);assert_eq!(ledger.report()?.observed_bytes,0);
        }
        tx.execute("UPDATE tasks SET payload_json=?2 WHERE task_id=?1",params![key.id,"!".repeat(96*1024)])?;
        let mut tight=request;tight.byte_limit=64*1024;
        let mut phase=TaskReadMaterial::new(&scope,&tight,Some(&ledger));
        let mut decoded=false;
        let error=phase.with_original_written_row(tx,&scope.agent_id,&key,|_|{decoded=true;Ok((true,false))}).unwrap_err();
        assert!(error.downcast_ref::<TaskSnapshotError>().is_some_and(|e|matches!(e,TaskSnapshotError::ResourceExhausted(_))),"{error:#}");
        assert!(!decoded);drop(phase);assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })
}

#[test]
fn original_written_projection_reserves_old_and_new_sqlite_capacity_while_all_three_nul_columns_remain_live() -> Result<()> {
    let (_dir,db)=fresh()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let scope=scope();let task=task("actual-realloc-projection",crate::types::TaskStatus::Running);
        repositories::upsert_task_tx(tx,&task)?;
        let raw="\0".repeat(96*1024);
        tx.execute("UPDATE tasks SET summary=?2,output_path=?2,result_summary=?2 WHERE task_id=?1",params![task.id,raw])?;
        let key=TaskKey::new(TaskFamily::Task,&task.id);
        let mut request=TaskDomainLoadRequest {query:Default::default(),capture:TaskCaptureRequest::current(),byte_limit:4*1024*1024,record_limit:65536};
        let mut material=TaskReadMaterial::new(&scope,&request,Some(&ledger));
        let mut decoded=false;
        let error=material.with_original_written_row(tx,&scope.agent_id,&key,|_|{decoded=true;Ok((true,false))}).unwrap_err();
        assert!(error.downcast_ref::<TaskSnapshotError>().is_some_and(|e|matches!(e,TaskSnapshotError::ResourceExhausted(_))),"{error:#}");
        assert!(!decoded);drop(material);assert_eq!(ledger.report()?.observed_bytes,0);
        request.byte_limit=16*1024*1024;
        let mut material=TaskReadMaterial::new(&scope,&request,Some(&ledger));
        let error=material.with_original_written_row(tx,&scope.agent_id,&key,|_|{decoded=true;Ok((true,false))}).unwrap_err();
        assert!(format!("{error:#}").contains("normalized/payload projection mismatch"),"{error:#}");
        assert!(!decoded);drop(material);assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })
}
#[test]
fn original_written_projection_counts_expected_canonical_fields_even_when_normalized_text_is_short_and_drifted() -> Result<()> {
    let (_dir,db)=fresh()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    db.transaction(|tx| {
        let scope=scope();let mut task=task("actual-canonical-expected",crate::types::TaskStatus::Running);
        repositories::upsert_task_tx(tx,&task)?;
        // A same-TX trigger/raw drift can leave a large readable canonical field
        // with a short normalized field. The original comparator must still run
        // when enough budget exists, but expected clones must be admitted first.
        task.summary=Some("x".repeat(96*1024));
        let payload=serde_json::to_string(&task)?;
        tx.execute("UPDATE tasks SET payload_json=?2,summary='' WHERE task_id=?1",params![task.id,payload])?;
        let key=TaskKey::new(TaskFamily::Task,&task.id);
        let request=TaskDomainLoadRequest {query:Default::default(),capture:TaskCaptureRequest::current(),byte_limit:16*1024*1024,record_limit:65536};
        let columns={
            let mut sizing=TaskReadMaterial::new(&scope,&request,Some(&ledger));
            let values=sizing.original_written_columns_tx(tx,&scope.agent_id,&key)?.context("actual row missing")?;
            drop(sizing);values
        };
        assert!(columns.0>96*1024 && columns.1<4096 && columns.2<4096);
        let actual={
            let mut decoder=TaskReadMaterial::new(&scope,&request,Some(&ledger));
            decoder.admit_original_written_decode_tx(tx,&key,columns)?;
            let actual=decoder.material_estimate();drop(decoder);actual
        };
        assert_eq!(ledger.report()?.observed_bytes,0);
        let mut tight=request.clone();tight.byte_limit=actual.bytes-columns.0;
        let mut decoder=TaskReadMaterial::new(&scope,&tight,Some(&ledger));let mut decoded=false;
        let error=decoder.with_original_written_row(tx,&scope.agent_id,&key,|_|{decoded=true;Ok((true,false))}).unwrap_err();
        assert!(error.downcast_ref::<TaskSnapshotError>().is_some_and(|e|matches!(e,TaskSnapshotError::ResourceExhausted(_))),"{error:#}");
        assert!(!decoded);drop(decoder);assert_eq!(ledger.report()?.observed_bytes,0);
        let mut decoder=TaskReadMaterial::new(&scope,&request,Some(&ledger));
        let error=decoder.with_original_written_row(tx,&scope.agent_id,&key,|_|{decoded=true;Ok((true,false))}).unwrap_err();
        assert!(format!("{error:#}").contains("normalized/payload projection mismatch"),"{error:#}");
        assert!(!decoded);drop(decoder);assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })
}
