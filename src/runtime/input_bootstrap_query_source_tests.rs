use super::*;
use crate::runtime::{
    resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey},
    tests::support::LifecycleHarness,
};
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);
#[tokio::test]
async fn genuine_bootstrap_prepares_the_actual_native_input_baseline_without_dropping_ordinary_windows_or_adopting_a_raw_late_body(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();

    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual bootstrap Root missing")?;
    let inputs = root
        .input_evidence
        .as_ref()
        .context("actual Input missing")?;
    let cursor=runtime.runtime_db().connection()?.query_row(
        "SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
        rusqlite::params![inputs.scope().agent_id,inputs.scope().identity_incarnation],|row|row.get::<_,u64>(0))?;
    assert_eq!(inputs.query_coverage().birth_cursor, Some(cursor));
    assert_eq!(inputs.query_coverage().birth_after, None);
    assert!(inputs.query_coverage().born_key_seqs.is_empty());
    let ordinary = inputs.capture()?;
    assert!(ordinary.windows.iter().any(|window| window.selector
        == crate::runtime::resident_input_evidence::InputWindowSelector::RecentBriefs));
    let source =
        held.input_query_source(inputs.scope(), &runtime.inner.input_evidence_budget, LIMITS)?;
    assert_eq!(
        source.roles().original_base(),
        Some(inputs.query_coverage())
    );
    let entry = crate::types::TranscriptEntry::new(
        runtime.instance_agent_id(),
        crate::types::TranscriptEntryKind::AssistantRound,
        Some(0),
        None,
        serde_json::json!({"text":"late raw append has no native birth or accepted body"}),
    );
    runtime
        .runtime_db()
        .transaction(|tx| crate::runtime_db::evidence::append_transcript_entry_tx(tx, &entry))?;
    let again = held
        .capture_ready_snapshot()?
        .context("actual Root missing")?;
    assert!(Arc::ptr_eq(&root, &again));
    let selected = again.input_evidence.as_ref().unwrap().capture()?;
    assert!(!selected.records.iter().any(|record| record.locator.key
        == InputEvidenceKey::new(InputEvidenceFamily::Transcript, &entry.id)));
    assert_eq!(
        again
            .input_evidence
            .as_ref()
            .unwrap()
            .query_coverage()
            .birth_cursor,
        Some(cursor)
    );
    let alias: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_evidence_current WHERE family='transcript' AND record_id=?1",
        [&entry.id],
        |row| row.get(0),
    )?;
    let birth: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_query_birth_members WHERE family='transcript' AND record_id=?1",
        [&entry.id],
        |row| row.get(0),
    )?;
    assert_eq!((alias, birth), (0, 0));
    Ok(())
}
#[tokio::test]
async fn genuine_bootstrap_input_cursor_becomes_incomplete_after_an_actual_writer_without_the_shared_incremental_capsule(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();

    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    {
        let held = runtime.inner.mutation_owner.reserve().await?;
        let root = held
            .capture_ready_snapshot()?
            .context("actual Root missing")?;
        assert!(root
            .input_evidence
            .as_ref()
            .unwrap()
            .query_coverage()
            .birth_cursor
            .is_some());
    }
    let entry = crate::types::TranscriptEntry::new(
        runtime.instance_agent_id(),
        crate::types::TranscriptEntryKind::AssistantRound,
        Some(0),
        None,
        serde_json::json!({"text":"actual original writer ACK remains explicit incomplete"}),
    );
    runtime.persist_transcript_evidence(&entry).await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Root missing")?;
    let inputs = root
        .input_evidence
        .as_ref()
        .context("actual Input missing")?;
    assert!(inputs.query_coverage().birth_cursor.is_none());
    let source =
        held.input_query_source(inputs.scope(), &runtime.inner.input_evidence_budget, LIMITS)?;
    assert!(source.roles().original_base().is_none());
    let actual:u64=runtime.runtime_db().connection()?.query_row(
        "SELECT birth_seq FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND family='transcript' AND record_id=?3",
        rusqlite::params![inputs.scope().agent_id,inputs.scope().identity_incarnation,entry.id],|row|row.get(0))?;
    assert!(actual > 0);
    Ok(())
}
