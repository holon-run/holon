use super::*;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};
#[tokio::test]
async fn unprepared_execution_input_is_typed_without_reading_canonical_partition() -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new(
        "execution-none",
        home.path().into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "execution-none".into(),
        context_config(),
    )?;
    let mut owner = runtime.begin_mutation().await?;
    owner.input_evidence = None; // deliberate missing-domain fixture; no canonical fallback.
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE execution_protocol_partitions RENAME TO c1_none_forbidden_partition",
        )?;
        Ok(())
    })?;
    let result = owner.execution_command_source();
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE c1_none_forbidden_partition RENAME TO execution_protocol_partitions",
        )?;
        Ok(())
    })?;
    let error = result.err().context("InputNone accepted")?;
    assert!(matches!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
        Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::NeedsPreparation(_))));
    let missing_task = owner
        .prepare_wait_for_outcome_with_id(
            "execution-none",
            None,
            crate::runtime::waiting::WaitForWakeKind::TaskResult,
            Some("not-an-admitted-task".into()),
            "original missing Task error".into(),
            None,
            None,
        )
        .await
        .err()
        .context("missing Task accepted")?;
    assert_eq!(
        crate::runtime_error::describe_runtime_error(&missing_task).code,
        "task_not_found"
    );
    Ok(())
}
