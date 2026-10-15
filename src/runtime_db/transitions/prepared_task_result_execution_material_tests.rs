//! Actual Result execution exits before final readback; ACK and W1 stay live.
use super::*;
use crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial;
use super::super::super::task_writes;

#[test]
fn consumed_result_selection_and_predicate_retire_only_after_actual_ack_and_last_guard() -> Result<()> {
    for predicate in [false,true] {
        let f=Fixture::new()?;let ledger=ledger()?;
        let (source,_)=pending(&f,&ledger,false)?;
        let first=freeze_result(&f,"retirement:first",TaskResultOperation::Defer {
            message_id:source.id.clone(),reason:"x".repeat(96*1024),at:f.at+chrono::Duration::seconds(1),
            deadline:f.at+chrono::Duration::seconds(20),
        })?;
        drop(execute(&f,&first,&request(),&ledger)?);
        let operation=if predicate {TaskResultOperation::AdmitMessage {
            message_id:source.id.clone(),activation_id:"retirement:activation".into(),at:f.at+chrono::Duration::seconds(2),
        }} else {TaskResultOperation::Defer {
            message_id:source.id.clone(),reason:"y".repeat(96*1024),at:f.at+chrono::Duration::seconds(2),
            deadline:f.at+chrono::Duration::seconds(30),
        }};
        let p=freeze_result(&f,"retirement:actual",operation)?;
        let mut policy=request();policy.byte_limit=8*1024*1024;
        let value=f.db.transaction(|tx| {
            let mut task=task_writes::prepare_task_transaction_tx(tx,&p,&f.scope,false,&policy,&ledger,None)?
                .context("actual Result phase missing")?;
            let before=task.input_writer_material(&f.scope,&ledger)?;
            let guard=task.result_selection_guard_for_test()?;
            let ledger_before=ledger.report()?.observed_bytes;
            let early=task.retire_result_execution(&f.scope,&ledger,&results::TaskResultWriteOutcome::Unit,&BTreeSet::new())
                .err().context("live Result decoder retired")?;
            assert!(format!("{early:#}").contains("still live"));
            assert_eq!(task.input_writer_material(&f.scope,&ledger)?,before);
            assert_eq!(ledger.report()?.observed_bytes,ledger_before);
            let ack=task.execute_result()?;
            let actual=ack.actual_result_ids.iter().map(|id|TaskKey::new(TaskFamily::ResultSettlement,id)).collect();
            let escaped=task.retire_result_execution(&f.scope,&ledger,&ack.outcome,&actual)
                .err().context("escaped decoder guard retired")?;
            assert!(format!("{escaped:#}").contains("another live holder"));
            assert_eq!(task.input_writer_material(&f.scope,&ledger)?,before);
            drop(guard);
            let before_retire=ledger.report()?.observed_bytes;
            let released=task.retire_result_execution(&f.scope,&ledger,&ack.outcome,&actual)?.context("retirement missing")?;
            let (freed,additional)=released.validate_original_result_exchange(&f.scope,&ledger)?;
            let after=task.input_writer_material(&f.scope,&ledger)?;
            assert_eq!(after,(before.0-freed.0+additional.0,before.1-freed.1+additional.1));
            assert!(freed.0>96*1024 && freed.1>0);
            assert_eq!(ledger.report()?.observed_bytes,before_retire-u64::try_from(freed.0)?+u64::try_from(additional.0)?);
            drop(released);
            assert!(task.retire_result_execution(&f.scope,&ledger,&ack.outcome,&actual)?.is_none());
            let results::TaskResultWriteOutcome::Record(Some(record))=&ack.outcome else {anyhow::bail!("actual ACK lost its original record")};
            assert_eq!(record.deferred_reason.as_deref().map(str::len),if predicate {None}else{Some(96*1024)});
            assert_eq!(task.task_write_proof().scope(),&f.scope);
            let mut value=TransitionCommit::default();value.effects.task_write_keys=actual;
            task.finish(tx,&mut value,&ledger,None)?;
            Ok((value,ack))
        })?;
        let (value,ack)=value;assert!(ledger.report()?.observed_bytes>0);
        let last=value.effects.clone();drop(value);assert!(ledger.report()?.observed_bytes>0);
        drop(ack);drop(last);assert_eq!(ledger.report()?.observed_bytes,0);
    }
    Ok(())
}

#[test]
fn consumed_result_execution_releases_the_same_original_input_memory_prior_once() -> Result<()> {
    use crate::runtime_db::dispatch_memory::{DispatchMemoryIntent,DispatchMemoryStart,DispatchMemoryTransactionBudget};
    use crate::runtime_db::episode_memory::{EpisodeMemoryKey,EpisodeMemoryInvocation};
    use crate::types::{TurnRecord,ClosureDecision,ClosureOutcome,RuntimePosture};
    let f=Fixture::new()?;let ledger=ledger()?;let (source,_)=pending(&f,&ledger,false)?;
    let first=freeze_result(&f,"memory-retirement:first",TaskResultOperation::Defer {
        message_id:source.id.clone(),reason:"x".repeat(96*1024),at:f.at,
        deadline:f.at+chrono::Duration::seconds(20),
    })?;drop(execute(&f,&first,&request(),&ledger)?);
    let mut turn=TurnRecord::new(&f.scope.agent_id,"memory-retirement:turn",1);
    turn.created_at=f.at;turn.input_message_ids.push(source.id.clone());
    f.db.transaction(|tx| {
        crate::runtime_db::repositories::upsert_turn_record_tx(tx,&turn)?;
        let actual=crate::runtime_db::repositories::committed_turn_record_tx(tx,&f.scope.agent_id,&turn.turn_id)?;
        crate::runtime_db::input_evidence_snapshot::register_committed_input_evidence_tx(tx,&f.scope,
            &[crate::runtime_db::input_evidence_snapshot::CanonicalInputEvidence::Turn(actual.record)])?;
        Ok(())
    })?;
    let state=f.state()?;
    let start=DispatchMemoryStart::new(EpisodeMemoryKey {agent_id:f.scope.agent_id.clone(),identity_incarnation:1,
        actual_dispatch_turn_id:turn.turn_id.clone()},EpisodeMemoryInvocation {message_id:source.id.clone(),turn_index:1},
        &source,ClosureDecision {outcome:ClosureOutcome::Completed,waiting_reason:None,work_signal:None,
            runtime_posture:RuntimePosture::Awake,evidence:Vec::new()},state.working_memory)?;
    let p=freeze_result(&f,"memory-retirement:actual",TaskResultOperation::AdmitMessage {
        message_id:source.id.clone(),activation_id:"memory-retirement:activation".into(),at:f.at+chrono::Duration::seconds(1),
    })?;
    let mut policy=request();policy.byte_limit=8*1024*1024;
    let input_request=crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
        windows:Vec::new(),mandatory_roots:BTreeSet::new(),interrupted_sources:BTreeSet::new(),
        selected_completion_brief_ids:BTreeSet::new(),byte_limit:64*1024*1024,record_limit:65536,
    };
    f.db.transaction(|tx| {
        let mut input=InputReadMaterial::new(&f.scope,&input_request,&ledger,(0,0))?;
        input.scope_facts_tx(tx)?;input.original_receipt_material_tx(tx,p.descriptor())?;
        let memory_policy=crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
            capture:crate::runtime::resident_work::WorkCaptureRequest::current(),byte_limit:64*1024*1024,record_limit:65536,
        };
        let mut memory=DispatchMemoryTransactionBudget::prepare_with_original_input_tx(tx,
            &DispatchMemoryIntent::Started {start:Box::new(start.clone())},&memory_policy,&ledger,None,Some(&input))?;
        let mut input=input.resume_after_original_memory(&mut memory)?;
        let native=crate::runtime_db::input_evidence_snapshot::native_birth::NativeInputWriteProof::prepare_using_input_material_tx(
            tx,&f.scope,|_|Ok(()),&mut input,None)?.context("actual Native phase missing")?;
        let base=input.usage();
        let mut native=native.retain_input_phase(input)?;
        let mut task=task_writes::prepare_task_transaction_tx(tx,&p,&f.scope,false,&policy,&ledger,Some(&mut memory))?
            .context("actual Memory Result missing")?;
        let old=task.input_writer_material(&f.scope,&ledger)?;
        let ack=task.execute_result()?;
        let actual=ack.actual_result_ids.iter().map(|id|TaskKey::new(TaskFamily::ResultSettlement,id)).collect();
        let remaining=memory.remaining_material_limits()?;
        let memory_before=(memory_policy.byte_limit-remaining.0,memory_policy.record_limit-remaining.1);
        assert_eq!(memory_before,(base.0+old.0,base.1+old.1));
        let freed=native.with_input_phase(tx,&ledger,Some(&mut memory),|input| {
            input.observe_original_writer_prior(old)?;
            assert_eq!(input.usage(),memory_before);
            let released=task.retire_result_execution(&f.scope,&ledger,&ack.outcome,&actual)?.context("actual Memory release missing")?;
            let (freed,additional)=released.validate_original_result_exchange(&f.scope,&ledger)?;
            assert_eq!(additional,(0,0),"the existing large selected row releases scratch");
            let ledger_before=ledger.report()?.observed_bytes;
            input.release_consumed_task_result_prior(released)?;
            assert_eq!(input.usage(),(memory_before.0-freed.0,memory_before.1-freed.1));
            assert_eq!(ledger.report()?.observed_bytes,ledger_before,"Memory borrows the replacement guard, without re-registration");
            Ok(freed)
        })?;
        let remaining=memory.remaining_material_limits()?;
        assert_eq!((memory_policy.byte_limit-remaining.0,memory_policy.record_limit-remaining.1),
            (memory_before.0-freed.0,memory_before.1-freed.1));
        assert!(freed.0>0&&freed.1>0);
        drop((task,ack,native,memory));assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn consumed_result_retains_large_frozen_command_and_new_ack_as_distinct_allocations() -> Result<()> {
    let f=Fixture::new()?;let ledger=ledger()?;let (source,_)=pending(&f,&ledger,false)?;
    let p=freeze_result(&f,"retirement:larger-ack",TaskResultOperation::Defer {
        message_id:source.id.clone(),reason:"z".repeat(512*1024),at:f.at+chrono::Duration::seconds(1),
        deadline:f.at+chrono::Duration::seconds(20),
    })?;
    let mut policy=request();policy.byte_limit=64*1024*1024;
    let value=f.db.transaction(|tx| {
        let mut task=task_writes::prepare_task_transaction_tx(tx,&p,&f.scope,false,&policy,&ledger,None)?
            .context("large actual Result phase missing")?;
        let before=task.input_writer_material(&f.scope,&ledger)?;
        let ack=task.execute_result()?;
        let actual=ack.actual_result_ids.iter().map(|id|TaskKey::new(TaskFamily::ResultSettlement,id)).collect();
        let ledger_before=ledger.report()?.observed_bytes;
        let exchanged=task.retire_result_execution(&f.scope,&ledger,&ack.outcome,&actual)?.context("actual ACK exchange missing")?;
        let (released,additional)=exchanged.validate_original_result_exchange(&f.scope,&ledger)?;
        assert!(additional.0>0,"a new large ACK cannot borrow the old small selected row as its allocation");
        assert_eq!(task.input_writer_material(&f.scope,&ledger)?,
            (before.0-released.0+additional.0,before.1-released.1+additional.1));
        assert_eq!(ledger.report()?.observed_bytes,ledger_before-u64::try_from(released.0)?+u64::try_from(additional.0)?);
        let results::TaskResultWriteOutcome::Record(Some(record))=&ack.outcome else {anyhow::bail!("large actual ACK missing")};
        assert_eq!(record.deferred_reason.as_deref().map(str::len),Some(512*1024));
        let InputTransitionCommand::TaskResult(command)=p.command.as_ref() else {unreachable!()};
        let TaskResultOperation::Defer {reason,..}=&command.operation else {unreachable!()};
        assert_eq!(reason.len(),512*1024,"the complete distinct frozen command remains held");
        drop(exchanged);
        let mut value=TransitionCommit::default();value.effects.task_write_keys=actual;
        task.finish(tx,&mut value,&ledger,None)?;drop(ack);Ok(value)
    })?;
    assert!(ledger.report()?.observed_bytes>0);drop(value);assert_eq!(ledger.report()?.observed_bytes,0);
    Ok(())
}
