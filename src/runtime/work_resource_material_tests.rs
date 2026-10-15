use super::*;
use crate::runtime::tests::support::{context_config, tempdir};
use crate::runtime_db::{
    scheduling_snapshot::SchedulingDomainLoadRequest,
    task_snapshot::TaskDomainLoadRequest,
    transitions::{PreparedInputTransition, WorkItemMutation, WorkItemTransitionCommand},
    work_snapshot::{WorkDomainLoadRequest, WorkSnapshotError},
};
use crate::types::{WorkItemRecord, WorkItemState};

fn runtime(home: &Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new_isolated_resource_fixture("work-resource-sql", home.to_path_buf(), context_config())
}
fn prepared(runtime: &RuntimeHandle, record: &WorkItemRecord) -> Result<PreparedInputTransition> {
    PreparedInputTransition::work(
        resident_input_evidence::InputEvidenceScope { agent_id: record.agent_id.clone(),
            identity_incarnation: runtime.inner.identity_incarnation },
        "original-resource-sql".into(), record.updated_at,
        WorkItemTransitionCommand {
            agent_id: record.agent_id.clone(), mutation: WorkItemMutation::Insert { record:record.clone() },
            agent_state:None, brief_evidence:Vec::new(), audit_events:Vec::new(), index_changes:Vec::new(),
            notify_scheduler:true, fault:None,
        }, Default::default(), false,
    )
}
fn execute(runtime: &RuntimeHandle, command: &PreparedInputTransition, bytes:usize)
    -> Result<crate::runtime_db::transitions::InputMutationExecution<crate::runtime_db::transitions::TransitionCommit>>
{
    let scope = resident_input_evidence::InputEvidenceScope { agent_id:runtime.instance_agent_id().to_owned(),
        identity_incarnation:runtime.inner.identity_incarnation };
    runtime.inner.runtime_db.transitions_in_scope(&scope).commit_prepared_transition_with_domain_requests(
        command, &WorkDomainLoadRequest {
            capture:resident_work::WorkCaptureRequest::current(), byte_limit:bytes, record_limit:65_536,
        }, &runtime.inner.input_evidence_budget, &TaskDomainLoadRequest {
            capture:resident_task::TaskCaptureRequest::current(), query:Default::default(), byte_limit:64*1024*1024,
            record_limit:65_536,
        }, &SchedulingDomainLoadRequest { capture:resident_scheduling::SchedulingCaptureRequest::current(),
            byte_limit:64*1024*1024, record_limit:65_536 })
}

#[tokio::test]
async fn actual_file_material_and_work_sql_share_cap_without_a_second_ledger_view() -> Result<()> {
    let home=tempdir()?;
    let runtime=runtime(home.path())?;
    runtime.stop_context_resource_observer().await?;
    let mut record=WorkItemRecord::new(runtime.instance_agent_id(),"actual original file SQL allowance",WorkItemState::Open);
    let (artifact,file,cost)=runtime.create_work_plan_file_with_material(&record,Some(&"original plan ".repeat(16*1024)))?;
    record.plan_artifact=Some(artifact);
    let loan=WorkResourceMaterialLoan::created_file(file.clone(),cost);
    let scope=resident_input_evidence::InputEvidenceScope {agent_id:record.agent_id.clone(),identity_incarnation:runtime.inner.identity_incarnation};
    let baseline=runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let actual=loan.validate_accounting(&scope,&runtime.inner.input_evidence_budget)?;
    assert_eq!(actual,(usize::try_from(cost.0)?,cost.1));
    assert_eq!(runtime.inner.input_evidence_budget.report()?.observed_bytes,baseline,"loan shares original guard");
    let limit=actual.0.checked_add(8*1024).context("fixture limit overflow")?;
    assert!(limit<64*1024*1024);
    runtime.inner.runtime_db.connection()?.execute_batch(
        "CREATE TRIGGER rollback_original_resource_sql BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='original-resource-sql' BEGIN SELECT RAISE(ABORT,'original resource SQL probe'); END;")?;
    let command=prepared(&runtime,&record)?;
    let error=execute(&runtime,&command,limit).err().context("original last receipt fault must fire")?;
    assert!(format!("{error:#}").contains("original resource SQL probe"),"{error:#}");
    assert_eq!(runtime.inner.input_evidence_budget.report()?.observed_bytes,baseline);
    let command=command.with_work_resource_material(loan)?;
    let error=execute(&runtime,&command,limit).err().context("individually fitting file and SQL must reject together")?;
    assert!(matches!(error.downcast_ref::<WorkSnapshotError>(),Some(WorkSnapshotError::ResourceExhausted(_))),"{error:#}");
    assert!(runtime.inner.runtime_db.work_items().latest(&record.id)?.is_none());
    let aliases:usize=runtime.inner.runtime_db.connection()?.query_row(
        "SELECT COUNT(*) FROM work_domain_records WHERE family='work' AND record_id=?1",[&record.id],|r|r.get(0))?;
    assert_eq!(aliases,0);
    assert_eq!(runtime.inner.input_evidence_budget.report()?.observed_bytes,baseline);
    runtime.inner.runtime_db.connection()?.execute_batch("DROP TRIGGER rollback_original_resource_sql")?;
    let ack=execute(&runtime,&command,64*1024*1024)?;
    assert!(matches!(&ack,crate::runtime_db::transitions::InputMutationExecution::Committed {..}));
    assert_eq!(runtime.inner.runtime_db.work_items().latest(&record.id)?,Some(record));
    let weak=Arc::downgrade(&file);
    drop(file);
    assert!(weak.upgrade().is_some(),"actual Prepared command retains original file responsibility");
    drop((command,ack));
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn original_work_resource_loan_rejects_foreign_scope_and_ledger() -> Result<()> {
    let home=tempdir()?;
    let runtime=runtime(home.path())?;
    runtime.stop_context_resource_observer().await?;
    let record=WorkItemRecord::new(runtime.instance_agent_id(),"original file binding",WorkItemState::Open);
    let (_artifact,file,cost)=runtime.create_work_plan_file_with_material(&record,Some("actual file"))?;
    let weak=Arc::downgrade(&file);
    let loan=WorkResourceMaterialLoan::created_file(file,cost);
    let scope=resident_input_evidence::InputEvidenceScope {agent_id:record.agent_id,identity_incarnation:runtime.inner.identity_incarnation};
    let before=runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let other=input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(loan.validate_accounting(&scope,&other).is_err());
    let foreign=resident_input_evidence::InputEvidenceScope {identity_incarnation:scope.identity_incarnation+1,..scope.clone()};
    assert!(loan.validate_accounting(&foreign,&runtime.inner.input_evidence_budget).is_err());
    assert_eq!(runtime.inner.input_evidence_budget.report()?.observed_bytes,before);
    assert!(weak.upgrade().is_some());
    drop(loan);
    assert!(weak.upgrade().is_none());
    assert_eq!(other.report()?.observed_bytes,0);
    Ok(())
}
