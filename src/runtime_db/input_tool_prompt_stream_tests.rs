use super::*;
use crate::{
    runtime::input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    runtime_db::{
        evidence::{insert_tool_evidence_tx,upsert_agent_identity_tx,upsert_agent_state_tx},RuntimeDb,
    },
    types::{AgentKind,AgentOwnership,AgentProfilePreset,AgentState,AgentVisibility,AuthorityClass,ToolExecutionStatus},
};
use std::sync::Arc;
fn scope()->InputEvidenceScope {InputEvidenceScope{agent_id:"stream-prompt-owner".into(),identity_incarnation:1}}
fn request()->InputEvidenceLoadRequest {InputEvidenceLoadRequest{windows:Vec::new(),mandatory_roots:Default::default(),interrupted_sources:Default::default(),selected_completion_brief_ids:Default::default(),byte_limit:MAX_METADATA_BYTES,record_limit:MAX_RECORDS}}
fn tool(name:&str)->ToolExecutionRecord {ToolExecutionRecord{
    id:"stream-prompt-tool".into(),agent_id:scope().agent_id,work_item_id:None,turn_index:0,turn_id:None,
    tool_name:name.into(),created_at:Utc::now(),completed_at:None,duration_ms:0,authority_class:AuthorityClass::RuntimeInstruction,
    status:ToolExecutionStatus::Success,input:serde_json::Value::Null,output:serde_json::Value::Null,
    summary:"actual original prompt".into(),invocation_surface:None,
}}
fn db()->Result<(tempfile::TempDir,RuntimeDb)> {
    let home=tempfile::tempdir()?;
    let db=RuntimeDb::open_and_migrate(home.path().join("runtime.sqlite"),home.path().join("runtime.lock"))?;
    db.transaction(|tx|{
        upsert_agent_identity_tx(tx,&AgentIdentityRecord::new(&scope().agent_id,AgentKind::Named,AgentVisibility::Public,AgentOwnership::SelfOwned,AgentProfilePreset::PublicNamed,None,None))?;
        upsert_agent_state_tx(tx,&AgentState::new(&scope().agent_id))
    })?;Ok((home,db))
}
fn write(db:&RuntimeDb,original:&ToolExecutionRecord)->Result<()> {
    db.transaction(|tx|{
        insert_tool_evidence_tx(tx,original)?;
        register_committed_input_evidence_tx(tx,&scope(),&[CanonicalInputEvidence::ToolExecution(original.clone())])?;
        Ok(())
    })
}
// Both the old SQL reader and stream use the actual normalized column clock.
fn stored_prompt(db: &RuntimeDb, original: &ToolExecutionRecord) -> Result<ToolContextProjection> {
    let mut expected = project_tool_record(original)?;
    let normalized: String = db.connection()?.query_row(
        "SELECT created_at FROM tool_executions WHERE agent_id=?1 AND evidence_id=?2",
        params![original.agent_id,original.id], |row|row.get(0),
    )?;
    assert_eq!(normalized, original.created_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    expected.created_at = normalized.parse()?;
    Ok(expected)
}
fn read(db:&RuntimeDb,policy:&InputEvidenceLoadRequest,ledger:&InputEvidenceBudgetLedger)->Result<(InputEvidenceCurrentSnapshot,Vec<Arc<BudgetViewGuard>>)> {
    db.transaction(|tx|{
        let actual_scope=scope();
        let mut material=read_material::InputReadMaterial::new(&actual_scope,policy,ledger,(0,0))?;
        let current=match prepare_input_evidence_snapshot_with_material_tx(tx,&actual_scope,policy,false,&mut material)? {
            InputEvidenceBootstrapSnapshot::Current(current)=>current,
            _=>anyhow::bail!("actual prompt scope unexpectedly retired"),
        };
        Ok((current,material.into_guards()))
    })
}
#[test]
fn actual_selected_prompt_stream_matches_original_canonical_command_and_batch_projection() -> Result<()> {
    let (_home,db)=db()?;
    let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let mut original=tool("ExecCommand");
    let mut policy=request();policy.mandatory_roots.insert(Key::new(Family::ToolExecution,&original.id));
    for mode in 0..3 {
        let batch=mode==1;
        original.tool_name=if batch {"ExecCommandBatch"} else {"ExecCommand"}.into();
        original.input=serde_json::json!({"cmd":"curl -H 'Authorization: secret' https://example.invalid", "items":[{},{},{},{},{}]});
        original.output=serde_json::json!({"result":{
            "disposition":"promoted_to_task","task_handle":{"task_id":"original-task"},"exit_status":0,
            "stdout_preview":null,"stdout_truncated":1,"output_truncated":"1suffix","stderr_artifact":{"private":"must not own artifact body"},
            "truncated":true,"artifacts":[{"body":"must not own artifact"}],
            "items":[{"result":{"exit_status":0}},{"exit_code":7},{"result":{"disposition":"already_running"}},{},
                {"result":{"exit_status":0}},{"exit_status":"outside original input count must not decode"}]},
            "envelope":{"result":{"exit_status":9}},"stdout":"x".repeat(10*1024*1024)});
        if mode==2 { original.output["result"]=serde_json::Value::Null; }
        write(&db,&original)?;
        let expected=stored_prompt(&db,&original)?;
        let (current,guards)=read(&db,&policy,&ledger)?;
        let capture=current.resident.capture()?;
        let actual=capture.lookup(&Key::new(Family::ToolExecution,&original.id))?.context("selected Tool absent")?;
        assert_eq!(actual.record,PreparedInputEvidence::ToolExecution(expected));
        assert!(actual.resident_bytes <32*1024);
        assert!(actual.tool_run_report.is_none(),"prompt role does not silently grant report completeness");
        assert!(ledger.report()?.observed_bytes <2*1024*1024,"original blob scratch must retire before snapshot handoff");
        let last=Arc::downgrade(guards.last().context("actual retained prompt guard missing")?);
        let clone=guards.clone();
        drop((guards,capture,current));assert!(last.upgrade().is_some());drop(clone);assert!(last.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes,0);
    }
    Ok(())
}
#[test]
fn actual_prompt_stream_keeps_unicode_nul_summary_and_oversized_command_contract() -> Result<()> {
    let (_home,db)=db()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let mut original=tool("ExecCommand");let mut policy=request();policy.mandatory_roots.insert(Key::new(Family::ToolExecution,&original.id));
    for summary in ["中🙂".repeat(5000),"原始\0SQL TEXT suffix".into()] {
        original.summary=summary;
        original.input=serde_json::json!({"cmd":"q".repeat(MAX_COMMAND_PREVIEW_INPUT_BYTES+1)});
        original.output=serde_json::json!({"envelope":{"result":{"exit_code":0,"initial_output_preview":null,"output_truncated":"1suffix"}}});
        write(&db,&original)?;
        let expected=stored_prompt(&db,&original)?;
        let (current,guards)=read(&db,&policy,&ledger)?;
        let captured=current.resident.capture()?;
        let actual=captured.lookup(&Key::new(Family::ToolExecution,&original.id))?.context("actual prompt absent")?;
        assert_eq!(actual.record,PreparedInputEvidence::ToolExecution(expected));
        assert!(actual.resident_bytes <32*1024);
        drop((captured,current,guards));assert_eq!(ledger.report()?.observed_bytes,0);
    }
    Ok(())
}
#[test]
fn actual_prompt_metadata_guard_rejects_large_normalized_column_before_owned_header() -> Result<()> {
    let (_home,db)=db()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let original=tool("ExecCommand");write(&db,&original)?;
    let mut policy=request();policy.mandatory_roots.insert(Key::new(Family::ToolExecution,&original.id));
    let before:(u64,String)=db.connection()?.query_row("SELECT revision,content_digest FROM input_evidence_current WHERE family='tool_execution' AND record_id=?1",[&original.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    db.connection()?.execute("UPDATE tool_executions SET work_item_id=?1 WHERE evidence_id=?2",params!["x".repeat(9*1024*1024),original.id])?;
    let error=read(&db,&policy,&ledger).err().context("actual normalized string must be gated before owned row")?;
    assert!(matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::ResourceExhausted(_))),"{error:#}");
    assert_eq!(ledger.report()?.observed_bytes,0);
    assert_eq!(db.connection()?.query_row("SELECT revision,content_digest FROM input_evidence_current WHERE family='tool_execution' AND record_id=?1",[&original.id],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?)))?,before);
    db.connection()?.execute("UPDATE tool_executions SET work_item_id=NULL WHERE evidence_id=?1",[&original.id])?;
    drop(read(&db,&policy,&ledger)?);assert_eq!(ledger.report()?.observed_bytes,0);
    // A small raw normalized relationship drift cannot become an owning edge,
    // even when the old body hash and exact alias are unchanged.
    db.connection()?.execute("UPDATE tool_executions SET turn_id='raw-unaccepted-turn-edge' WHERE evidence_id=?1",[&original.id])?;
    let error=read(&db,&policy,&ledger).err().context("normalized edge must match actual original body")?;
    assert!(matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id==&original.id),"{error:#}");
    assert_eq!(ledger.report()?.observed_bytes,0);
    assert_eq!(db.connection()?.query_row("SELECT revision,content_digest FROM input_evidence_current WHERE family='tool_execution' AND record_id=?1",[&original.id],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?)))?,before);
    Ok(())
}
#[test]
fn actual_twelve_prompt_sources_do_not_keep_ten_megabyte_source_scratch_in_selected_pool() -> Result<()> {
    let (_home,db)=db()?;let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let mut policy=request();
    for index in 0..12 {
        let mut original=tool("ExecCommand");original.id=format!("stream-prompt-{index}");
        original.output=serde_json::json!({"stdout":"x".repeat(10*1024*1024)});
        write(&db,&original)?;policy.mandatory_roots.insert(Key::new(Family::ToolExecution,&original.id));
    }
    let (current,guards)=read(&db,&policy,&ledger)?;
    let capture=current.resident.capture()?;
    for key in &policy.mandatory_roots {assert!(capture.lookup(key)?.is_some());}
    assert!(ledger.report()?.observed_bytes <4*1024*1024,"sequential scratch is not retained native/body material");
    drop((capture,current,guards));assert_eq!(ledger.report()?.observed_bytes,0);
    Ok(())
}
