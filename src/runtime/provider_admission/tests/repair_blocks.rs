use super::*;
use crate::domain::execution_protocol::WorkItemExecutionState;
use crate::types::{BriefKind, QueueEntryStatus, WorkItemPlanStatus};
use std::sync::atomic::Ordering;

#[test]
fn repair_blocked_operator_sources_have_receipts_without_provider_admission_or_consumption() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(repair_blocked_operator_sources_scenario());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn repair_blocked_operator_sources_scenario() {
    let server = OutageServer::start(false).await;
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), [&server.url, &server.url]);
    let runtime = open_runtime(home.path(), config.clone());
    let work = runtime
        .create_work_item(
            "source revision repair block".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut one = operator();
    one.work_item_id = Some(work.id.clone());
    let mut two = operator();
    two.priority = Priority::Interject;
    runtime.enqueue(one.clone()).await.unwrap();
    let original = runtime
        .runtime_db()
        .work_items()
        .latest(&work.id)
        .unwrap()
        .unwrap();
    let registration = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap()
        .work_items[&work.id]
        .clone();
    let stranded = runtime
        .update_work_item_fields(
            work.id.clone(),
            Some(original.objective.clone()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    crate::runtime::tests::support::seed_live_work_execution(
        &runtime,
        &original,
        registration.state,
    )
    .await;
    let canonical_before = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(
            SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await
                .unwrap(),
            RunLoopPoll::Idle
        ));
    }
    runtime.enqueue(two.clone()).await.unwrap();
    assert!(matches!(
        SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap(),
        RunLoopPoll::Idle
    ));
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    let canonical_after = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        canonical_after, canonical_before,
        "no admission, settlement, or consumption"
    );
    for message in [&one, &two] {
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(&message.id)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Queued,
        );
    }
    let briefs = runtime.storage().read_recent_briefs(100).unwrap();
    let failures = briefs
        .iter()
        .filter(|brief| brief.kind == BriefKind::Failure)
        .collect::<Vec<_>>();
    assert_eq!(
        failures.len(),
        3,
        "one episode alert and two input receipts"
    );
    for input in [&one, &two] {
        assert!(failures.iter().any(|brief| {
            brief.related_message_id.as_deref() == Some(input.id.as_str())
                && brief.text.contains("this input was not executed")
        }));
    }
    let inspection = runtime.inspect_scheduler_repair().await.unwrap();
    assert_eq!(inspection.source_revision_drifts.len(), 1);
    assert_eq!(inspection.source_revision_repair_blocks.len(), 1);
    assert!(inspection.source_revision_repair_blocks[0].drift_present);
    assert!(inspection.source_revision_repair_blocks[0]
        .alert_created_event_seq
        .is_some());
    drop(runtime);

    let runtime = open_runtime(home.path(), config);
    assert!(matches!(
        SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap(),
        RunLoopPoll::Idle
    ));
    assert_eq!(
        runtime
            .storage()
            .read_recent_briefs(100)
            .unwrap()
            .iter()
            .filter(|brief| brief.kind == BriefKind::Failure)
            .count(),
        3
    );
    // Simulate the separately authorized offline registration repair.
    // A real owner blocker must remain intact and is not replay authority.
    crate::runtime::tests::support::seed_live_work_execution(
        &runtime,
        &stranded,
        WorkItemExecutionState::NeedsRepair {
            generation: canonical_before.work_items[&work.id].generation(),
            repair_id: "business-repair".into(),
        },
    )
    .await;
    let inspection = runtime.inspect_scheduler_repair().await.unwrap();
    assert!(inspection.source_revision_drifts.is_empty());
    assert!(!inspection.source_revision_repair_blocks[0].drift_present);
    assert!(inspection.source_revision_repair_blocks[0]
        .retained_source_disposition
        .contains("requires_explicit_recovery"));
    assert!(matches!(
        SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .unwrap(),
        RunLoopPoll::Idle
    ));
    let protocol = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert!(matches!(
        &protocol.work_items[&work.id].state,
        WorkItemExecutionState::NeedsRepair { repair_id, .. } if repair_id == "business-repair"
    ));
    assert!(runtime.suppress_repair_blocked_source(&one).unwrap());
    assert!(!runtime.suppress_repair_blocked_source(&operator()).unwrap());
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}
