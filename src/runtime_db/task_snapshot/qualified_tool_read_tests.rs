use super::*;
use chrono::Utc;
use super::super::tests::{fresh, legacy, scope};
use crate::{
    runtime_db::{evidence, input_evidence_snapshot::{
        self, CanonicalInputEvidence, InputEvidenceSnapshotError,
    }},
    runtime::{
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey, PreparedInputEvidence, QualifiedInputEvidence},
        input_evidence_budget::{BudgetViewGuard, InputMemoryCategory},
    },
    types::{AuthorityClass, ToolExecutionRecord, ToolExecutionStatus},
};

fn policy(bytes: usize) -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        query: Default::default(), capture: TaskCaptureRequest::current(),
        record_limit: 65_536, byte_limit: bytes,
    }
}
fn original_tool() -> ToolExecutionRecord {
    ToolExecutionRecord {
        id: "readonly-task-tool".into(), agent_id: scope().agent_id, work_item_id: None,
        turn_index: 0, turn_id: None, tool_name: "ExecCommand".into(), created_at: Utc::now(),
        completed_at: None, duration_ms: 0, authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success, input: serde_json::json!({"cmd":"printf original"}),
        output: serde_json::json!({"stdout":"x".repeat(10 * 1024 * 1024)}),
        summary: "original readonly task output".into(), invocation_surface: None,
    }
}
fn selected_tool(
    tx: &Transaction<'_>, ledger: &InputEvidenceBudgetLedger, load: &TaskDomainLoadRequest,
) -> Result<(Arc<QualifiedInputEvidence>, Vec<Arc<BudgetViewGuard>>, usize)> {
    let scope = scope();
    let current = prepare_task_domain_snapshot_with_budget_tx(tx, &scope, load, ledger)?.require_current()?;
    let task = TaskKey::new(TaskFamily::Task, "genuinely-absent-task-for-readonly-input");
    let mut read = prepare_task_input_read_base_tx(tx, &scope, current.facts.state.as_ref().unwrap(),
        current.resident.coverage().revision, &task, None, None, load, ledger)?;
    let before = read.material.material_estimate();
    let key = InputEvidenceKey::new(InputEvidenceFamily::ToolExecution, "readonly-task-tool");
    let actual = input_evidence_snapshot::read_authorized_task_input_tx(tx, &mut read, &key, false)?
        .context("qualified original Tool disappeared")?;
    let after = read.material.material_estimate();
    assert!(after.bytes > before.bytes && after.records > before.records);
    assert!(after.bytes < 4 * 1024 * 1024, "10MiB stdout scratch must retire before Task retains the finite prompt");
    Ok((actual, read.material.into_guards(), after.bytes))
}

#[test]
fn task_input_stream_uses_original_allowance_and_retains_only_finite_actual_guards() -> Result<()> {
    let (_home, db) = fresh()?;
    let original = original_tool();
    db.transaction(|tx| {
        evidence::insert_tool_evidence_tx(tx, &original)?;
        input_evidence_snapshot::register_committed_input_evidence_tx(tx, &scope(),
            &[CanonicalInputEvidence::ToolExecution(original.clone())])?;
        Ok(())
    })?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let aliases = db.connection()?.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r|r.get::<_,usize>(0))?;
    let error = db.transaction(|tx| selected_tool(tx, &ledger, &policy(16 * 1024 * 1024)))
        .err().context("raw Source fits but actual Serde scratch must not get another cap")?;
    assert!(matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ResourceExhausted(_))), "{error:#}");
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let (actual, guards, used) = db.transaction(|tx| selected_tool(tx, &ledger, &policy(64 * 1024 * 1024)))?;
    let PreparedInputEvidence::ToolExecution(prompt) = &actual.record else { unreachable!() };
    assert_eq!(prompt.command.command_preview.as_deref(), Some("printf original"));
    assert_eq!(prompt.summary, original.summary);
    assert!(serde_json::to_vec(actual.as_ref())?.len() < 32 * 1024);
    assert!(used < 4 * 1024 * 1024);
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r|r.get::<_,usize>(0))?, aliases);
    let weak = Arc::downgrade(&actual);
    let pins = ledger.register_view(&scope(), InputMemoryCategory::Pins, std::slice::from_ref(&actual))?;
    let selected = actual.clone();
    drop(actual);
    assert!(weak.upgrade().is_some());
    drop(selected);
    assert!(weak.upgrade().is_none());
    assert!(ledger.report()?.observed_bytes > 0);
    drop((guards, pins));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn task_input_stream_keeps_fixed_without_alias_and_rejects_same_length_body_drift() -> Result<()> {
    let original = original_tool();
    let (_home, db) = legacy(true, |tx| evidence::insert_tool_evidence_tx(tx, &original))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let before = db.connection()?.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE family='tool_execution'", [], |r|r.get::<_,usize>(0))?;
    assert_eq!(before, 0);
    drop(db.transaction(|tx| selected_tool(tx, &ledger, &policy(64 * 1024 * 1024)))?);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let raw: String = db.connection()?.query_row("SELECT payload_json FROM tool_executions WHERE evidence_id=?1", [&original.id], |r|r.get(0))?;
    let changed = raw.replacen("printf original", "printf tampered", 1);
    assert_eq!(raw.len(), changed.len());
    assert_ne!(raw, changed);
    db.connection()?.execute("UPDATE tool_executions SET payload_json=?1 WHERE evidence_id=?2", params![changed,original.id])?;
    let error = db.transaction(|tx| selected_tool(tx, &ledger, &policy(64 * 1024 * 1024)))
        .err().context("normalized hash cannot authorize changed raw Tool bytes")?;
    assert!(matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == &original.id), "{error:#}");
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM input_evidence_current WHERE family='tool_execution'", [], |r|r.get::<_,usize>(0))?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn task_input_stream_does_not_decode_foreign_poisoned_body_or_change_original_membership() -> Result<()> {
    let (_home, db) = fresh()?;
    let original = original_tool();
    db.transaction(|tx| {
        evidence::insert_tool_evidence_tx(tx, &original)?;
        input_evidence_snapshot::register_committed_input_evidence_tx(tx, &scope(),
            &[CanonicalInputEvidence::ToolExecution(original.clone())])?;
        Ok(())
    })?;
    db.connection()?.execute("UPDATE tool_executions SET agent_id='foreign-readonly-tool',payload_json=?1 WHERE evidence_id=?2",
        params!["[".to_owned()+&"x".repeat(9 * 1024 * 1024), original.id])?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let aliases = db.connection()?.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r|r.get::<_,usize>(0))?;
    let error = db.transaction(|tx| selected_tool(tx, &ledger, &policy(64 * 1024 * 1024)))
        .err().context("foreign Tool cannot borrow the original scope")?;
    assert!(matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::NeedsPreparation(id)) if id == &original.id), "{error:#}");
    assert_eq!(db.connection()?.query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r|r.get::<_,usize>(0))?, aliases);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
