use super::*;
use crate::{
    runtime::{
        resident_input_evidence::{InputEvidenceFamily, InputEvidenceScope},
        run_birth_sessions::RunBirthSessions,
        tests::support::{context_config, tempdir},
        RuntimeHandle,
    },
    types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        ToolExecutionRecord, ToolExecutionStatus, TranscriptEntry, TranscriptEntryKind,
    },
};

// The original factory reads real native Source and actual owner roles. No
// serialized resident metadata or fabricated complete frontier is installed.
pub(super) async fn prepare_named(runtime: &RuntimeHandle) -> Result<()> {
    let mut held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("fixture Root missing")?;
    let scope = InputEvidenceScope {
        agent_id: held.agent_id().to_owned(),
        identity_incarnation: held.identity_incarnation(),
    };
    let roles = held.input_query_roles(
        &scope,
        &runtime.inner.input_evidence_budget,
        (64 * 1024 * 1024, 65_536),
    )?;
    let event_roles = held.current_event_roles(
        &scope,
        &runtime.inner.input_evidence_budget,
        (64 * 1024 * 1024, 65_536),
    )?;
    let (inputs, current) =
        crate::runtime_db::input_evidence_snapshot::prepare_named_run_sources_for_fixture(
            runtime.runtime_db(),
            &scope,
            &runtime.inner.input_evidence_budget,
            roles.as_ref(),
            held.owner_epoch(),
            &root.state,
            event_roles.as_ref(),
        )
        .context("actual named Run Input/Current factories")?;
    held.begin("actual-named-run-source-fixture".into())?;
    held.committed_prepared_with_current_read_domains(
        root.state.clone(),
        root.episodes.clone(),
        Some(Arc::new(inputs)),
        root.work.clone(),
        root.tasks.clone(),
        root.scheduling.clone(),
        Some(current),
    )?;
    held.publish(root.state.clone())?;
    Ok(())
}

pub(super) async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, RunBirthSessions, String)>
{
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "original-run-report",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let old = runtime
        .enqueue(MessageEnvelope::new(
            "original-run-report",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "older required dependency".into(),
            },
        ))
        .await?;
    prepare_named(&runtime).await?;
    let sessions = runtime.begin_run_birth_sessions().await?;
    let transcript = TranscriptEntry::new(
        "original-run-report",
        TranscriptEntryKind::AssistantRound,
        Some(1),
        Some(old.id.clone()),
        serde_json::json!({"blocks":[{"type":"text","text":"born output"}]}),
    );
    runtime.persist_transcript_evidence(&transcript).await?;
    runtime
        .persist_tool_execution_evidence(&ToolExecutionRecord {
            id: "actual-born-apply-patch".into(),
            agent_id: "original-run-report".into(),
            work_item_id: None,
            turn_index: 1,
            turn_id: None,
            tool_name: "ApplyPatch".into(),
            created_at: chrono::Utc::now(),
            completed_at: None,
            duration_ms: 0,
            authority_class: AuthorityClass::OperatorInstruction,
            status: ToolExecutionStatus::Success,
            input: serde_json::json!({"patch":"--- /dev/null\n+++ b/report.rs\n"}),
            output: serde_json::Value::Null,
            summary: "actual qualified report".into(),
            invocation_surface: None,
        })
        .await?;
    prepare_named(&runtime).await?;
    Ok((home, runtime, sessions, old.id))
}

#[tokio::test]
async fn actual_run_report_counts_only_original_births_and_holds_qualified_sources_without_root_or_sql(
) -> Result<()> {
    let (_home, runtime, sessions, old_id) = fixture().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let root_count = Arc::strong_count(&root);
    let ledger = &runtime.inner.input_evidence_budget;
    let before_capture = ledger.report()?.observed_bytes;
    let captured = runtime.capture_run_birth_sessions(&sessions)?;
    assert!(captured
        .inputs
        .input
        .records
        .iter()
        .any(|r| r.locator.key.id == old_id));
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE messages RENAME TO report_forbidden_messages;
         ALTER TABLE transcript_entries RENAME TO report_forbidden_transcript;
         ALTER TABLE tool_executions RENAME TO report_forbidden_tools;
         ALTER TABLE tasks RENAME TO report_forbidden_tasks;
         ALTER TABLE audit_events RENAME TO report_forbidden_events;",
    )?;
    let report = captured.project_report((usize::MAX, usize::MAX))?;
    assert_eq!(report.born_inputs().count(), 2);
    assert!(!report.born_inputs().any(|r| r.locator.key.id == old_id));
    let tool = report
        .born_inputs()
        .find(|r| r.locator.key.family == InputEvidenceFamily::ToolExecution)
        .unwrap();
    assert_eq!(tool.tool_run_report()?.patch_paths, ["report.rs"]);
    assert_eq!(report.born_tasks().count(), 0);
    assert!(report
        .born_events()
        .all(|event| event.event_seq > sessions.baseline.event_cursor));
    assert_eq!(Arc::strong_count(&root), root_count);
    let weak = Arc::downgrade(&report._indices);
    let last = report._indices.clone();
    let index_bytes = report.material.0;
    drop(report);
    assert!(weak.upgrade().is_some());
    assert_eq!(
        ledger.report()?.observed_bytes,
        before_capture + index_bytes
    );
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, before_capture);
    Ok(())
}

#[tokio::test]
async fn original_run_report_source_and_first_indices_share_exact_byte_and_node_allowance(
) -> Result<()> {
    let (_home, runtime, sessions, _old_id) = fixture().await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let measured = runtime
        .capture_run_birth_sessions(&sessions)?
        .project_report((usize::MAX, usize::MAX))?;
    let own = measured.material_estimate()?;
    drop(measured);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let exact = (usize::try_from(own.0)?, own.1);
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let error = runtime
            .capture_run_birth_sessions(&sessions)?
            .project_report(limits)
            .err()
            .context("Run report source and first index copied with separate allowances")?;
        assert!(
            matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let exact_report = runtime
        .capture_run_birth_sessions(&sessions)?
        .project_report(exact)?;
    assert_eq!(exact_report.material_estimate()?, own);
    assert_eq!(exact_report.born_inputs().count(), 2);
    drop(exact_report);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let mut foreign = runtime.capture_run_birth_sessions(&sessions)?;
    let other =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    foreign.inputs.input.budget = Some(other.clone());
    let error = foreign
        .project_report(exact)
        .err()
        .context("foreign ledger minted report indices")?;
    assert!(format!("{error:#}").contains("ledger differs"), "{error:#}");
    assert_eq!(other.report()?.observed_bytes, 0);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn original_run_report_rejects_a_missing_exact_tool_report_before_any_index_copy(
) -> Result<()> {
    let (_home, runtime, sessions, _old_id) = fixture().await?;
    let mut captured = runtime.capture_run_birth_sessions(&sessions)?;
    let tool = captured
        .inputs
        .input
        .records
        .iter_mut()
        .find(|record| record.locator.key.family == InputEvidenceFamily::ToolExecution)
        .context("actual born Tool missing")?;
    // Adversarial old-format metadata, with the actual canonical locator and
    // source cursor unchanged. Missing report metadata grants no empty result.
    let key = tool.locator.key.clone();
    Arc::make_mut(tool).tool_run_report = None;
    let ledger = &runtime.inner.input_evidence_budget;
    let before = ledger.report()?.observed_bytes;
    let error = captured
        .project_report((usize::MAX, usize::MAX))
        .err()
        .context("missing qualified Tool report was silently omitted")?;
    assert!(
        matches!(error.downcast_ref::<crate::runtime::resident_input_evidence::InputEvidenceReadError>(),
        Some(crate::runtime::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(actual)) if actual == &key),
        "{error:#}"
    );
    assert!(
        ledger.report()?.observed_bytes < before,
        "failed capture must release its real source guards"
    );
    Ok(())
}
