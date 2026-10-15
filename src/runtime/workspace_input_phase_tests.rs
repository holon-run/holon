use super::*;
use crate::runtime_db::{
    input_evidence_snapshot::InputEvidenceSnapshotError, task_snapshot::TaskDomainLoadRequest,
    transitions::{AgentPostureTransitionCommand,AgentStateMutation,InputMutationExecution,PreparedInputTransition,TransitionCommit},
    work_snapshot::WorkDomainLoadRequest,
};
async fn fixture() -> Result<(tempfile::TempDir,RuntimeHandle)> {
    let home=tests::support::tempdir()?;
    let runtime=RuntimeHandle::new_isolated_resource_fixture("workspace-first-input-owner",home.path().into(),tests::support::context_config())?;
    runtime.refresh_context_resources().await?;
    Ok((home,runtime))
}
fn scope(runtime:&RuntimeHandle)->resident_input_evidence::InputEvidenceScope {
    resident_input_evidence::InputEvidenceScope {agent_id:runtime.instance_agent_id().into(),identity_incarnation:runtime.inner.identity_incarnation}
}
fn posture(runtime:&RuntimeHandle,root:&Arc<resident_state::AgentSnapshot>,nonce:&str)->Result<PreparedInputTransition> {
    let mut state=root.state.as_ref().clone();state.last_wake_reason=Some("actual Workspace first phase".into());
    PreparedInputTransition::posture(scope(runtime),nonce.into(),Utc::now(),AgentPostureTransitionCommand {
        agent_id:runtime.instance_agent_id().into(),expected_incarnation:Some(runtime.inner.identity_incarnation),
        agent_state:AgentStateMutation {expected:Some(Box::new(root.state.as_ref().clone())),record:Box::new(state)},audit_events:Vec::new(),fault:None,
    },None)?.with_current_read_source(root.current_reads.as_ref().context("actual Current source absent")?.clone())
}
fn commit(runtime:&RuntimeHandle,p:&PreparedInputTransition,source:&Arc<mutation_owner::OriginalInputQuerySource>,
    ledger:&input_evidence_budget::InputEvidenceBudgetLedger,bytes:usize)->Result<InputMutationExecution<TransitionCommit>> {
    let work=WorkDomainLoadRequest {capture:resident_work::WorkCaptureRequest::current(),byte_limit:bytes,record_limit:65536};
    let tasks=TaskDomainLoadRequest {capture:resident_task::TaskCaptureRequest::current(),query:Default::default(),byte_limit:bytes,record_limit:65536};
    runtime.runtime_db().transitions_in_scope(&scope(runtime)).commit_prepared_transition_with_input_query_source(p,&work,ledger,&tasks,source)
}
fn counts(runtime:&RuntimeHandle)->Result<(usize,usize,usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM input_mutation_receipts),(SELECT COUNT(*) FROM input_query_birth_members),(SELECT COUNT(*) FROM current_read_members)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
}
#[tokio::test]
async fn workspace_first_input_phase_keeps_actual_host_loan_through_state_ack_and_last_effect_clone()->Result<()> {
    let (_home,runtime)=fixture().await?;let _pause=runtime.pause_resource_observer_for_test().await;
    let reservation=runtime.inner.mutation_owner.reserve().await?;
    let root=runtime.inner.mutation_owner.capture()?.context("actual published root missing")?;
    let ledger=&runtime.inner.input_evidence_budget;
    let source=reservation.input_query_source(&scope(&runtime),ledger,(64*1024*1024,65536))?;
    let baseline=ledger.report()?.observed_bytes;
    let loan=runtime.prepare_original_workspace_request_material(&"original owned Workspace descriptor")?;
    let mut weak=None;loan.visit_actual_guards(|guard|{
        assert!(guard.validate_owned_material_binding(&scope(&runtime),ledger,0).is_err(),"Host guard must not become agent-scoped Input material");
        weak=Some(Arc::downgrade(guard));Ok(())
    })?;let weak=weak.context("actual Workspace guard missing")?;
    let p=posture(&runtime,&root,"workspace-first-actual-ack")?.with_original_workspace_request_material(loan.clone(),reservation.owner_epoch(),ledger)?;
    let before=counts(&runtime)?;
    let InputMutationExecution::Committed {value,receipt}=commit(&runtime,&p,&source,ledger,64*1024*1024)? else {anyhow::bail!("actual Workspace State must commit")};
    assert!(value.applied&&receipt.outcome.applied);
    assert_eq!(runtime.runtime_db().agent_states().latest(runtime.instance_agent_id())?.unwrap().last_wake_reason.as_deref(),Some("actual Workspace first phase"));
    let current=value.effects.current_read_facts.as_ref().context("actual Current ACK missing")?;
    assert_eq!(current.base_revision,root.current_reads.as_ref().unwrap().coverage().revision);
    assert_eq!(current.revision,current.base_revision+1);
    assert_eq!(counts(&runtime)?,(before.0+1,before.1,before.2));
    assert_eq!(runtime.runtime_db().connection()?.query_row("SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",rusqlite::params![runtime.instance_agent_id(),runtime.inner.identity_incarnation],|r|r.get::<_,u64>(0))?,0);
    drop((loan,p));let last=value.effects.clone();drop(value);
    assert!(weak.upgrade().is_some(),"last original effects holder must retain actual Host guard");
    drop(last);assert!(weak.upgrade().is_none());assert_eq!(ledger.report()?.observed_bytes,baseline);
    Ok(())
}
#[tokio::test]
async fn workspace_first_input_phase_debits_loan_before_original_receipt_raw_and_decoder()->Result<()> {
    let (_home,runtime)=fixture().await?;let _pause=runtime.pause_resource_observer_for_test().await;
    let reservation=runtime.inner.mutation_owner.reserve().await?;
    let root=runtime.inner.mutation_owner.capture()?.context("actual published root missing")?;
    let ledger=&runtime.inner.input_evidence_budget;
    let source=reservation.input_query_source(&scope(&runtime),ledger,(64*1024*1024,65536))?;
    let p=posture(&runtime,&root,"workspace-first-original-receipt")?;
    let InputMutationExecution::Committed {value,receipt}=commit(&runtime,&p,&source,ledger,64*1024*1024)? else {anyhow::bail!("original seed State must commit")};drop(value);
    let raw:String=runtime.runtime_db().connection()?.query_row("SELECT outcome_json FROM input_mutation_receipts WHERE command_nonce=?1",[p.descriptor().command_nonce()],|r|r.get(0))?;
    // Only valid trailing JSON whitespace grows the original whole receipt.
    // Typed outcome and checksum are unchanged; no outcome field is invented.
    let padded=format!("{raw}{}"," ".repeat(64*1024));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&raw)?,serde_json::from_str::<serde_json::Value>(&padded)?);
    runtime.runtime_db().connection()?.execute("UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce=?2",rusqlite::params![padded,p.descriptor().command_nonce()])?;
    let InputMutationExecution::Replayed {receipt:actual,..}=commit(&runtime,&p,&source,ledger,64*1024*1024)? else {anyhow::bail!("valid padded original receipt must replay")};assert_eq!(actual,receipt);
    let borrowed="w".repeat(256*1024);let loan=runtime.prepare_original_workspace_request_material(&borrowed)?;
    let bytes=loan.material_estimate()?.0.checked_add(128*1024).context("fixture joint bytes overflow")?;
    assert!(padded.len()*8<bytes,"receipt fits alone and fails jointly with the real loan");
    let p=p.with_original_workspace_request_material(loan.clone(),reservation.owner_epoch(),ledger)?;
    let before=counts(&runtime)?;let state=runtime.runtime_db().agent_states().latest(runtime.instance_agent_id())?;
    let baseline=ledger.report()?.observed_bytes;
    for invalid in [false,true] {
        if invalid {runtime.runtime_db().connection()?.execute("UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce=?2",rusqlite::params![format!("!{padded}"),p.descriptor().command_nonce()])?;}
        let error=commit(&runtime,&p,&source,ledger,bytes).err().context("real Workspace/receipt combination must reject")?;
        assert!(matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::ResourceExhausted(_))),"{error:#}");
        assert_eq!(counts(&runtime)?,before);assert_eq!(runtime.runtime_db().agent_states().latest(runtime.instance_agent_id())?,state);
        assert_eq!(ledger.report()?.observed_bytes,baseline);
    }
    runtime.runtime_db().connection()?.execute("UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce=?2",rusqlite::params![padded,p.descriptor().command_nonce()])?;
    let InputMutationExecution::Replayed {receipt:actual,..}=commit(&runtime,&p,&source,ledger,64*1024*1024)? else {anyhow::bail!("adequate original replay must succeed")};assert_eq!(actual,receipt);
    Ok(())
}
#[tokio::test]
async fn workspace_first_input_phase_rejects_foreign_actual_ledger_before_scope_decoder()->Result<()> {
    let (_home,runtime)=fixture().await?;let (_other_home,other)=fixture().await?;
    let _pause=runtime.pause_resource_observer_for_test().await;let _other_pause=other.pause_resource_observer_for_test().await;
    let reservation=runtime.inner.mutation_owner.reserve().await?;
    let root=runtime.inner.mutation_owner.capture()?.context("actual published root missing")?;
    let ledger=&runtime.inner.input_evidence_budget;
    let source=reservation.input_query_source(&scope(&runtime),ledger,(64*1024*1024,65536))?;
    let loan=runtime.prepare_original_workspace_request_material(&"actual owned Workspace request")?;
    let p=posture(&runtime,&root,"workspace-first-foreign-ledger")?.with_original_workspace_request_material(loan,reservation.owner_epoch(),ledger)?;
    let raw:String=runtime.runtime_db().connection()?.query_row("SELECT payload_json FROM agent_states WHERE agent_id=?1",[runtime.instance_agent_id()],|r|r.get(0))?;
    runtime.runtime_db().connection()?.execute("UPDATE agent_states SET payload_json='not-json' WHERE agent_id=?1",[runtime.instance_agent_id()])?;
    let before=counts(&runtime)?;let baseline=ledger.report()?.observed_bytes;let other_baseline=other.inner.input_evidence_budget.report()?.observed_bytes;
    let error=commit(&runtime,&p,&source,&other.inner.input_evidence_budget,64*1024*1024).err().context("foreign actual ledger must reject before State")?;
    assert!(format!("{error:#}").contains("owned material ledger differs"),"{error:#}");
    assert_eq!(counts(&runtime)?,before);assert_eq!(ledger.report()?.observed_bytes,baseline);assert_eq!(other.inner.input_evidence_budget.report()?.observed_bytes,other_baseline);
    runtime.runtime_db().connection()?.execute("UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",rusqlite::params![raw,runtime.instance_agent_id()])?;
    Ok(())
}
