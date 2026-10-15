use super::*;
use crate::{
    runtime::{input_queries::{source_roles_for_fixture,InputQuerySpec},mutation_owner::MutationOwnerRegistry},
    runtime_db::transitions::{InputMutationExecution,PreparedTimerTransition},
    types::{TimerRecord,TimerStatus},
};
#[test]
fn actual_started_memory_timer_readback_borrows_original_state_and_retains_only_whole_ack_guards()->Result<()> {
    for fail_finalizer in [false,true] {
        // The existing fixture commits a real StartedTurn and its M2 baseline.
        let f=Fixture::new(true)?;
        let ledger=crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
        let owners=MutationOwnerRegistry::default();let owner=owners.claim(&f.scope.agent_id,f.scope.identity_incarnation)?;
        let (reservation,lease,roles)=source_roles_for_fixture(&owner,&f.scope,InputQuerySpec {birth_after:Some(0),..Default::default()},&ledger,(64*1024*1024,65536))?;drop(roles);
        let source=reservation.input_query_source(&f.scope,&ledger,(64*1024*1024,65536))?;
        let baseline=ledger.report()?.observed_bytes;
        let at=chrono::DateTime::from_timestamp_millis(f.at.timestamp_millis()).context("Timer fixture clock")?;
        let timer=TimerRecord {id:"original-memory-timer-readback".into(),agent_id:f.scope.agent_id.clone(),created_at:at,
            duration_ms:1000,interval_ms:None,repeat:false,status:TimerStatus::Active,summary:Some("original bounded State readback".into()),
            next_fire_at:Some(at+chrono::Duration::seconds(1)),last_fired_at:None,fire_count:0};
        let p=PreparedInputTransition::timer(f.scope.clone(),"original-memory-timer-readback".into(),f.at,PreparedTimerTransition::create(timer.clone(),None))?
            .with_dispatch_memory(DispatchMemoryIntent::Deferred {binding:DispatchMemoryBinding::from_start(&f.start),frozen:f.frozen("original-memory-timer-readback")})?;
        let tasks=crate::runtime_db::task_snapshot::TaskDomainLoadRequest {query:Default::default(),capture:crate::runtime::resident_task::TaskCaptureRequest::current(),byte_limit:64*1024*1024,record_limit:65536};
        let work=WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit:64*1024*1024,record_limit:65536};
        let before_state=f.db.agent_states().latest(&f.scope.agent_id)?;let before_memory=f.read()?;
        if fail_finalizer {f.db.connection()?.execute_batch("CREATE TRIGGER original_memory_scheduling_finalizer_fault BEFORE INSERT ON scheduling_domain_records BEGIN SELECT RAISE(ABORT,'original-memory-scheduling-finalizer'); END;")?;}
        let result=f.db.transitions_in_scope(&f.scope).commit_prepared_transition_with_input_query_source(&p,&work,&ledger,&tasks,&source);
        if fail_finalizer {
            let error=result.err().context("actual Scheduling readback fault omitted")?;
            assert!(format!("{error:#}").contains("original-memory-scheduling-finalizer"),"{error:#}");
            assert!(f.db.timers().latest(&timer.id)?.is_none());assert_eq!(f.db.agent_states().latest(&f.scope.agent_id)?,before_state);assert_eq!(f.read()?,before_memory);
            assert_eq!(f.db.connection()?.query_row("SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1",[p.descriptor().command_nonce()],|r|r.get::<_,usize>(0))?,0);
            f.db.connection()?.execute_batch("DROP TRIGGER original_memory_scheduling_finalizer_fault;")?;
        } else {
            let InputMutationExecution::Committed {value,receipt}=result? else {anyhow::bail!("actual original Timer whole ACK absent")};
            assert!(value.applied&&receipt.outcome.applied);assert_eq!(f.db.timers().latest(&timer.id)?,Some(timer));
            assert!(value.effects.scheduling_delta.is_some()&&!value.effects.scheduling_material_budget_views.is_empty());
            assert_eq!(f.db.agent_states().latest(&f.scope.agent_id)?,before_state);assert_eq!(f.read()?,before_memory,"no actual Work write creates no new deferred member");
            let last=value.effects.clone();drop(value);assert!(ledger.report()?.observed_bytes>baseline);drop(last);
        }
        assert_eq!(ledger.report()?.observed_bytes,baseline);drop((source,lease,reservation));assert_eq!(ledger.report()?.observed_bytes,0);
    }
    Ok(())
}
