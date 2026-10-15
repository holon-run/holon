use super::*;
use crate::{
    runtime::{
        input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
        resident_input_evidence::InputEvidenceScope,
        run_report_projection::project_tool_run_report,
    },
    runtime_db::{evidence::insert_tool_evidence_tx, RuntimeDb},
    types::{AuthorityClass, ToolExecutionRecord, ToolExecutionStatus},
};
use std::sync::Arc;

macro_rules! report_source_sql {
    ($selection:literal) => {
        concat!(
            r#"WITH report_source AS (
 SELECT json_object(
   'tool_name',json_extract(payload_json,'$.tool_name'),
   'input',json_object('patch',CASE
     WHEN json_extract(payload_json,'$.tool_name')='ApplyPatch' THEN CASE
       WHEN json_type(payload_json,'$.input')='text' THEN json_extract(payload_json,'$.input')
       WHEN json_type(payload_json,'$.input.patch')='text' THEN json_extract(payload_json,'$.input.patch') END END),
   'output',json_object('envelope',json_object(
     'result',json_object(
       'changed_paths',json(CASE WHEN json_extract(payload_json,'$.tool_name')='ApplyPatch'
          AND json_type(payload_json,'$.output.envelope.result.changed_paths')='array'
          THEN json_extract(payload_json,'$.output.envelope.result.changed_paths') ELSE 'null' END),
       'reason',CASE WHEN json_extract(payload_json,'$.tool_name')='Sleep'
          AND json_type(payload_json,'$.output.envelope.result.reason')='text'
          THEN json_extract(payload_json,'$.output.envelope.result.reason') END),
     'summary_text',CASE WHEN json_extract(payload_json,'$.tool_name')='Sleep'
          AND json_type(payload_json,'$.output.envelope.summary_text')='text'
          THEN json_extract(payload_json,'$.output.envelope.summary_text') END))
 ) AS projected FROM tool_executions
 WHERE agent_id=?1 AND evidence_id=?2
   AND COALESCE(json_extract(payload_json,'$.agent_id'),json_extract(payload_json,'$.session_id'))=?1
   AND json_extract(payload_json,'$.id')=?2
)
"#,
            $selection
        )
    };
}
const RAW_SQL: &str =
    report_source_sql!("SELECT length(CAST(projected AS BLOB)) FROM report_source");
// JSON object keys become owned Map keys in the nested input/output Values.
const NATIVE_SQL: &str = report_source_sql!(
    "SELECT (SELECT COUNT(*)+COALESCE(SUM(CASE WHEN typeof(key)='text' THEN 1 ELSE 0 END),0) FROM json_tree(projected)) FROM report_source"
);

fn tool(name: &str) -> ToolExecutionRecord {
    ToolExecutionRecord {
        id: "original-tool-report".into(),
        agent_id: "original-tool-owner".into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: None,
        tool_name: name.into(),
        created_at: Utc::now(),
        completed_at: None,
        duration_ms: 0,
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        input: serde_json::Value::Null,
        output: serde_json::Value::Null,
        summary: "actual canonical tool".into(),
        invocation_surface: None,
    }
}
fn db() -> Result<(tempfile::TempDir, RuntimeDb)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    Ok((home, db))
}
fn read(
    db: &RuntimeDb,
    record: &ToolExecutionRecord,
    ledger: &InputEvidenceBudgetLedger,
    byte_limit: u64,
) -> Result<(ToolRunReportProjection, Vec<Arc<BudgetViewGuard>>)> {
    let scope = InputEvidenceScope {
        agent_id: record.agent_id.clone(),
        identity_incarnation: 1,
    };
    db.transaction(|tx| {
        let request = InputEvidenceLoadRequest {
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            byte_limit: usize::try_from(byte_limit)?,
            record_limit: MAX_RECORDS,
        };
        let mut material = read_material::InputReadMaterial::new(&scope, &request, ledger, (0, 0))?;
        let projected = prepare_tool_run_report_on(
            tx,
            &record.agent_id,
            &record.id,
            &record.tool_name,
            &mut material,
        )?;
        Ok((projected, material.into_guards()))
    })
}

#[test]
fn finite_tool_report_sql_preserves_original_patch_and_sleep_source_semantics() -> Result<()> {
    let (_home, db) = db()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut original = tool("ApplyPatch");
    for input in [
        serde_json::json!(
            "rename from a/old.rs\nrename to b/new.rs\n--- /dev/null\n+++ b/added.rs\n"
        ),
        serde_json::json!({"patch":"--- a/old.rs\n+++ b/new.rs\n"}),
        serde_json::json!({"patch":"@@ no file headers @@"}),
    ] {
        original.input = input;
        original.output =
            serde_json::json!({"envelope":{"result":{"changed_paths":["fallback.rs",7,null]}}});
        db.transaction(|tx| insert_tool_evidence_tx(tx, &original))?;
        let (actual, held) = read(&db, &original, &ledger, 1024 * 1024)?;
        assert_eq!(actual, project_tool_run_report(&original, |_, _| Ok(()))?);
        drop(held);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    original.tool_name = "Sleep".into();
    for output in [
        serde_json::json!({"envelope":{"result":{"reason":" original wait "},"summary_text":"fallback"}}),
        serde_json::json!({"envelope":{"result":{"reason":" "},"summary_text":"fallback"}}),
        serde_json::json!({"envelope":{"summary_text":" fallback "}}),
    ] {
        original.output = output;
        db.transaction(|tx| insert_tool_evidence_tx(tx, &original))?;
        let (actual, held) = read(&db, &original, &ledger, 1024 * 1024)?;
        assert_eq!(actual, project_tool_run_report(&original, |_, _| Ok(()))?);
        drop(held);
    }
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn finite_tool_report_sql_keeps_large_stdout_out_of_rust_and_holds_actual_guards() -> Result<()> {
    let (_home, db) = db()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut original = tool("ExecCommand");
    original.output = serde_json::json!({"envelope":{"result":{"stdout":"x".repeat(10*1024*1024),"changed_paths":["not-a-patch"]}}});
    db.transaction(|tx| insert_tool_evidence_tx(tx, &original))?;
    let (actual, held) = read(&db, &original, &ledger, MAX_METADATA_BYTES as u64)?;
    assert_eq!(actual, ToolRunReportProjection::default());
    assert!(ledger.report()?.observed_bytes > 0);
    assert!(ledger.report()?.observed_bytes < 128 * 1024);
    let last = Arc::downgrade(held.last().context("actual report guard absent")?);
    let holder = held.clone();
    drop(held);
    assert!(last.upgrade().is_some());
    drop((actual, holder));
    assert!(last.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let aliases: usize =
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| {
                r.get(0)
            })?;
    assert_eq!(
        aliases, 0,
        "the finite helper alone grants no qualification"
    );
    Ok(())
}

#[test]
fn finite_tool_report_sql_rejects_raw_and_native_material_before_owned_decode() -> Result<()> {
    let (_home, db) = db()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for native in [false, true] {
        let mut original = tool("ApplyPatch");
        if native {
            original.output =
                serde_json::json!({"envelope":{"result":{"changed_paths":vec!["p";9000]}}});
        } else {
            original.input = serde_json::json!({"patch":"{".repeat(9*1024*1024)});
        }
        db.transaction(|tx| insert_tool_evidence_tx(tx, &original))?;
        let error = read(&db, &original, &ledger, MAX_METADATA_BYTES as u64)
            .err()
            .context("material must reject before decode")?;
        let label = if native {
            "Tool report native"
        } else {
            "Tool report raw"
        };
        assert!(
            matches!(error.downcast_ref::<InputEvidenceSnapshotError>(),Some(InputEvidenceSnapshotError::ResourceExhausted(actual)) if *actual==label),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn finite_tool_report_sql_debits_one_original_remaining_phase_without_aliases() -> Result<()> {
    let (_home, db) = db()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut original = tool("ApplyPatch");
    original.output =
        serde_json::json!({"envelope":{"result":{"changed_paths":["p".repeat(32 * 1024)]}}});
    db.transaction(|tx| insert_tool_evidence_tx(tx, &original))?;
    // Raw, native and the final distinct report copies share one actual phase.
    let error = read(&db, &original, &ledger, 1024 * 1024)
        .err()
        .context("combined report material must reject")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(
                "Input read material"
            ))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let (actual, held) = read(&db, &original, &ledger, 2 * 1024 * 1024)?;
    assert_eq!(actual.output_paths, vec!["p".repeat(32 * 1024)]);
    assert!(ledger.report()?.observed_bytes > 64 * 1024);
    drop((actual, held));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| r
                .get::<_, usize>(
                0
            ))?,
        0
    );
    Ok(())
}

#[test]
fn finite_tool_report_sql_counts_nested_object_keys_before_owned_value_decode() -> Result<()> {
    let (_home, db) = db()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut original = tool("ApplyPatch");
    let nested = (0..5000)
        .map(|index| (format!("k{index}"), serde_json::Value::Null))
        .collect::<serde_json::Map<_, _>>();
    original.output = serde_json::json!({
        "envelope":{"result":{"changed_paths":[nested]}}
    });
    db.transaction(|tx| insert_tool_evidence_tx(tx, &original))?;
    let (raw, value_nodes, native_nodes): (u64, usize, usize) = db.transaction(|tx| {
        Ok((
            tx.query_row(RAW_SQL, params![original.agent_id, original.id], |r| {
                r.get(0)
            })?,
            tx.query_row(
                report_source_sql!(
                    "SELECT (SELECT COUNT(*) FROM json_tree(projected)) FROM report_source"
                ),
                params![original.agent_id, original.id],
                |r| r.get(0),
            )?,
            tx.query_row(NATIVE_SQL, params![original.agent_id, original.id], |r| {
                r.get(0)
            })?,
        ))
    })?;
    assert!((raw + 4096) * 8 < 1024 * 1024, "raw source fits the phase");
    assert!(
        value_nodes * 8 <= MAX_RECORDS,
        "values alone miss the limit"
    );
    assert!(
        native_nodes * 8 > MAX_RECORDS,
        "owned object keys exhaust it"
    );
    let error = read(&db, &original, &ledger, MAX_METADATA_BYTES as u64)
        .err()
        .context("nested object keys must reject before owned Value decode")?;
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(
                "Tool report native"
            ))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        db.connection()?
            .query_row("SELECT COUNT(*) FROM input_evidence_current", [], |r| r
                .get::<_, usize>(
                0
            ))?,
        0,
        "a report-source budget failure grants no alias"
    );
    Ok(())
}

#[test]
fn finite_original_batch_report_counts_original_summary_fields_without_adopting_prompt_item_counts(
) -> Result<()> {
    let (_home, db) = db()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut original = tool("ExecCommandBatch");
    original.input = serde_json::json!({"items":[{}, {}, {}, {}, {}]});
    for (result, expected) in [
        (
            serde_json::json!({"completed_count":2,"failed_count":1,"items":[]}),
            3,
        ),
        (
            serde_json::json!({"completed_count":0,"failed_count":0,"items":[{"state":"completed"}]}),
            0,
        ),
        (
            serde_json::json!({"completed_count":-1,"failed_count":"wrong-type"}),
            0,
        ),
        (serde_json::json!({"failed_count":4}), 4),
    ] {
        original.output =
            serde_json::json!({"envelope":{"result":result},"stdout":"x".repeat(1024*1024)});
        db.transaction(|tx| insert_tool_evidence_tx(tx, &original))?;
        let (report, guards) = read(&db, &original, &ledger, 4 * 1024 * 1024)?;
        assert_eq!(report, project_tool_run_report(&original, |_, _| Ok(()))?);
        assert_eq!(report.batched_exec_command_items, expected);
        assert!(
            report.patch_paths.is_empty()
                && report.output_paths.is_empty()
                && report.sleep_reason.is_none()
        );
        assert!(
            ledger.report()?.observed_bytes < 256 * 1024,
            "unrelated stdout/items entered the original finite report"
        );
        drop(guards);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
