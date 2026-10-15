use super::*;
use crate::runtime::tests::support::LifecycleHarness;
use crate::runtime_db::input_evidence_snapshot::{
    exercise_original_publication_for_fixture, InputEvidenceLoadRequest, InputEvidenceSnapshotError,
};
use crate::types::{
    AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
};
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);
fn message(agent: &str, id: &str) -> MessageEnvelope {
    let mut row = MessageEnvelope::new(
        agent,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "actual incremental native Input".into(),
        },
    );
    row.id = id.into();
    row
}
fn policy(limits: (usize, usize)) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: limits.0,
        record_limit: limits.1,
    }
}
#[tokio::test]
async fn genuine_bootstrap_to_actual_queue_ack_advances_live_input_role_and_pure_capture_keeps_ordinary_windows(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let (session, cursor) = {
        let held = runtime.inner.mutation_owner.reserve().await?;
        let root = held
            .capture_ready_snapshot()?
            .context("actual bootstrap Root missing")?;
        let cursor = root
            .input_evidence
            .as_ref()
            .unwrap()
            .query_coverage()
            .birth_cursor
            .context("actual bootstrap cursor missing")?;
        (
            runtime.register_input_birth_session_from_snapshot(&held, &root)?,
            cursor,
        )
    };
    let accepted = runtime
        .enqueue(message(
            runtime.instance_agent_id(),
            "actual-source-publication-message",
        ))
        .await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual ACK Root missing")?;
    let inputs = root
        .input_evidence
        .as_ref()
        .context("actual Input missing")?;
    let key = InputEvidenceKey::new(
        crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
        &accepted.id,
    );
    assert_eq!(inputs.query_coverage().birth_cursor, Some(cursor + 1));
    assert_eq!(
        inputs.query_coverage().born_key_seqs.get(&key),
        Some(&(cursor + 1))
    );
    assert!(inputs
        .capture()?
        .windows
        .iter()
        .any(|window| window.selector
            == crate::runtime::resident_input_evidence::InputWindowSelector::RecentBriefs));
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE messages RENAME TO forbidden_incremental_source_messages; ALTER TABLE transcript_entries RENAME TO forbidden_incremental_source_transcript")?;
    let captured = runtime.capture_input_birth_session_from_snapshot(&session, &root)?;
    assert_eq!(captured.query.birth_cursor, Some(cursor + 1));
    let record = captured
        .input
        .records
        .iter()
        .find(|record| record.locator.key == key)
        .context("actual born Message omitted")?;
    let original = inputs
        .capture()?
        .records
        .into_iter()
        .find(|record| record.locator.key == key)
        .context("actual Message lost its resident allocation")?;
    assert!(Arc::ptr_eq(record, &original));
    Ok(())
}

#[tokio::test]
async fn actual_final_publication_admits_fresh_remaining_before_query_copy_and_transfers_only_its_new_guard(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Root missing")?;
    let input = root
        .input_evidence
        .as_ref()
        .context("actual Input missing")?;
    let scope = input.scope();
    let ledger = &runtime.inner.input_evidence_budget;
    let session = runtime.register_input_birth_session_from_snapshot(&held, &root)?;
    let source = held.input_query_source(scope, ledger, LIMITS)?;
    let row = message(runtime.instance_agent_id(), "actual-final-copy-budget");
    let baseline = ledger.report()?.observed_bytes;
    let mut measured = None;
    let rollback: Result<()> = exercise_original_publication_for_fixture(
        runtime.runtime_db(),
        scope,
        ledger,
        &source,
        &policy(LIMITS),
        &row,
        root.state.as_ref(),
        |before, delta| {
            let publication = delta
                .query_advance
                .as_ref()
                .and_then(|advance| advance.publication())
                .context("actual final capsule missing")?;
            measured = Some((before, publication.cost_for_fixture()));
            anyhow::bail!("rollback actual final material measurement")
        },
    );
    assert!(format!("{:#}", rollback.unwrap_err())
        .contains("rollback actual final material measurement"));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let (before, cost) = measured.context("actual final material not measured")?;
    let exact = (
        before
            .0
            .checked_add(usize::try_from(cost.0)?)
            .context("fixture final bytes overflow")?,
        before
            .1
            .checked_add(cost.1)
            .context("fixture final nodes overflow")?,
    );
    let count = || -> Result<(usize, usize, u64)> {
        Ok(runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM messages WHERE id=?1),(SELECT COUNT(*) FROM input_evidence_current WHERE record_id=?1),(SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?2 AND identity_incarnation=?3)",
        rusqlite::params![row.id,scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
    };
    let original = count()?;
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let mut copied = false;
        let result = exercise_original_publication_for_fixture(
            runtime.runtime_db(),
            scope,
            ledger,
            &source,
            &policy(limits),
            &row,
            root.state.as_ref(),
            |_, _| {
                copied = true;
                Ok(())
            },
        );
        let error = result
            .err()
            .context("final copy bypassed actual remaining allowance")?;
        assert!(
            matches!(
                error.downcast_ref::<InputEvidenceSnapshotError>(),
                Some(InputEvidenceSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert!(!copied);
        assert_eq!(count()?, original);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let (delta, weak, scratch) = exercise_original_publication_for_fixture(
        runtime.runtime_db(),
        scope,
        ledger,
        &source,
        &policy(exact),
        &row,
        root.state.as_ref(),
        |_, delta| {
            let publication = delta
                .query_advance
                .as_ref()
                .and_then(|advance| advance.publication())
                .context("actual capsule missing")?;
            Ok((
                delta.clone(),
                publication.weak_guard_for_fixture(),
                publication.weak_scratch_for_fixture(),
            ))
        },
    )?;
    let before_guard = ledger.report()?.observed_bytes;
    let mut drift = root.state.as_ref().clone();
    drift.current_turn_id = Some("different-final-state".repeat(1024));
    let error = input
        .mandatory_roots_for_publication(
            &delta,
            &drift,
            &crate::queue::RuntimeQueue::default(),
            None,
        )
        .err()
        .context("changed final State allocated mandatory roots")?;
    assert!(
        format!("{error:#}").contains("actual final State selectors"),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before_guard);
    let next = input
        .with_committed(&delta, Default::default())?
        .context("actual frontier/ordinary closure failed installation")?;
    assert_eq!(next.query_coverage().birth_cursor, Some(original.2 + 1));
    assert_eq!(next.query_coverage().born_key_seqs.len(), 1);
    drop(delta);
    drop(source);
    drop(session);
    drop(held);
    assert!(
        scratch.upgrade().is_none(),
        "new Root pinned the retired writer copy scratch"
    );
    assert!(
        weak.upgrade().is_some(),
        "new metadata lost its actual guard before the resident last holder"
    );
    drop(next);
    assert!(
        weak.upgrade().is_none(),
        "new Root retained the old Source or entire writer phase"
    );
    Ok(())
}
