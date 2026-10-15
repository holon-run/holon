use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::runtime_db::input_evidence_snapshot::{read_material::InputReadMaterial,InputEvidenceLoadRequest,CanonicalInputEvidence};
use crate::runtime_db::task_snapshot::tests::{fresh,scope,task,at};
use crate::types::{MessageEnvelope,MessageKind,MessageOrigin,MessageBody,AuthorityClass,Priority,TaskStatus};

fn task_policy()->TaskDomainLoadRequest {
    TaskDomainLoadRequest {query:Default::default(),capture:TaskCaptureRequest::current(),byte_limit:4*1024*1024,record_limit:65536}
}
fn input_policy(bytes:usize,nodes:usize)->InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {windows:Vec::new(),mandatory_roots:Default::default(),interrupted_sources:Default::default(),
        selected_completion_brief_ids:Default::default(),byte_limit:bytes,record_limit:nodes}
}
fn seed(tx:&Transaction<'_>,ledger:&InputEvidenceBudgetLedger)->Result<CommittedInputEvidenceDelta> {
    let task=task("actual-shared-output-task",TaskStatus::Running);
    let keys=BTreeSet::from([TaskKey::new(TaskFamily::Task,&task.id)]);
    let proof=prepare_task_write_tx(tx,&scope(),&keys)?;
    repositories::upsert_task_tx(tx,&task)?;
    let actual=register_task_writes_tx(tx,&proof,&keys,&AgentState::new("task-agent"),&task_policy())?;
    drop(actual);
    let output=loader::prepare_task_output_snapshot_with_budget_tx(tx,&scope(),&task_policy(),ledger)?.require_current()?;
    assert!(output.resident.capture(&task_policy().capture)?.output_source(&task.id)?.is_none());drop(output);
    let mut message=MessageEnvelope::new("task-agent",MessageKind::TaskResult,MessageOrigin::Task {task_id:task.id.clone()},
        AuthorityClass::RuntimeInstruction,Priority::Normal,MessageBody::Text {text:"actual qualified output head".into()});
    message.id="actual-shared-output-message".into();message.created_at=at();message.metadata=Some(serde_json::json!({"task_id":task.id}));
    let (actual,_)=crate::runtime_db::evidence::append_message_tx(tx,&message)?;
    crate::runtime_db::input_evidence_snapshot::register_committed_input_evidence_tx(tx,&scope(),&[CanonicalInputEvidence::Message(actual)])
}
fn read(tx:&Transaction<'_>,scope:&InputEvidenceScope,ledger:&InputEvidenceBudgetLedger,input:&CommittedInputEvidenceDelta,
    material:&mut InputReadMaterial<'_>,limits:(usize,usize))->Result<Option<CommittedTaskDelta>> {
    material.with_task_material_limits(scope,ledger,Some(limits),|task|
        register_task_source_metadata_using_material_tx(tx,scope,input,ledger,task))
}
#[test]
fn supplemental_task_metadata_uses_input_remaining_and_its_own_new_material_cap_with_actual_last_holder()->Result<()> {
    let (_home,db)=fresh()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let input=db.transaction(|tx|seed(tx,&ledger))?;let actual_scope=scope();let baseline=ledger.report()?.observed_bytes;
    let full=input_policy(64*1024*1024,65536);let held_bytes=8*1024*1024;
    let mut connection=db.connection()?;let before_revision:u64=connection.query_row(
        "SELECT revision FROM task_domain_current WHERE agent_id='task-agent' AND identity_incarnation=1",[],|r|r.get(0))?;
    let measure=connection.transaction()?;let mut material=InputReadMaterial::new(&actual_scope,&full,&ledger,(0,0))?;
    material.observe_projected(u64::try_from(held_bytes)?,1)?;let held=vec![0u8;held_bytes];let prior=material.usage();
    let delta=read(&measure,&actual_scope,&ledger,&input,&mut material,(4*1024*1024,65536))?.context("actual source metadata absent")?;
    let added=(material.usage().0-prior.0,material.usage().1-prior.1);
    assert!(added.0>0 && added.0<4*1024*1024 && added.1>0);
    assert!(prior.0>4*1024*1024,"other-domain prior is not Task-owned allocation");
    assert!(delta.records.is_empty() && delta.stop_records.is_empty());assert_eq!(delta.revision,before_revision+1);
    assert_eq!(delta.coverage.output.heads.values().next().unwrap().source.as_ref().unwrap().message.key.id,"actual-shared-output-message");
    measure.rollback()?;drop((delta,material,held));assert_eq!(ledger.report()?.observed_bytes,baseline);
    // Independently constrain global Input bytes, Task-only bytes, and native nodes.
    for mode in 0..3 {
        let policy=input_policy(if mode==0 {prior.0+added.0-1} else {full.byte_limit},full.record_limit);
        let own=if mode==1 {(added.0-1,65536)} else if mode==2 {(4*1024*1024,added.1-1)} else {(4*1024*1024,65536)};
        let tx=connection.transaction()?;let mut material=InputReadMaterial::new(&actual_scope,&policy,&ledger,(0,0))?;
        material.observe_projected(u64::try_from(held_bytes)?,1)?;let held=vec![0u8;held_bytes];
        let error=read(&tx,&actual_scope,&ledger,&input,&mut material,own).err().context("supplemental Task received another full allowance")?;
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ResourceExhausted(_))),"mode{mode}: {error:#}");
        tx.rollback()?;drop((material,held));assert_eq!(ledger.report()?.observed_bytes,baseline);
        let revision:u64=connection.query_row("SELECT revision FROM task_domain_current WHERE agent_id='task-agent' AND identity_incarnation=1",[],|r|r.get(0))?;
        assert_eq!(revision,before_revision);
    }
    let tx=connection.transaction()?;let mut material=InputReadMaterial::new(&actual_scope,&full,&ledger,(0,0))?;
    material.observe_projected(u64::try_from(held_bytes)?,1)?;let held=vec![0u8;held_bytes];
    let delta=read(&tx,&actual_scope,&ledger,&input,&mut material,(4*1024*1024,65536))?.unwrap();tx.commit()?;
    let mut guards=material.into_guards();let last=guards.pop().context("supplemental Task omitted actual guard")?;let weak=Arc::downgrade(&last);
    drop((delta,guards,held));assert!(weak.upgrade().is_some());assert!(ledger.report()?.observed_bytes>baseline);
    drop(last);assert!(weak.upgrade().is_none());assert_eq!(ledger.report()?.observed_bytes,baseline);Ok(())
}

#[test]
fn supplemental_task_identity_raw_gate_precedes_malformed_decoder_without_reading_state_or_fixed_roots()->Result<()> {
    let (_home,db)=fresh()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let input=db.transaction(|tx|seed(tx,&ledger))?;let actual_scope=scope();let full=input_policy(64*1024*1024,65536);
    let baseline=ledger.report()?.observed_bytes;let mut connection=db.connection()?;
    let original:String=connection.query_row("SELECT payload_json FROM agent_identities WHERE agent_id='task-agent'",[],|r|r.get(0))?;
    let tx=connection.transaction()?;
    tx.execute("UPDATE agent_identities SET payload_json=?1 WHERE agent_id='task-agent'",[format!("!{}","i".repeat(96*1024))])?;
    let mut material=InputReadMaterial::new(&actual_scope,&full,&ledger,(0,0))?;
    let error=read(&tx,&actual_scope,&ledger,&input,&mut material,(512*1024,65536)).err().context("malformed identity decoded before original raw bound")?;
    assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ResourceExhausted(_))),"{error:#}");
    tx.rollback()?;drop(material);assert_eq!(ledger.report()?.observed_bytes,baseline);
    let restored:String=connection.query_row("SELECT payload_json FROM agent_identities WHERE agent_id='task-agent'",[],|r|r.get(0))?;assert_eq!(restored,original);
    let tx=connection.transaction()?;tx.execute("UPDATE agent_states SET payload_json='!' WHERE agent_id='task-agent'",[])?;
    // Original metadata-only maintenance needs identity and State existence, not its body.
    let mut material=InputReadMaterial::new(&actual_scope,&full,&ledger,(0,0))?;
    let delta=read(&tx,&actual_scope,&ledger,&input,&mut material,(4*1024*1024,65536))?.unwrap();
    assert!(delta.records.is_empty() && delta.stop_records.is_empty());
    tx.rollback()?;drop((delta,material));assert_eq!(ledger.report()?.observed_bytes,baseline);Ok(())
}

#[test]
fn supplemental_task_identity_complete_keys_are_admitted_before_its_typed_decoder_and_no_state_stays_empty()->Result<()> {
    let (_home,db)=fresh()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let input=db.transaction(|tx|seed(tx,&ledger))?;let actual_scope=scope();let full=input_policy(64*1024*1024,65536);
    let extension=serde_json::to_string(&serde_json::Value::Object((0..512).map(|n|(format!("actual-identity-key-{n}"),serde_json::Value::Null)).collect()))?;
    let mut connection=db.connection()?;let baseline=ledger.report()?.observed_bytes;let tx=connection.transaction()?;
    tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.ignored_extension',json(?1)) WHERE agent_id='task-agent'",[&extension])?;
    let (values,nodes):(usize,usize)=tx.query_row("SELECT COUNT(*),COUNT(*)+COUNT(CASE WHEN typeof(j.key)='text' THEN 1 END) FROM agent_identities i,json_tree(i.payload_json) j WHERE i.agent_id='task-agent'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    assert!(nodes>values+512);
    let mut probe=InputReadMaterial::new(&actual_scope,&full,&ledger,(0,0))?;let before=probe.usage();
    let delta=read(&tx,&actual_scope,&ledger,&input,&mut probe,(4*1024*1024,65536))?.unwrap();
    let actual=probe.usage().1-before.1;assert!(actual>=nodes*8);drop((delta,probe));tx.rollback()?;
    let tx=connection.transaction()?;tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.ignored_extension',json(?1),'$.status','invalid-identity') WHERE agent_id='task-agent'",[extension])?;
    // The typed identity decoder would now fail if the native gate were late.
    // Preserve enough nodes for the whole previous values-only path; only the
    // missing object-key allocation makes this real original phase reject.
    let limited=actual-(nodes-values)*8+1;
    let mut material=InputReadMaterial::new(&actual_scope,&full,&ledger,(0,0))?;
    let error=read(&tx,&actual_scope,&ledger,&input,&mut material,(4*1024*1024,limited)).err().context("identity object keys omitted")?;
    assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ResourceExhausted("records"))),"{error:#}");
    tx.rollback()?;drop(material);assert_eq!(ledger.report()?.observed_bytes,baseline);
    let tx=connection.transaction()?;tx.execute("DELETE FROM agent_states WHERE agent_id='task-agent'",[])?;
    let mut material=InputReadMaterial::new(&actual_scope,&full,&ledger,(0,0))?;
    assert!(read(&tx,&actual_scope,&ledger,&input,&mut material,(4*1024*1024,65536))?.is_none());
    tx.rollback()?;drop(material);assert_eq!(ledger.report()?.observed_bytes,baseline);Ok(())
}
