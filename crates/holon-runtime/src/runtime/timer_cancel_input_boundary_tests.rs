use super::timer_cancel_input_tests::{
    assert_qualified_canonical, execution_qualifications, fixture, observe_execution, waiting_work,
};
use super::*;
use crate::runtime::resident_input_evidence::{
    InputEvidenceFamily as Family, InputEvidenceKey as Key, PreparedInputEvidence,
    QualifiedInputEvidence,
};
use crate::runtime::tests::support::seed_live_waiting_work_execution;
use crate::runtime_db::input_evidence_snapshot::{
    load_input_evidence_snapshot_tx, InputEvidenceLoadRequest, InputEvidenceSnapshotError,
};
use crate::runtime_db::transitions::{PreparedInputTransition, PreparedTimerTransition};

fn boundary_rows(runtime: &RuntimeHandle, queries: &[&str]) -> Result<Vec<Vec<String>>> {
    runtime.runtime_db().transaction(|tx| {
        queries
            .iter()
            .map(|query| {
                let mut statement = tx.prepare(query)?;
                let columns = statement.column_count();
                let mut rows = statement
                    .query_map([], |row| {
                        (0..columns)
                            .map(|column| row.get::<_, rusqlite::types::Value>(column))
                            .collect::<rusqlite::Result<Vec<_>>>()
                            .map(|values| format!("{values:?}"))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.sort();
                Ok(rows)
            })
            .collect()
    })
}

const EXECUTION_QUERIES: &[&str] = &[
    "SELECT * FROM execution_protocol_work_items",
    "SELECT * FROM execution_protocol_attempts",
    "SELECT * FROM execution_protocol_outcomes",
    "SELECT * FROM input_evidence_current WHERE family='work_execution'",
];

async fn assert_boundary_execution_unchanged(
    runtime: &RuntimeHandle,
    observed: &QualifiedInputEvidence,
) -> Result<()> {
    assert_qualified_canonical(runtime, observed)?;
    let PreparedInputEvidence::WorkExecution { work_item_id, .. } = &observed.record else {
        panic!("expected WorkExecution");
    };
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(
        published
            .input_evidence
            .as_ref()
            .unwrap()
            .capture()?
            .lookup(&Key::new(Family::WorkExecution, work_item_id))?
            .unwrap()
            .as_ref(),
        observed
    );
    Ok(())
}

fn assert_boundary_raw_change_fails_closed(
    runtime: &RuntimeHandle,
    observed: &QualifiedInputEvidence,
) -> Result<()> {
    let PreparedInputEvidence::WorkExecution {
        work_item_id,
        record,
    } = &observed.record
    else {
        panic!("expected WorkExecution");
    };
    let qualifications = execution_qualifications(runtime)?;
    runtime.runtime_db().transaction(|tx| {
        assert_eq!(
            tx.execute(
                "UPDATE execution_protocol_work_items
                 SET payload_json=json_set(payload_json, '$.source_revision', ?1)
                 WHERE agent_id=?2 AND work_item_id=?3",
                rusqlite::params![
                    record.source_revision + 1,
                    observed.locator.scope.agent_id,
                    work_item_id,
                ],
            )?,
            1
        );
        Ok(())
    })?;
    let request = InputEvidenceLoadRequest {
        query: None,
        windows: Vec::new(),
        mandatory_roots: [Key::new(Family::WorkExecution, work_item_id)]
            .into_iter()
            .collect(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: 512,
        byte_limit: 1024 * 1024,
    };
    let error = runtime
        .runtime_db()
        .transaction(|tx| load_input_evidence_snapshot_tx(tx, &observed.locator.scope, &request))
        .expect_err("an excluded WorkExecution cannot gain authority from a raw canonical write");
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ProjectionChanged(_))
        ) || error.root_cause().to_string() == "prepared evidence raw/normalized digest mismatch",
        "{error:#}"
    );
    assert_eq!(execution_qualifications(runtime)?, qualifications);
    Ok(())
}

#[tokio::test]
async fn timer_cancel_input_boundary_admitted_trigger_is_not_retracted() -> Result<()> {
    use crate::runtime::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
    use crate::types::QueueEntryStatus;

    let (_home, _workspace, runtime, mut timer) = fixture().await?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (work, wait) = waiting_work(&runtime, &timer).await?;
    seed_live_waiting_work_execution(&runtime, &work, &wait.id).await;
    runtime.fire_timer_record(&mut timer).await?;
    let wake = runtime
        .runtime_db()
        .timers()
        .pending_wake(&timer.id)?
        .unwrap();
    assert_eq!(
        runtime
            .runtime_db()
            .wait_conditions()
            .latest_for_agent("default")?
            .into_iter()
            .find(|candidate| candidate.id == wait.id)
            .unwrap()
            .status,
        WaitConditionStatus::Triggered
    );
    drop(_refresh);
    assert!(matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            SchedulerDecisionExecutor::new(&runtime).poll(),
        )
        .await??,
        RunLoopPoll::Message(_)
    ));
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let admitted_wait = runtime
        .runtime_db()
        .wait_conditions()
        .latest_for_agent("default")?
        .into_iter()
        .find(|candidate| candidate.id == wait.id)
        .unwrap();
    assert_ne!(admitted_wait.status, WaitConditionStatus::Cancelled);
    let admitted_work = runtime.latest_work_item(&work.id).await?.unwrap();
    let observed = observe_execution(&runtime, &work.id).await?;
    let before = boundary_rows(&runtime, EXECUTION_QUERIES)?;
    let pending = runtime.agent_state().await?.pending;

    assert_eq!(
        runtime.cancel_timer(&timer.id).await?.status,
        TimerStatus::Cancelled
    );

    assert_eq!(
        runtime
            .runtime_db()
            .wait_conditions()
            .latest_for_agent("default")?
            .into_iter()
            .find(|candidate| candidate.id == wait.id)
            .unwrap(),
        admitted_wait
    );
    assert_eq!(
        runtime.latest_work_item(&work.id).await?.unwrap(),
        admitted_work
    );
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&wake.message_id)?
            .unwrap()
            .status,
        QueueEntryStatus::Dequeued
    );
    assert_eq!(runtime.agent_state().await?.pending, pending);
    assert_eq!(boundary_rows(&runtime, EXECUTION_QUERIES)?, before);
    assert_boundary_execution_unchanged(&runtime, &observed).await?;
    assert_boundary_raw_change_fails_closed(&runtime, &observed)?;
    Ok(())
}

async fn cancel_excluding_changed_work(completed: bool) -> Result<()> {
    let (_home, _workspace, runtime, timer) = fixture().await?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (mut work, wait) = waiting_work(&runtime, &timer).await?;
    seed_live_waiting_work_execution(&runtime, &work, &wait.id).await;
    if completed {
        runtime
            .complete_work_item_with_report(
                work.id.clone(),
                crate::runtime::WorkItemCompletionAuthority::Control,
                "Completed boundary fixture.".into(),
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                Vec::new(),
            )
            .await?;
    } else {
        // A stale legacy link must not make the timer own an unrelated pause.
        let expected_revision = work.revision;
        work.revision += 1;
        work.blocked_by_wait_id = Some(wait.id.clone());
        runtime
            .update_work_item_record_expected(&work, expected_revision)
            .await?;
        crate::runtime::tests::support::seed_live_work_execution(
            &runtime,
            &work,
            crate::domain::execution_protocol::WorkItemExecutionState::Paused {
                generation: work.revision,
                reason: "independent pause".into(),
            },
        )
        .await;
    }
    let work = runtime.latest_work_item(&work.id).await?.unwrap();
    let observed = observe_execution(&runtime, &work.id).await?;
    let before = boundary_rows(&runtime, EXECUTION_QUERIES)?;
    let pending = runtime.agent_state().await?.pending;

    assert_eq!(
        runtime.cancel_timer(&timer.id).await?.status,
        TimerStatus::Cancelled
    );

    assert_eq!(runtime.latest_work_item(&work.id).await?.unwrap(), work);
    assert_eq!(
        runtime
            .runtime_db()
            .wait_conditions()
            .latest_for_agent("default")?
            .into_iter()
            .find(|candidate| candidate.id == wait.id)
            .unwrap()
            .status,
        WaitConditionStatus::Cancelled
    );
    assert_eq!(runtime.agent_state().await?.pending, pending);
    assert_eq!(boundary_rows(&runtime, EXECUTION_QUERIES)?, before);
    if completed {
        // Window refresh may evict completed work; its durable qualification must not change.
        assert_qualified_canonical(&runtime, &observed)?;
    } else {
        assert_boundary_execution_unchanged(&runtime, &observed).await?;
    }
    assert_boundary_raw_change_fails_closed(&runtime, &observed)?;
    Ok(())
}

#[tokio::test]
async fn timer_cancel_input_boundary_completed_work_execution_is_not_acknowledged() -> Result<()> {
    cancel_excluding_changed_work(true).await
}

#[tokio::test]
async fn timer_cancel_input_boundary_different_wait_owner_is_not_acknowledged() -> Result<()> {
    cancel_excluding_changed_work(false).await
}

#[tokio::test]
async fn timer_cancel_input_boundary_original_command_rejects_foreign_and_stale_scope() -> Result<()>
{
    for stale_incarnation in [false, true] {
        let (_home, _workspace, runtime, timer) = fixture().await?;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let (work, wait) = waiting_work(&runtime, &timer).await?;
        seed_live_waiting_work_execution(&runtime, &work, &wait.id).await;
        let observed = observe_execution(&runtime, &work.id).await?;
        let scope = observed.locator.scope.clone();
        let mut cancelled = timer.clone();
        cancelled.status = TimerStatus::Cancelled;
        cancelled.next_fire_at = None;
        let prepared = PreparedInputTransition::timer(
            scope.clone(),
            crate::ids::audit_event_id(),
            runtime.now(),
            PreparedTimerTransition::cancel(
                crate::runtime_db::TimerCancel {
                    expected: timer.clone(),
                    record: cancelled,
                    cancelled_at: runtime.now(),
                    agent_state: None,
                },
                None,
                None,
            ),
        )?;
        let mut repository_scope = scope.clone();
        if stale_incarnation {
            // Keep the original command and reserved scope. Only the durable
            // identity advances, so this reaches the live-incarnation fence.
            let mut identity: crate::types::AgentIdentityRecord =
                runtime.runtime_db().transaction(|tx| {
                    let json: String = tx.query_row(
                        "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
                        [&scope.agent_id],
                        |row| row.get(0),
                    )?;
                    Ok(serde_json::from_str(&json)?)
                })?;
            identity.incarnation += 1;
            identity.updated_at = runtime.now();
            runtime.runtime_db().agent_identities().upsert(&identity)?;
        } else {
            let foreign = crate::types::AgentIdentityRecord::new(
                "foreign-cancel-owner",
                crate::types::AgentKind::Named,
                crate::types::AgentVisibility::Public,
                crate::types::AgentOwnership::SelfOwned,
                crate::types::AgentProfilePreset::PublicNamed,
                None,
                None,
            );
            runtime.runtime_db().agent_identities().upsert(&foreign)?;
            repository_scope.agent_id = foreign.agent_id;
            repository_scope.identity_incarnation = foreign.incarnation;
        }
        let queries = [
            "SELECT * FROM timers",
            "SELECT * FROM timer_wakes",
            "SELECT * FROM wait_conditions",
            "SELECT * FROM work_items",
            "SELECT * FROM execution_protocol_work_items",
            "SELECT * FROM execution_protocol_attempts",
            "SELECT * FROM execution_protocol_outcomes",
            "SELECT * FROM input_evidence_current",
            "SELECT * FROM input_query_birth_cursors",
            "SELECT * FROM input_query_birth_members",
            "SELECT * FROM input_mutation_receipts",
        ];
        let before = boundary_rows(&runtime, &queries)?;
        let token = runtime.inner.mutation_owner.capture()?.unwrap().token;

        let error = runtime
            .runtime_db()
            .transitions_in_scope(&repository_scope)
            .commit_prepared_transition(&prepared)
            .expect_err("an original cancellation cannot cross agent or identity incarnation");

        let error = format!("{error:#}");
        assert!(
            if stale_incarnation {
                error.contains("incarnation")
            } else {
                error.contains("scope differs")
            },
            "{error}"
        );
        assert_eq!(boundary_rows(&runtime, &queries)?, before);
        assert_eq!(
            runtime.inner.mutation_owner.capture()?.unwrap().token,
            token
        );
        assert_boundary_execution_unchanged(&runtime, &observed).await?;
    }
    Ok(())
}
