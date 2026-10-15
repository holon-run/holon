//! Real original Started/Deferred Memory with actual file material and one Work TX.
use super::*;
use crate::runtime::{WorkResourceMaterialLoan, resident_work::WorkCaptureRequest};
use crate::runtime_db::{
    work_snapshot::{WorkDomainLoadRequest, WorkSnapshotError},
    task_snapshot::TaskDomainLoadRequest,
    scheduling_snapshot::SchedulingDomainLoadRequest,
    transitions::InputMutationExecution,
};

fn execute(
    f:&Fixture, prepared:&PreparedInputTransition, byte_limit:usize,
) -> Result<InputMutationExecution<TransitionCommit>> {
    f.runtime.runtime_db().transitions_in_scope(&f.scope())
        .commit_prepared_transition_with_domain_requests(
            prepared, &WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit,record_limit:65_536},
            &f.runtime.inner.input_evidence_budget,
            &TaskDomainLoadRequest {capture:crate::runtime::resident_task::TaskCaptureRequest::current(),
                query:Default::default(),byte_limit:64*1024*1024,record_limit:65_536},
            &SchedulingDomainLoadRequest {capture:crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
                byte_limit:64*1024*1024,record_limit:65_536})
}

fn sidecars(f:&Fixture) -> Result<Vec<String>> {
    f.runtime.runtime_db().transaction(|tx| {
        [
            "SELECT COALESCE(json_group_array(json_array(agent_id,identity_incarnation,revision,coverage_json)),'[]') FROM work_domain_current",
            "SELECT COALESCE(json_group_array(json_array(agent_id,identity_incarnation,family,record_id,availability,content_digest,fingerprint_json)),'[]') FROM work_domain_records",
            "SELECT COALESCE(json_group_array(json_array(agent_id,identity_incarnation,revision,coverage_json)),'[]') FROM current_read_current",
            "SELECT COALESCE(json_group_array(json_array(agent_id,identity_incarnation,family,record_id,content_digest,projection_digest,revision)),'[]') FROM current_read_members",
        ].into_iter().map(|sql| Ok(tx.query_row(sql,[],|r|r.get(0))?)).collect()
    })
}

#[tokio::test]
async fn original_started_memory_and_actual_file_loan_share_work_phase_and_whole_ack() -> Result<()> {
    Box::pin(async {
        let mut f=Fixture::new()?;
        f.work().await?;
        f.start().await?;
        let original_memory=f.memory()?;
        assert!(matches!(original_memory,DispatchMemoryRecord::Started {..}));
        let mut record=WorkItemRecord::new("default","actual file plus original Memory",crate::types::WorkItemState::Open);
        let (artifact,file,cost)=f.runtime.create_work_plan_file_with_material(
            &record,Some(&"actual file kernel ".repeat(16*1024)))?;
        record.plan_artifact=Some(artifact);
        let command=WorkItemTransitionCommand {
            agent_id:"default".into(),mutation:WorkItemMutation::Insert {record:record.clone()},
            agent_state:None,brief_evidence:Vec::new(),audit_events:Vec::new(),index_changes:Vec::new(),
            notify_scheduler:true,fault:None,
        };
        let plain=f.plain_work("original-memory-resource-loan",f.runtime.now(),&command)?;
        let mut owner=f.runtime.begin_mutation().await?;
        // The real owner locates the actual accepted Start and freezes Deferred.
        let prepared=owner.attach_actual_dispatch_memory(plain)?;
        let before=f.durable_image()?;
        let original_sidecars=sidecars(&f)?;
        let baseline=f.runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let limit=usize::try_from(cost.0)?.checked_add(8*1024).context("fixture combined limit overflow")?;
        assert!(limit<64*1024*1024);
        f.runtime.runtime_db().connection()?.execute_batch(
            "CREATE TRIGGER reject_original_memory_loan_receipt BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='original-memory-resource-loan' BEGIN SELECT RAISE(ABORT,'original Memory Resource last receipt'); END;")?;
        let error=execute(&f,&prepared,limit).err().context("no-loan M2 must reach real last receipt fault")?;
        assert!(format!("{error:#}").contains("original Memory Resource last receipt"),"{error:#}");
        assert_eq!(f.durable_image()?,before);
        assert_eq!(sidecars(&f)?,original_sidecars);
        assert_eq!(f.memory()?,original_memory);
        assert_eq!(f.runtime.inner.input_evidence_budget.report()?.observed_bytes,baseline);
        let prepared=prepared.with_work_resource_material(WorkResourceMaterialLoan::created_file(file.clone(),cost))?;
        let descriptor=prepared.descriptor().clone();
        let error=execute(&f,&prepared,limit).err().context("M2 plus original Resource must share remaining")?;
        assert!(matches!(error.downcast_ref::<DispatchMemoryError>(),Some(DispatchMemoryError::ResourceExhausted))
            || matches!(error.downcast_ref::<WorkSnapshotError>(),Some(WorkSnapshotError::ResourceExhausted(_))),"{error:#}");
        assert_eq!(f.durable_image()?,before);
        assert_eq!(sidecars(&f)?,original_sidecars);
        assert_eq!(f.memory()?,original_memory);
        assert_eq!(f.runtime.inner.input_evidence_budget.report()?.observed_bytes,baseline);
        f.runtime.runtime_db().connection()?.execute_batch("DROP TRIGGER reject_original_memory_loan_receipt")?;
        // Same original frozen command, actual owner and ledger; no replacement
        // nonce, Start, frozen clock, alias or invented Memory allowance.
        let commit=owner.commit_prepared_input_transition(prepared).await?;
        assert!(commit.applied);
        let pending=f.memory()?;
        assert!(matches!(pending,DispatchMemoryRecord::Pending {..}));
        let checkpoint=serde_json::to_value(&pending)?;
        let members=checkpoint["obligation"]["members"].as_object().context("actual Deferred members absent")?;
        assert_eq!(members.len(),1);
        let actual_descriptor:crate::runtime_db::input_mutation_receipts::PreparedInputMutation=
            serde_json::from_value(members[descriptor.command_nonce()]["original"].clone())?;
        assert_eq!(actual_descriptor,descriptor);
        assert_eq!(f.runtime.runtime_db().work_items().latest(&record.id)?,Some(record.clone()));
        let root=owner.capture_published_root()?;
        let selected=root.work.as_ref().context("actual ACK omitted Work domain")?
            .qualified_context_plan_source(&record.id).context("actual ACK omitted admitted new Work")?;
        assert!(matches!(&selected.record,crate::runtime::resident_work::CanonicalWorkRecord::Work(actual) if actual==&record));
        drop(root);
        let weak=Arc::downgrade(&file);
        drop(file);
        assert!(weak.upgrade().is_none(),"Prepared loan released after original SQL/publication");
        drop(owner);
        f.runtime.apply_transition_commit(commit).await;
        f.finish("original-memory-resource-loan-finish").await?;
        Ok(())
    }).await
}
