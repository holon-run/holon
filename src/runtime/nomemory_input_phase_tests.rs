use super::*;
use crate::runtime_db::{
    input_evidence_snapshot::InputEvidenceSnapshotError,
    task_snapshot::TaskDomainLoadRequest,
    transitions::{InputMutationExecution,PreparedInputTransition,PreparedTaskTransition,TaskTransitionCommand,TransitionCommit,TransitionFaultPoint},
    work_snapshot::WorkDomainLoadRequest,
};
async fn fixture()->Result<(tempfile::TempDir,RuntimeHandle,TaskRecord)> {
    let home=tests::support::tempdir()?;
    let runtime=RuntimeHandle::new_isolated_resource_fixture("nomemory-actual-task-phase",home.path().into(),tests::support::context_config())?;
    runtime.refresh_context_resources().await?;
    let at=runtime.now();
    let task=TaskRecord {id:crate::ids::task_id(),agent_id:runtime.instance_agent_id().into(),kind:TaskKind::WorktreeSubagentTask,
        status:TaskStatus::Queued,created_at:at,updated_at:at,parent_message_id:None,work_item_id:None,summary:None,
        detail:Some(serde_json::json!("original scalar detail")),recovery:None};
    runtime.persist_task_transition(&task,"nomemory_original_actual_task_seed").await?;
    Ok((home,runtime,task))
}
fn scope(runtime:&RuntimeHandle)->resident_input_evidence::InputEvidenceScope {
    resident_input_evidence::InputEvidenceScope {agent_id:runtime.instance_agent_id().into(),identity_incarnation:runtime.instance_incarnation()}
}
fn policy(bytes:usize)->TaskDomainLoadRequest {
    TaskDomainLoadRequest {query:Default::default(),capture:resident_task::TaskCaptureRequest::current(),byte_limit:bytes,record_limit:65536}
}
fn prepared(runtime:&RuntimeHandle,task:&TaskRecord,nonce:&str,fault:Option<TransitionFaultPoint>,root:&Arc<resident_state::AgentSnapshot>)->Result<PreparedInputTransition> {
    let mut updated=task.clone();updated.status=TaskStatus::Running;updated.updated_at+=chrono::Duration::milliseconds(1);
    PreparedInputTransition::task(scope(runtime),nonce.into(),runtime.now(),PreparedTaskTransition::new(TaskTransitionCommand {
        agent_id:task.agent_id.clone(),task:updated,agent_deletion:None,task_result_settlement:None,queue_entry:None,
        work_items:Vec::new(),expected_wait_conditions:Vec::new(),wait_conditions:Vec::new(),agent_state:None,
        message_evidence:Vec::new(),audit_events:Vec::new(),index_changes:Vec::new(),notify_scheduler:false,commit_on_idempotent:false,fault},
        Default::default(),false,None,None)?)?.with_current_read_source(root.current_reads.as_ref().context("actual Current source missing")?.clone())
}
fn commit(runtime:&RuntimeHandle,p:&PreparedInputTransition,source:&Arc<mutation_owner::OriginalInputQuerySource>,bytes:usize)->Result<InputMutationExecution<TransitionCommit>> {
    runtime.runtime_db().transitions_in_scope(&scope(runtime)).commit_prepared_transition_with_input_query_source(p,
        &WorkDomainLoadRequest {capture:resident_work::WorkCaptureRequest::current(),byte_limit:bytes,record_limit:65536},
        &runtime.inner.input_evidence_budget,&policy(bytes),source)
}
fn counts(runtime:&RuntimeHandle)->Result<(usize,usize,usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM input_mutation_receipts),(SELECT COUNT(*) FROM input_query_birth_members),(SELECT COUNT(*) FROM task_domain_records)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
}
fn compound(runtime:&RuntimeHandle,source:&str)->Result<Arc<OriginalWorkspaceRequestMaterial>> {
    let loan=runtime.prepare_original_workspace_request_material(&source)?;
    let copy=crate::runtime_db::command_task_terminal::charge_material(&scope(runtime),4096,64,&policy(64*1024*1024),&runtime.inner.input_evidence_budget)?;
    let baseline=runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let compound=loan.with_original_task_copy(copy,64)?;
    assert_eq!(runtime.inner.input_evidence_budget.report()?.observed_bytes,baseline,"borrowing cannot register the actual Task copy twice");
    Ok(compound)
}
#[tokio::test]
async fn ordinary_nomemory_task_borrows_compound_workspace_loan_before_all_family_phases_and_keeps_last_ack_holder()->Result<()> {
    let (_home,runtime,task)=fixture().await?;let _pause=runtime.pause_resource_observer_for_test().await;
    let reservation=runtime.inner.mutation_owner.reserve().await?;let root=runtime.inner.mutation_owner.capture()?.unwrap();
    let ledger=&runtime.inner.input_evidence_budget;
    let source=reservation.input_query_source(&scope(&runtime),ledger,(64*1024*1024,65536))?;
    let baseline=ledger.report()?.observed_bytes;
    let compound=compound(&runtime,"actual owned parent request")?;
    let mut weak=Vec::new();compound.visit_actual_guards(|g|{weak.push(Arc::downgrade(g));Ok(())})?;assert_eq!(weak.len(),2);
    let p=prepared(&runtime,&task,"nomemory-compound-task-whole",None,&root)?
        .with_original_task_workspace_request_material(compound.clone(),reservation.owner_epoch(),ledger)?;
    let before=counts(&runtime)?;
    let InputMutationExecution::Committed {value,receipt}=commit(&runtime,&p,&source,64*1024*1024)? else {anyhow::bail!("actual Task whole ACK absent")};
    assert!(value.applied&&receipt.outcome.applied);
    let actual=runtime.runtime_db().tasks().latest(&task.id)?.unwrap();assert_eq!(actual.status,TaskStatus::Running);assert_eq!(actual.created_at,task.created_at);assert_eq!(actual.detail,task.detail);
    assert!(value.effects.task_delta.is_some()&&value.effects.work_delta.is_some()&&value.effects.current_read_facts.is_some());
    assert_eq!(counts(&runtime)?,(before.0+1,before.1,before.2));
    assert!(Arc::ptr_eq(value.effects._original_workspace_material.as_ref().unwrap(),&compound));
    drop((p,compound));let last=value.effects.clone();drop(value);assert!(weak.iter().all(|g|g.upgrade().is_some()));
    drop(last);assert!(weak.iter().all(|g|g.upgrade().is_none()));assert_eq!(ledger.report()?.observed_bytes,baseline);
    Ok(())
}
#[tokio::test]
async fn ordinary_nomemory_task_loan_rejects_before_raw_task_decode_and_whole_tx_failure_keeps_original_receipt_boundary()->Result<()> {
    let (_home,runtime,task)=fixture().await?;let _pause=runtime.pause_resource_observer_for_test().await;
    let reservation=runtime.inner.mutation_owner.reserve().await?;let root=runtime.inner.mutation_owner.capture()?.unwrap();let ledger=&runtime.inner.input_evidence_budget;
    let source=reservation.input_query_source(&scope(&runtime),ledger,(64*1024*1024,65536))?;
    let borrowed="q".repeat(256*1024);let compound=compound(&runtime,&borrowed)?;let limit=compound.material_estimate()?.0+160*1024;
    let p=prepared(&runtime,&task,"nomemory-compound-budget",None,&root)?
        .with_original_task_workspace_request_material(compound.clone(),reservation.owner_epoch(),ledger)?;
    let raw:String=runtime.runtime_db().connection()?.query_row("SELECT payload_json FROM tasks WHERE task_id=?1",[&task.id],|r|r.get(0))?;
    let poisoned=format!("!{}","x".repeat(96*1024));assert!(poisoned.len()*8<limit);
    runtime.runtime_db().connection()?.execute("UPDATE tasks SET payload_json=?1 WHERE task_id=?2",rusqlite::params![poisoned,task.id])?;
    let before=counts(&runtime)?;let baseline=ledger.report()?.observed_bytes;
    let error=commit(&runtime,&p,&source,limit).err().context("actual shared Task loan must reject before malformed decoder")?;
    assert!(matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::ResourceExhausted(_)))
        || matches!(error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),Some(crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(_))),"{error:#}");
    assert_eq!(counts(&runtime)?,before);assert_eq!(ledger.report()?.observed_bytes,baseline);
    runtime.runtime_db().connection()?.execute("UPDATE tasks SET payload_json=?1 WHERE task_id=?2",rusqlite::params![raw,task.id])?;
    let failed=prepared(&runtime,&task,"nomemory-compound-rollback",Some(TransitionFaultPoint::BeforeCommit),&root)?
        .with_original_task_workspace_request_material(compound.clone(),reservation.owner_epoch(),ledger)?;
    assert!(commit(&runtime,&failed,&source,64*1024*1024).is_err());
    assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?,Some(task.clone()));assert_eq!(counts(&runtime)?,before);assert_eq!(ledger.report()?.observed_bytes,baseline);
    let InputMutationExecution::Committed {value,..}=commit(&runtime,&p,&source,64*1024*1024)? else {anyhow::bail!("same descriptor retry failed")};
    assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?.unwrap().status,TaskStatus::Running);drop(value);assert_eq!(ledger.report()?.observed_bytes,baseline);
    Ok(())
}
#[tokio::test]
async fn ordinary_nomemory_task_receipt_replay_is_bounded_by_the_same_compound_loan_before_json()->Result<()> {
    let (_home,runtime,task)=fixture().await?;let _pause=runtime.pause_resource_observer_for_test().await;
    let reservation=runtime.inner.mutation_owner.reserve().await?;let root=runtime.inner.mutation_owner.capture()?.unwrap();let ledger=&runtime.inner.input_evidence_budget;
    let source=reservation.input_query_source(&scope(&runtime),ledger,(64*1024*1024,65536))?;
    let p=prepared(&runtime,&task,"nomemory-compound-receipt",None,&root)?;
    let InputMutationExecution::Committed {value,receipt}=commit(&runtime,&p,&source,64*1024*1024)? else {anyhow::bail!("original Task ACK missing")};drop(value);
    let raw:String=runtime.runtime_db().connection()?.query_row("SELECT outcome_json FROM input_mutation_receipts WHERE command_nonce=?1",[p.descriptor().command_nonce()],|r|r.get(0))?;
    let padded=format!("{raw}{}"," ".repeat(64*1024));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&raw)?,serde_json::from_str::<serde_json::Value>(&padded)?);
    let borrowed="r".repeat(256*1024);let compound=compound(&runtime,&borrowed)?;let limit=compound.material_estimate()?.0+128*1024;assert!(padded.len()*8<limit);
    let p=p.with_original_task_workspace_request_material(compound,reservation.owner_epoch(),ledger)?;
    let before=counts(&runtime)?;let baseline=ledger.report()?.observed_bytes;
    for malformed in [false,true] {
        let actual=if malformed {format!("!{padded}")} else {padded.clone()};
        runtime.runtime_db().connection()?.execute("UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce=?2",rusqlite::params![actual,p.descriptor().command_nonce()])?;
        let error=commit(&runtime,&p,&source,limit).err().context("shared Task receipt must reject before JSON")?;
        assert!(matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::ResourceExhausted(_))),"{error:#}");
        assert_eq!(counts(&runtime)?,before);assert_eq!(ledger.report()?.observed_bytes,baseline);
    }
    runtime.runtime_db().connection()?.execute("UPDATE input_mutation_receipts SET outcome_json=?1 WHERE command_nonce=?2",rusqlite::params![padded,p.descriptor().command_nonce()])?;
    let InputMutationExecution::Replayed {receipt:actual,..}=commit(&runtime,&p,&source,64*1024*1024)? else {anyhow::bail!("adequate original Task replay failed")};
    assert_eq!(actual,receipt);assert_eq!(counts(&runtime)?,before);assert_eq!(ledger.report()?.observed_bytes,baseline);Ok(())
}
#[tokio::test]
async fn ordinary_nomemory_scheduling_finalizer_returns_actual_usage_and_rolls_back_the_same_whole_receipt()->Result<()> {
    let (_home,runtime,_task)=fixture().await?;let _pause=runtime.pause_resource_observer_for_test().await;
    let reservation=runtime.inner.mutation_owner.reserve().await?;let root=runtime.inner.mutation_owner.capture()?.unwrap();let ledger=&runtime.inner.input_evidence_budget;
    let source=reservation.input_query_source(&scope(&runtime),ledger,(64*1024*1024,65536))?;
    let at=chrono::DateTime::from_timestamp_millis(runtime.now().timestamp_millis()).context("fixture millis clock")?;let timer=crate::types::TimerRecord {id:crate::ids::timer_id(),agent_id:runtime.instance_agent_id().into(),created_at:at,
        duration_ms:1000,interval_ms:None,repeat:false,status:crate::types::TimerStatus::Active,summary:Some("actual original NoMemory Timer".into()),
        next_fire_at:Some(at+chrono::Duration::seconds(1)),last_fired_at:None,fire_count:0};
    let p=PreparedInputTransition::timer(scope(&runtime),"nomemory-scheduling-whole".into(),at,
        crate::runtime_db::transitions::PreparedTimerTransition::create(timer.clone(),None))?
        .with_current_read_source(root.current_reads.as_ref().context("actual Current absent")?.clone())?;
    let before=counts(&runtime)?;let baseline=ledger.report()?.observed_bytes;
    runtime.runtime_db().connection()?.execute_batch("CREATE TRIGGER original_nomemory_scheduling_fault BEFORE INSERT ON scheduling_domain_records BEGIN SELECT RAISE(ABORT,'actual-shared-scheduling-finalizer'); END;")?;
    let error=commit(&runtime,&p,&source,64*1024*1024).err().context("actual original finalizer must fail")?;
    assert!(format!("{error:#}").contains("actual-shared-scheduling-finalizer"),"{error:#}");
    assert!(runtime.runtime_db().timers().latest(&timer.id)?.is_none());assert_eq!(counts(&runtime)?,before);assert_eq!(ledger.report()?.observed_bytes,baseline);
    runtime.runtime_db().connection()?.execute_batch("DROP TRIGGER original_nomemory_scheduling_fault;")?;
    let InputMutationExecution::Committed {value,receipt}=commit(&runtime,&p,&source,64*1024*1024)? else {anyhow::bail!("actual original Timer retry absent")};
    assert!(receipt.outcome.applied);assert!(value.effects.scheduling_delta.is_some()&&value.effects.current_read_facts.is_some());
    assert_eq!(runtime.runtime_db().timers().latest(&timer.id)?,Some(timer));assert!(ledger.report()?.observed_bytes>baseline);
    let last=value.effects.clone();drop(value);assert!(ledger.report()?.observed_bytes>baseline);drop(last);assert_eq!(ledger.report()?.observed_bytes,baseline);
    Ok(())
}

#[tokio::test]
async fn ordinary_nomemory_workspace_task_accepts_only_one_original_input_selection_and_keeps_its_actual_cost()->Result<()> {
    let (_home,runtime,task)=fixture().await?;let _pause=runtime.pause_resource_observer_for_test().await;
    let reservation=runtime.inner.mutation_owner.reserve().await?;let root=runtime.inner.mutation_owner.capture()?.unwrap();
    let ledger=&runtime.inner.input_evidence_budget;let actual_scope=scope(&runtime);
    let source=reservation.input_query_source(&actual_scope,ledger,(64*1024*1024,65536))?;
    let inputs=root.input_evidence.as_ref().context("original owner Input missing")?;
    let compound=compound(&runtime,"actual Task owner and its preserved Input request")?;
    // Both shapes contain real bounded selector guards. Only the one central
    // attachment is supported; a second layer cannot widen the closed factory.
    for layers in [2,1] {
        let nonce=format!("nomemory-workspace-input-selection-{layers}");
        let mut p=prepared(&runtime,&task,&nonce,None,&root)?
            .with_original_task_workspace_request_material(compound.clone(),reservation.owner_epoch(),ledger)?;
        for _ in 0..layers {p=p.with_work_input_selection(inputs,ledger,(64*1024*1024,65536))?;}
        let cost=p.original_work_material_prior(ledger)?;
        assert!(cost.0>0 && cost.1>0,"Input selection has actual metadata, not an empty loan");
        let before=counts(&runtime)?;let baseline=ledger.report()?.observed_bytes;
        if layers==2 {
            let error=commit(&runtime,&p,&source,64*1024*1024).err().context("nested selector loan must stay rejected")?;
            assert!(format!("{error:#}").contains("Workspace material requires its original closed standalone State or Task command"),"{error:#}");
            assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?,Some(task.clone()));
            assert_eq!(counts(&runtime)?,before);assert_eq!(ledger.report()?.observed_bytes,baseline);
        } else {
            let InputMutationExecution::Committed {value,receipt}=commit(&runtime,&p,&source,64*1024*1024)?
                else {anyhow::bail!("original Task Input selector ACK missing")};
            assert!(value.applied && receipt.outcome.applied);
            assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?.unwrap().status,TaskStatus::Running);
            assert!(value.effects.task_delta.is_some() && value.effects.work_delta.is_some());
            assert_eq!(counts(&runtime)?.0,before.0+1);
            drop(value);assert_eq!(ledger.report()?.observed_bytes,baseline,"original selector guard is borrowed once");
        }
    }
    Ok(())
}

#[inline(never)]
fn automatic_workspace_task_case(fail_before_commit:bool)
    ->std::pin::Pin<Box<impl std::future::Future<Output=Result<()>>+Send>> {
    Box::pin(async move {
        let (_home,runtime,task)=fixture().await?;let _pause=runtime.pause_resource_observer_for_test().await;
        let message=MessageEnvelope::new(runtime.instance_agent_id(),MessageKind::OperatorPrompt,
            MessageOrigin::Operator {actor_id:None,actor_display_name:None},AuthorityClass::OperatorInstruction,
            Priority::Normal,MessageBody::Text {text:"original automatic Workspace Task dispatch".into()});
        runtime.enqueue(message).await?;
        let scheduler_executor::RunLoopPoll::Message(scheduled)=scheduler_executor::SchedulerDecisionExecutor::new(&runtime).poll().await?
            else {anyhow::bail!("actual Workspace Task dispatch claim missing")};
        let (turn,_)=runtime.begin_dispatch_turn(&scheduled.message,None,None,
            scheduled.dispatch_plan.execution_admission_provenance.clone(),&scheduled.dispatch_plan.prior_closure).await?;
        let actual_scope=scope(&runtime);let state=runtime.agent_state().await?;
        let binding=runtime.runtime_db().transaction(|tx|
            crate::runtime_db::dispatch_memory::load_dispatch_memory_binding_tx(tx,&actual_scope,&state))?
            .context("real StartedTurn did not install original Memory")?;
        assert_eq!(binding.key().actual_dispatch_turn_id,turn);
        let compound=compound(&runtime,"original bounded Workspace request across automatic Deferred")?;
        let mut weak=Vec::new();compound.visit_actual_guards(|g|{weak.push(Arc::downgrade(g));Ok(())})?;
        let command=|fault| {
            let mut updated=task.clone();updated.status=TaskStatus::Running;updated.updated_at=runtime.now();
            TaskTransitionCommand {agent_id:task.agent_id.clone(),task:updated,agent_deletion:None,task_result_settlement:None,
                queue_entry:None,work_items:Vec::new(),expected_wait_conditions:Vec::new(),wait_conditions:Vec::new(),agent_state:None,
                message_evidence:Vec::new(),audit_events:Vec::new(),index_changes:Vec::new(),notify_scheduler:false,commit_on_idempotent:false,fault}
        };
        let before_state=runtime.runtime_db().agent_states().latest(runtime.instance_agent_id())?;
        let before_memory:String=runtime.runtime_db().connection()?.query_row(
            "SELECT payload_json FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
            rusqlite::params![actual_scope.agent_id,actual_scope.identity_incarnation,turn],|r|r.get(0))?;
        let nonce=if fail_before_commit {"automatic-workspace-task-rollback"} else {"automatic-workspace-task-whole"};
        if !fail_before_commit {
            let reservation=runtime.inner.mutation_owner.reserve().await?;let root=runtime.inner.mutation_owner.capture()?.unwrap();
            let ledger=&runtime.inner.input_evidence_budget;
            let source=reservation.input_query_source(&actual_scope,ledger,(64*1024*1024,65536))?;
            let explicit=prepared(&runtime,&task,"workspace-explicit-deferred-must-reject",None,&root)?
                .with_original_task_workspace_request_material(compound.clone(),reservation.owner_epoch(),ledger)?
                .with_work_input_selection(root.input_evidence.as_ref().context("actual Input missing")?,ledger,(64*1024*1024,65536))?
                .with_dispatch_memory(crate::runtime_db::dispatch_memory::DispatchMemoryIntent::Deferred {binding:binding.clone(),
                    frozen:crate::memory::episode_transition::FrozenEpisodeMemoryValues {timestamp:runtime.now(),
                        initial_builder_id:crate::ids::episode_id(),fallback_builder_id:crate::ids::episode_id(),next_builder_id:crate::ids::episode_id(),audit_event_id:crate::ids::audit_event_id()}})?;
            let before=counts(&runtime)?;let baseline=ledger.report()?.observed_bytes;
            let error=commit(&runtime,&explicit,&source,64*1024*1024).err().context("explicit Deferred cannot borrow automatic Workspace branch")?;
            assert!(format!("{error:#}").contains("Workspace material requires its original closed standalone State or Task command"),"{error:#}");
            assert_eq!(counts(&runtime)?,before);assert_eq!(ledger.report()?.observed_bytes,baseline);
        }
        let mut owner=runtime.begin_mutation().await?;
        let result=owner.commit_task_transition_with_original_workspace_request(nonce.into(),
            command(fail_before_commit.then_some(TransitionFaultPoint::BeforeCommit)),Default::default(),false,compound.clone()).await;
        if fail_before_commit {
            let error=result.err().context("original Task whole transaction fault omitted")?;
            assert!(format!("{error:#}").contains("BeforeCommit") || format!("{error:#}").contains("before commit"),"{error:#}");
            assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?,Some(task.clone()));
            let count:usize=runtime.runtime_db().connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1)+(SELECT COUNT(*) FROM dispatch_memory_auto_commands WHERE command_nonce=?1)",[nonce],|r|r.get(0))?;
            assert_eq!(count,0,"automatic decoration and whole receipt roll back together");
        } else {
            let (value,deletion)=result?;assert!(value.applied && deletion.is_none());
            assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?.unwrap().status,TaskStatus::Running);
            assert_eq!(runtime.runtime_db().tasks().latest(&task.id)?.unwrap().created_at,task.created_at);
            let (kind,actual_turn):(String,String)=runtime.runtime_db().connection()?.query_row(
                "SELECT json_extract(payload_json,'$.intent.kind'),json_extract(payload_json,'$.intent.binding.key.actual_dispatch_turn_id') FROM dispatch_memory_auto_commands WHERE command_nonce=?1",[nonce],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!(kind,"deferred");assert_eq!(actual_turn,turn);
            assert!(weak.iter().all(|g|g.upgrade().is_some()));drop(value);
        }
        assert_eq!(runtime.runtime_db().agent_states().latest(runtime.instance_agent_id())?,before_state);
        let after_memory:String=runtime.runtime_db().connection()?.query_row(
            "SELECT payload_json FROM dispatch_memory_checkpoints WHERE agent_id=?1 AND identity_incarnation=?2 AND turn_id=?3",
            rusqlite::params![actual_scope.agent_id,actual_scope.identity_incarnation,turn],|r|r.get(0))?;
        assert_eq!(after_memory,before_memory,"Task-only metadata creates no Work Memory member");
        drop(owner);drop(compound);
        assert!(weak.iter().all(|g|g.upgrade().is_none()),"published Root cannot retain original Host/Task-copy loan");
        assert!(runtime.inner.mutation_owner.capture()?.is_some());
        drop(scheduled);
        Ok(())
    })
}
#[tokio::test]
async fn actual_runtime_workspace_task_borrows_automatic_deferred_memory_and_releases_original_loan_after_whole_ack()->Result<()> {
    automatic_workspace_task_case(false).await
}
#[tokio::test]
async fn actual_runtime_workspace_task_automatic_deferred_and_receipt_roll_back_with_original_task()->Result<()> {
    automatic_workspace_task_case(true).await
}
