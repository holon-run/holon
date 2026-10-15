use super::*;
use std::sync::Arc;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::runtime::resident_task::{TaskCaptureRequest,TaskKey,TaskFamily};
use crate::runtime_db::input_evidence_snapshot::{read_material::InputReadMaterial,InputEvidenceLoadRequest};
use crate::runtime_db::task_snapshot as tasks;
use crate::types::{TaskRecord,TaskKind,TaskStatus};

#[test]
fn actual_started_memory_and_task_source_metadata_share_input_guards_without_a_second_debit()->Result<()> {
    let f=Fixture::new(true)?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let task_request=tasks::TaskDomainLoadRequest {query:Default::default(),capture:TaskCaptureRequest::current(),byte_limit:4*1024*1024,record_limit:65536};
    let input_request=InputEvidenceLoadRequest {windows:Vec::new(),mandatory_roots:Default::default(),interrupted_sources:Default::default(),
        selected_completion_brief_ids:Default::default(),byte_limit:64*1024*1024,record_limit:65536};
    let baseline=ledger.report()?.observed_bytes;
    f.db.transaction(|tx| {
        let state=crate::runtime_db::context_inputs::ContextDurableRead::new(tx,&f.scope.agent_id).scope_facts()?.state.context("actual original State absent")?;
        let task=TaskRecord {id:"actual-memory-source-task".into(),agent_id:f.scope.agent_id.clone(),kind:TaskKind::CommandTask,status:TaskStatus::Running,
            created_at:f.at,updated_at:f.at,parent_message_id:None,work_item_id:None,summary:None,detail:None,recovery:None};
        let keys=BTreeSet::from([TaskKey::new(TaskFamily::Task,&task.id)]);
        let proof=tasks::prepare_task_write_tx(tx,&f.scope,&keys)?;
        crate::runtime_db::repositories::upsert_task_tx(tx,&task)?;
        let actual=tasks::register_task_writes_tx(tx,&proof,&keys,&state,&task_request)?;drop(actual);
        let prepared=tasks::prepare_task_output_snapshot_with_budget_tx(tx,&f.scope,&task_request,&ledger)?.require_current()?;drop(prepared);
        let mut message=MessageEnvelope::new(&f.scope.agent_id,MessageKind::TaskResult,MessageOrigin::Task {task_id:task.id.clone()},
            AuthorityClass::RuntimeInstruction,Priority::Normal,MessageBody::Text {text:"original M2 output source".into()});
        message.id="actual-memory-output-source".into();message.created_at=f.at;message.metadata=Some(serde_json::json!({"task_id":task.id}));
        let (message,_)=append_message_tx(tx,&message)?;
        let input=register_committed_input_evidence_tx(tx,&f.scope,&[CanonicalInputEvidence::Message(message)])?;
        let intent=DispatchMemoryIntent::Started {start:Box::new(f.start.clone())};
        let memory_request=WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit:64*1024*1024,record_limit:65536};
        let mut memory=DispatchMemoryTransactionBudget::prepare_tx(tx,&intent,&memory_request,&ledger)?;
        let before=memory.material_usage_for_test();
        let mut material=InputReadMaterial::new_with_memory(&f.scope,&input_request,&ledger,(0,0),Some(&mut memory))?;
        let held_bytes=128*1024;material.observe_projected(u64::try_from(held_bytes)?,1)?;let held=vec![0u8;held_bytes];
        let delta=material.with_task_material_limits(&f.scope,&ledger,Some((task_request.byte_limit,task_request.record_limit)),|task|
            tasks::register_task_source_metadata_using_material_tx(tx,&f.scope,&input,&ledger,task))?.context("actual M2 Task metadata absent")?;
        let actual=material.usage();let mut guards=material.into_guards();
        assert_eq!(memory.material_usage_for_test(),(before.0+actual.0,before.1+actual.1),"same Input/Task allocations debit Memory once");
        assert!(delta.records.is_empty() && delta.stop_records.is_empty());
        let last=guards.pop().context("actual supplemental guard absent")?;let weak=Arc::downgrade(&last);
        drop((delta,guards,held,memory));assert!(weak.upgrade().is_some());assert!(ledger.report()?.observed_bytes>baseline);
        drop(last);assert!(weak.upgrade().is_none());assert_eq!(ledger.report()?.observed_bytes,baseline);
        Ok(())
    })
}
