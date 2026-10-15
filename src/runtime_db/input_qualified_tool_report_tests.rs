use super::*;
use crate::{
    runtime::{
        input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory},
        resident_input_evidence::{InputEvidenceReadError, QualifiedInputEvidence},
    },
    runtime_db::{
        evidence::{insert_tool_evidence_tx, upsert_agent_identity_tx, upsert_agent_state_tx},
        RuntimeDb,
    },
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, ToolExecutionRecord, ToolExecutionStatus,
    },
};
use std::sync::Arc;
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "qualified-tool-report-owner".into(),
        identity_incarnation: 1,
    }
}
fn request() -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: MAX_METADATA_BYTES,
        record_limit: MAX_RECORDS,
    }
}
fn tool(name: &str) -> ToolExecutionRecord {
    ToolExecutionRecord {
        id: "qualified-report-tool".into(),
        agent_id: scope().agent_id,
        work_item_id: None,
        turn_index: 0,
        turn_id: None,
        tool_name: name.into(),
        created_at: Utc::now(),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::Value::Null,
        output: serde_json::Value::Null,
        summary: "original tool prompt".into(),
        invocation_surface: None,
    }
}
fn source(
    original: ToolExecutionRecord,
) -> Result<(tempfile::TempDir, RuntimeDb, QualifiedInputEvidence)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let actual = db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                &scope().agent_id,
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        upsert_agent_state_tx(tx, &AgentState::new(&scope().agent_id))?;
        insert_tool_evidence_tx(tx, &original)?;
        let mut delta = register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::ToolExecution(original.clone())],
        )?;
        assert_eq!(delta.records.len(), 1);
        Arc::try_unwrap(delta.records.pop().unwrap())
            .map_err(|_| anyhow::anyhow!("actual writer unexpectedly retained Tool allocation"))
    })?;
    assert!(actual.tool_run_report.is_none());
    Ok((home, db, actual))
}
fn hydrate(
    db: &RuntimeDb,
    record: &mut QualifiedInputEvidence,
    policy: &InputEvidenceLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Vec<Arc<BudgetViewGuard>>> {
    let (report, guards) = db.transaction(|tx| {
        let actual_scope = scope();
        let mut material =
            read_material::InputReadMaterial::new(&actual_scope, policy, ledger, (0, 0))?;
        let report = prepare_qualified_tool_run_report_on(tx, record, &mut material)?;
        Ok((report, material.into_guards()))
    })?;
    record.tool_run_report = Some(report);
    Ok(guards)
}
#[test]
fn qualified_report_large_paths_preserve_small_prompt_and_complete_actual_native_floor(
) -> Result<()> {
    let mut original = tool("ApplyPatch");
    let path = "large-original-path".repeat(2048);
    original.input = serde_json::json!({"patch":format!("--- /dev/null\n+++ b/{path}\n")});
    original.output =
        serde_json::json!({"envelope":{"result":{"changed_paths":["wrong-fallback"]}}});
    let (_home, db, mut actual) = source(original)?;
    assert!(matches!(
        actual
            .tool_run_report()
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>(),
        Some(InputEvidenceReadError::NeedsPreparation(_))
    ));
    let prompt_bytes = actual.resident_bytes;
    assert!(prompt_bytes < 32 * 1024);
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let guards = hydrate(&db, &mut actual, &request(), &ledger)?;
    assert_eq!(actual.resident_bytes, prompt_bytes);
    assert_eq!(actual.tool_run_report()?.patch_paths, vec![path]);
    assert!(actual.tool_run_report()?.output_paths.is_empty());
    assert!(actual.resident_material_estimate()?.0 > 32 * 1024);
    let mut old = serde_json::to_value(&actual)?;
    old.as_object_mut().unwrap().remove("tool_run_report");
    let old: QualifiedInputEvidence = serde_json::from_value(old)?;
    assert!(matches!(
        old.tool_run_report()
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>(),
        Some(InputEvidenceReadError::NeedsPreparation(_))
    ));
    let mut changed = actual.clone();
    changed.locator.revision += 1;
    assert!(matches!(
        changed
            .tool_run_report()
            .unwrap_err()
            .downcast_ref::<InputEvidenceReadError>(),
        Some(InputEvidenceReadError::NeedsPreparation(_))
    ));
    let mut changed_scope = actual.clone();
    changed_scope.locator.scope.identity_incarnation += 1;
    assert!(changed_scope.tool_run_report().is_err());
    drop((guards, actual, old, changed, changed_scope));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn qualified_report_single_pass_keeps_ten_megabyte_stdout_out_and_pure_pins_until_last_drop(
) -> Result<()> {
    let mut original = tool("ExecCommand");
    original.input = serde_json::json!({"cmd":"printf original"});
    original.output = serde_json::json!({"stdout":"x".repeat(10 * 1024 * 1024)});
    let (_home, db, mut actual) = source(original)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let guards = hydrate(&db, &mut actual, &request(), &ledger)?;
    assert_eq!(
        actual.tool_run_report()?,
        &ToolRunReportProjection::default()
    );
    assert!(serde_json::to_vec(&actual)?.len() < 32 * 1024);
    let actual = Arc::new(actual);
    let view = ledger.register_view(
        &scope(),
        InputMemoryCategory::Pins,
        std::slice::from_ref(&actual),
    )?;
    let floor = u64::try_from(actual.resident_material_estimate()?.0)?;
    assert!(ledger.report()?.physical_records.bytes >= floor);
    let weak = Arc::downgrade(&actual);
    let selected = actual.clone();
    db.connection()?
        .execute_batch("ALTER TABLE tool_executions RENAME TO unavailable_original_tool_body")?;
    assert_eq!(
        selected.tool_run_report()?,
        &ToolRunReportProjection::default()
    );
    db.connection()?
        .execute_batch("ALTER TABLE unavailable_original_tool_body RENAME TO tool_executions")?;
    let retained = guards.clone();
    drop((guards, actual));
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(selected);
    assert!(
        weak.upgrade().is_none(),
        "material guard may not retain its source body"
    );
    drop((view, retained));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn qualified_report_same_length_raw_tamper_cannot_borrow_the_original_hash_or_alias() -> Result<()>
{
    let mut original = tool("ApplyPatch");
    original.input = serde_json::json!({"patch":"--- a/old.rs\n+++ b/old.rs\n"});
    let (_home, db, mut actual) = source(original)?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let before:(String,String,usize)=db.connection()?.query_row(
        "SELECT payload_json,content_hash,length(CAST(payload_json AS BLOB)) FROM tool_executions WHERE evidence_id=?1",
        [&actual.locator.key.id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    )?;
    let alias:(u64,String)=db.connection()?.query_row(
        "SELECT revision,content_digest FROM input_evidence_current WHERE family='tool_execution' AND record_id=?1",
        [&actual.locator.key.id], |r|Ok((r.get(0)?,r.get(1)?)),
    )?;
    let tampered = before.0.replace("old.rs", "new.rs");
    assert_eq!(tampered.len(), before.2);
    db.connection()?.execute(
        "UPDATE tool_executions SET payload_json=?1 WHERE evidence_id=?2",
        params![tampered, actual.locator.key.id],
    )?;
    let error = hydrate(&db, &mut actual, &request(), &ledger)
        .err()
        .context("same-length raw source must reject")?;
    assert!(
        matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),
        Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == &actual.locator.key.id),
        "{error:#}"
    );
    assert!(actual.tool_run_report.is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(db.connection()?.query_row("SELECT content_hash,length(CAST(payload_json AS BLOB)) FROM tool_executions WHERE evidence_id=?1",
        [&actual.locator.key.id], |r|Ok((r.get::<_,String>(0)?,r.get::<_,usize>(1)?)))?, (before.1, before.2));
    assert_eq!(db.connection()?.query_row("SELECT revision,content_digest FROM input_evidence_current WHERE family='tool_execution' AND record_id=?1",
        [&actual.locator.key.id], |r|Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?)))?,alias);
    db.connection()?.execute(
        "UPDATE tool_executions SET payload_json=?1 WHERE evidence_id=?2",
        params![before.0, actual.locator.key.id],
    )?;
    drop(hydrate(&db, &mut actual, &request(), &ledger)?);
    assert_eq!(actual.tool_run_report()?.patch_paths, vec!["old.rs"]);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn qualified_report_original_material_scope_and_pruned_alias_remain_strict() -> Result<()> {
    let (_home, db, mut actual) = source(tool("Sleep"))?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut limited = request();
    limited.byte_limit = 256 * 1024;
    let error = hydrate(&db, &mut actual, &limited, &ledger)
        .err()
        .context("hash scratch must be admitted before allocation")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let original_raw: String = db.connection()?.query_row(
        "SELECT payload_json FROM tool_executions WHERE evidence_id=?1",
        [&actual.locator.key.id],
        |r| r.get(0),
    )?;
    db.connection()?.execute("UPDATE tool_executions SET agent_id='foreign-tool-owner',payload_json=?1 WHERE evidence_id=?2",
        params!["[".to_owned()+&"x".repeat(9 * 1024 * 1024),actual.locator.key.id])?;
    let error = hydrate(&db, &mut actual, &request(), &ledger)
        .err()
        .context("foreign body may not become a report")?;
    assert!(
        matches!(error.downcast_ref::<InputEvidenceSnapshotError>(), Some(InputEvidenceSnapshotError::NeedsPreparation(id)) if id == &actual.locator.key.id),
        "{error:#}"
    );
    assert!(actual.tool_run_report.is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.connection()?.execute(
        "UPDATE tool_executions SET agent_id=?1,payload_json=?2 WHERE evidence_id=?3",
        params![scope().agent_id, original_raw, actual.locator.key.id],
    )?;
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM tool_executions WHERE evidence_id=?1",
            [&actual.locator.key.id],
        )?;
        register_pruned_input_evidence_tx(tx, &scope(), std::slice::from_ref(&actual.locator.key))?;
        Ok(())
    })?;
    let error = hydrate(&db, &mut actual, &request(), &ledger)
        .err()
        .context("Pruned body cannot become a report")?;
    assert!(
        matches!(error.downcast_ref::<InputEvidenceSnapshotError>(), Some(InputEvidenceSnapshotError::ProjectionChanged(id)) if id == &actual.locator.key.id),
        "{error:#}"
    );
    assert!(actual.tool_run_report.is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(db.connection()?.query_row("SELECT availability FROM input_evidence_current WHERE family='tool_execution' AND record_id=?1",
        [&actual.locator.key.id], |r|r.get::<_,String>(0))?, "pruned");
    Ok(())
}

#[test]
fn qualified_report_no_state_and_retired_poisoned_successor_never_create_ready_report() -> Result<()>
{
    for retired in [false, true] {
        let (_home, db, mut actual) = source(tool("ExecCommand"))?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        if retired {
            db.connection()?.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1", [&scope().agent_id])?;
            db.connection()?.execute(
                "UPDATE agent_states SET payload_json=?1 WHERE agent_id=?2",
                params![
                    "[".to_owned() + &"x".repeat(9 * 1024 * 1024),
                    scope().agent_id
                ],
            )?;
        } else {
            db.connection()?.execute(
                "DELETE FROM agent_states WHERE agent_id=?1",
                [&scope().agent_id],
            )?;
        }
        let mut policy = request();
        policy.byte_limit = 1024 * 1024;
        let error = hydrate(&db, &mut actual, &policy, &ledger)
            .err()
            .context("unavailable original scope may not prepare Tool report")?;
        if retired {
            assert!(
                matches!(
                    error.downcast_ref::<InputEvidenceSnapshotError>(),
                    Some(InputEvidenceSnapshotError::RetiredScope)
                ),
                "{error:#}"
            );
        } else {
            assert!(
                matches!(error.downcast_ref::<InputEvidenceSnapshotError>(), Some(InputEvidenceSnapshotError::NeedsPreparation(message)) if message == "Tool report owner has no State"),
                "{error:#}"
            );
        }
        assert!(actual.tool_run_report.is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn actual_tool_writer_and_named_hydration_share_normalized_view_clock_without_rewriting_raw(
) -> Result<()> {
    use crate::runtime::resident_input_evidence::{
        CommittedInputEvidenceDelta, InputEvidenceCoverage, ResidentInputEvidence,
    };
    let mut original = tool("ExecCommand");
    original.created_at = "2026-10-14T01:02:03.123456789Z".parse()?;
    original.input = serde_json::json!({"cmd":"printf original"});
    let (_home, db, mut writer) = source(original.clone())?;
    let raw_before: (String, String, String) = db.connection()?.query_row(
        "SELECT payload_json,content_hash,created_at FROM tool_executions WHERE evidence_id=?1",
        [&original.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(raw_before.2, "2026-10-14T01:02:03.123Z");
    let raw: ToolExecutionRecord = serde_json::from_str(&raw_before.0)?;
    assert_eq!(raw.created_at, original.created_at);
    let PreparedInputEvidence::ToolExecution(writer_prompt) = &writer.record else {
        unreachable!()
    };
    assert_eq!(
        writer_prompt.created_at,
        raw_before.2.parse::<chrono::DateTime<Utc>>()?
    );
    assert_ne!(writer_prompt.created_at, original.created_at);
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let guards = hydrate(&db, &mut writer, &request(), &ledger)?;
    let mut policy = request();
    policy.mandatory_roots.insert(writer.locator.key.clone());
    let (mut hydrated, phase_guards) = db.transaction(|tx| {
        let actual_scope = scope();
        let mut phase =
            read_material::InputReadMaterial::new(&actual_scope, &policy, &ledger, (0, 0))?;
        let current = match prepare_input_evidence_snapshot_with_material_tx(
            tx,
            &actual_scope,
            &policy,
            false,
            &mut phase,
        )? {
            InputEvidenceBootstrapSnapshot::Current(current) => current,
            _ => anyhow::bail!("actual named source retired"),
        };
        let selected = current.resident.capture()?;
        let actual = selected
            .lookup(&writer.locator.key)?
            .context("actual named Tool missing")?
            .as_ref()
            .clone();
        Ok((actual, phase.into_guards()))
    })?;
    let hydrated_guards = hydrate(&db, &mut hydrated, &request(), &ledger)?;
    assert_eq!(
        writer, hydrated,
        "full qualified canonical fields and both actual seals must agree at the same locator"
    );
    let mut old = writer.clone();
    let PreparedInputEvidence::ToolExecution(old_prompt) = &mut old.record else {
        unreachable!()
    };
    old_prompt.created_at = original.created_at;
    let old: QualifiedInputEvidence = serde_json::from_value(serde_json::to_value(&old)?)?;
    assert!(matches!(old.tool_run_report().unwrap_err().downcast_ref::<InputEvidenceReadError>(),
        Some(InputEvidenceReadError::NeedsPreparation(key)) if key==&writer.locator.key),
        "old serialized projection must remain incomplete until original named preparation normalizes it");
    let key = writer.locator.key.clone();
    let writer = Arc::new(writer);
    let hydrated = Arc::new(hydrated);
    let coverage = InputEvidenceCoverage {
        query: Default::default(),
        selected_completion_brief_ids: Default::default(),
        completion_source_observations: Vec::new(),
        recovery_source_absences: Default::default(),
        recovery_lineage_messages: Default::default(),
        execution: Default::default(),
        windows: Vec::new(),
        mandatory_roots: std::collections::BTreeSet::from([key.clone()]),
        turn_transcript_keys: Default::default(),
        message_turn_keys: Default::default(),
        unavailable: Default::default(),
        record_limit: MAX_RECORDS,
        byte_limit: MAX_METADATA_BYTES,
    };
    let domain = ResidentInputEvidence::bootstrap(scope(), vec![writer.clone()], coverage)?
        .with_budget(ledger.clone())?;
    let merged = domain
        .with_committed(
            &CommittedInputEvidenceDelta {
                records: vec![hydrated.clone()],
                ..Default::default()
            },
            std::collections::BTreeSet::from([key.clone()]),
        )?
        .context("strict same-revision full comparison must succeed")?;
    let selected = merged.capture()?;
    assert!(Arc::ptr_eq(
        selected.lookup(&key)?.context("same revision lost Tool")?,
        &writer
    ));
    let raw_after: (String, String, String) = db.connection()?.query_row(
        "SELECT payload_json,content_hash,created_at FROM tool_executions WHERE evidence_id=?1",
        [&original.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        raw_after, raw_before,
        "projection normalization cannot rewrite raw source or its fingerprint"
    );
    drop((
        selected,
        merged,
        domain,
        writer,
        hydrated,
        old,
        guards,
        hydrated_guards,
        phase_guards,
    ));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

use crate::types::TurnOwner;

fn report_domain(
    record: Arc<QualifiedInputEvidence>,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<crate::runtime::resident_input_evidence::ResidentInputEvidence> {
    use crate::runtime::resident_input_evidence::{InputEvidenceCoverage, ResidentInputEvidence};
    ResidentInputEvidence::bootstrap(
        scope(),
        vec![record.clone()],
        InputEvidenceCoverage {
            query: Default::default(),
            selected_completion_brief_ids: Default::default(),
            completion_source_observations: Vec::new(),
            recovery_source_absences: Default::default(),
            recovery_lineage_messages: Default::default(),
            execution: Default::default(),
            windows: Vec::new(),
            mandatory_roots: std::collections::BTreeSet::from([record.locator.key.clone()]),
            turn_transcript_keys: Default::default(),
            message_turn_keys: Default::default(),
            unavailable: Default::default(),
            record_limit: MAX_RECORDS,
            byte_limit: MAX_METADATA_BYTES,
        },
    )?
    .with_budget(ledger.clone())
}

#[test]
fn qualified_report_original_ack_and_work_sources_reuse_sealed_arc_without_body_reads() -> Result<()>
{
    use crate::runtime::resident_input_evidence::CommittedInputEvidenceDelta;
    let mut original = tool("ApplyPatch");
    original.input = serde_json::json!({"patch":"--- a/original.rs\n+++ b/original.rs\n"});
    original.output = serde_json::json!({"stdout":"x".repeat(10 * 1024 * 1024)});
    let (_home, db, mut actual) = source(original)?;
    let incoming = Arc::new(actual.clone());
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let guards = hydrate(&db, &mut actual, &request(), &ledger)?;
    let actual = Arc::new(actual);
    let actual_weak = Arc::downgrade(&actual);
    let incomplete_weak = Arc::downgrade(&incoming);
    let current = report_domain(actual.clone(), &ledger)?;
    let prepared_source = report_domain(incoming.clone(), &ledger)?;
    let roots = std::collections::BTreeSet::from([actual.locator.key.clone()]);
    db.connection()?
        .execute_batch("ALTER TABLE tool_executions RENAME TO forbidden_tool_body_reads")?;
    let acknowledged = current
        .with_committed(
            &CommittedInputEvidenceDelta {
                records: vec![incoming.clone()],
                ..Default::default()
            },
            roots.clone(),
        )?
        .context("same actual revision must retain its sealed report")?;
    let prepared = acknowledged
        .with_committed_work_sources(
            &CommittedInputEvidenceDelta::default(),
            roots,
            Some(&prepared_source),
        )?
        .context("prepared owning sources must retain the original sealed allocation")?;
    let selected = prepared.capture()?;
    let selected_record = selected
        .lookup(&actual.locator.key)?
        .context("actual Tool must remain selected")?;
    assert!(Arc::ptr_eq(selected_record, &actual));
    assert_eq!(
        selected_record.tool_run_report()?.patch_paths,
        vec!["original.rs"]
    );
    assert!(selected_record.tool_run_report()?.output_paths.is_empty());
    drop((
        current,
        acknowledged,
        prepared,
        prepared_source,
        actual,
        incoming,
    ));
    assert!(
        incomplete_weak.upgrade().is_none(),
        "the unsealed incoming body must not replace the original Arc"
    );
    assert!(actual_weak.upgrade().is_some());
    assert!(ledger.report()?.physical_records.bytes > 0);
    drop(selected);
    assert!(
        actual_weak.upgrade().is_none(),
        "retained budget guards must not own a source body"
    );
    assert!(
        ledger.report()?.observed_bytes > 0,
        "actual hydration guards still retain their material"
    );
    drop(guards);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn qualified_report_actual_hydration_enriches_once_and_equal_seals_keep_original_arc() -> Result<()>
{
    use crate::runtime::resident_input_evidence::CommittedInputEvidenceDelta;
    let (_home, db, mut actual) = source(tool("ExecCommand"))?;
    let plain = Arc::new(actual.clone());
    let mut second = actual.clone();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let guards = hydrate(&db, &mut actual, &request(), &ledger)?;
    let second_guards = hydrate(&db, &mut second, &request(), &ledger)?;
    let actual = Arc::new(actual);
    let second = Arc::new(second);
    let current = report_domain(plain, &ledger)?;
    let roots = std::collections::BTreeSet::from([actual.locator.key.clone()]);
    let enriched = current
        .with_committed(
            &CommittedInputEvidenceDelta {
                records: vec![actual.clone()],
                ..Default::default()
            },
            roots.clone(),
        )?
        .context("actual named hydration should enrich the original locator")?;
    let prepared = report_domain(second.clone(), &ledger)?;
    let repeated = enriched
        .with_committed_work_sources(
            &CommittedInputEvidenceDelta::default(),
            roots,
            Some(&prepared),
        )?
        .context("independent actual reads of the same source should agree")?;
    let selected = repeated.capture()?;
    let record = selected
        .lookup(&actual.locator.key)?
        .context("enriched Tool absent")?;
    assert!(Arc::ptr_eq(record, &actual));
    assert!(!Arc::ptr_eq(record, &second));
    assert_eq!(
        record.tool_run_report()?,
        &ToolRunReportProjection::default()
    );
    drop((
        current,
        enriched,
        repeated,
        prepared,
        selected,
        actual,
        second,
        guards,
        second_guards,
    ));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn qualified_report_reuse_rejects_canonical_locator_report_or_ledger_drift() -> Result<()> {
    use crate::runtime::resident_input_evidence::{
        CommittedInputEvidenceDelta, QualifiedToolRunReport,
    };
    let (_home, db, mut actual) = source(tool("ExecCommand"))?;
    let plain = actual.clone();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let guards = hydrate(&db, &mut actual, &request(), &ledger)?;
    let actual = Arc::new(actual);
    let current = report_domain(actual.clone(), &ledger)?;
    let roots = std::collections::BTreeSet::from([actual.locator.key.clone()]);
    // Each rejected candidate starts from the actual writer's canonical readback.
    for mode in 0..10 {
        let mut changed = plain.clone();
        match mode {
            0 => changed.created_at.push('x'),
            1 => changed.order_sequence = Some(123),
            2 => changed.turn_index = Some(123),
            3 => changed.owner = Some(TurnOwner::Command),
            4 => changed.resident_bytes += 1,
            5 => changed.attempt_turn_keys = Some(Default::default()),
            6 => changed.locator.content_digest.push('x'),
            7 => changed.locator.scope.identity_incarnation += 1,
            8 => {
                let PreparedInputEvidence::ToolExecution(tool) = &mut changed.record else {
                    unreachable!()
                };
                tool.summary.push('x');
            }
            9 => {
                changed.tool_run_report =
                    Some(Box::new(QualifiedToolRunReport::from_qualified_source(
                        changed.locator.clone(),
                        ToolRunReportProjection {
                            sleep_reason: Some("forged".into()),
                            ..Default::default()
                        },
                    )))
            }
            _ => unreachable!(),
        }
        assert!(
            current
                .with_committed(
                    &CommittedInputEvidenceDelta {
                        records: vec![Arc::new(changed)],
                        ..Default::default()
                    },
                    roots.clone()
                )
                .is_err(),
            "same-revision candidate {mode} must reject"
        );
    }
    let mut changed = plain.clone();
    let mut wrong_source = changed.locator.clone();
    wrong_source.revision += 1;
    changed.tool_run_report = Some(Box::new(QualifiedToolRunReport::from_qualified_source(
        wrong_source,
        ToolRunReportProjection::default(),
    )));
    assert!(current
        .with_committed(
            &CommittedInputEvidenceDelta {
                records: vec![Arc::new(changed)],
                ..Default::default()
            },
            roots.clone()
        )
        .is_err());
    let foreign_ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let prepared = report_domain(Arc::new(plain), &foreign_ledger)?;
    assert!(current
        .with_committed_work_sources(
            &CommittedInputEvidenceDelta::default(),
            roots,
            Some(&prepared),
        )
        .is_err());
    let selected = current.capture()?;
    assert!(Arc::ptr_eq(
        selected
            .lookup(&actual.locator.key)?
            .context("original Tool lost")?,
        &actual
    ));
    drop((current, selected, actual, prepared, guards));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(foreign_ledger.report()?.observed_bytes, 0);
    Ok(())
}
