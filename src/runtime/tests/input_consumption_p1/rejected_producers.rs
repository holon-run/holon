//! Real producer rejection must preserve the input behind it (#3484).
use super::*;
use crate::types::{BriefKind, QueueEntryStatus};

#[tokio::test]
async fn issue_3484_stale_producer_rejection_delivers_backlog_after_rollback_or_restart() {
    for restart in [false, true] {
        for rollback in [false, true] {
            let dir = tempdir().unwrap();
            let workspace = tempdir().unwrap();
            let provider = Arc::new(CountingProvider {
                calls: Mutex::new(0),
                reply: "backlog delivered",
            });
            let open_runtime = || {
                RuntimeHandle::new(
                    "default",
                    dir.path().to_path_buf(),
                    workspace.path().to_path_buf(),
                    "http://127.0.0.1:7878".into(),
                    provider.clone(),
                    "default".into(),
                    context_config(),
                )
                .unwrap()
            };
            let mut runtime = open_runtime();
            let owner = runtime
                .create_work_item(
                    "old producer snapshot".into(),
                    Some(WorkItemPlanStatus::Ready),
                    None,
                    Vec::new(),
                )
                .await
                .unwrap();

            // Use the production emitter, then invalidate its snapshot through
            // the normal WorkItem mutation rather than rewriting durable data.
            runtime
                .emit_system_tick_from_work_queue(&owner, None, "queued_available", None)
                .await
                .unwrap();
            let entries = runtime.storage().latest_queue_entries().unwrap();
            assert_eq!(entries.len(), 1);
            let stale_id = entries[0].message_id.clone();
            let follower = runtime
                .enqueue(MessageEnvelope::new(
                    "default",
                    MessageKind::OperatorPrompt,
                    MessageOrigin::Operator {
                        actor_id: None,
                        actor_display_name: None,
                    },
                    AuthorityClass::OperatorInstruction,
                    Priority::Background,
                    MessageBody::Text {
                        text: "deliver the input behind the rejected producer".into(),
                    },
                ))
                .await
                .unwrap();
            let owner = runtime
                .update_work_item_fields(
                    owner.id,
                    Some("new producer snapshot".into()),
                    Some(WorkItemPlanStatus::NeedsInput),
                    None,
                    None,
                    None,
                )
                .await
                .unwrap();

            if rollback {
                let before = runtime.storage().latest_queue_entries().unwrap();
                runtime.inject_next_transition_fault(
                    crate::runtime_db::transitions::TransitionFaultPoint::AfterAuditWrites,
                );
                let error = scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                    .poll()
                    .await
                    .err()
                    .expect("a real storage failure must not be swallowed");
                assert!(
                    error
                        .to_string()
                        .contains("injected runtime transition fault"),
                    "{error:#}"
                );
                assert_eq!(runtime.storage().latest_queue_entries().unwrap(), before);
                assert!(!runtime
                    .storage()
                    .read_recent_events(usize::MAX)
                    .unwrap()
                    .iter()
                    .any(|event| event.kind == "scheduler_authority_input_rejected"));
                assert!(runtime.storage().read_recent_briefs(10).unwrap().is_empty());
                assert_eq!(provider.call_count().await, 0);
            }
            if restart {
                drop(runtime);
                runtime = open_runtime();
            }

            // No new ingress after reconstruction; run the actual loop through
            // rejection, follower consumption, and user-visible publication.
            let mut runner = tokio::spawn(runtime.clone().run());
            let delivered = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    assert!(
                        !runner.is_finished(),
                        "loop exited before delivering backlog: {:?}",
                        (&mut runner).await
                    );
                    let processed = runtime
                        .storage()
                        .latest_queue_entries()
                        .unwrap()
                        .iter()
                        .any(|entry| {
                            entry.message_id == follower.id
                                && entry.status == QueueEntryStatus::Processed
                        });
                    let published = runtime
                        .storage()
                        .read_recent_briefs(10)
                        .unwrap()
                        .iter()
                        .any(|brief| {
                            brief.related_message_id.as_deref() == Some(follower.id.as_str())
                        });
                    if processed && published {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await;
            runner.abort();
            let _ = runner.await;
            delivered.expect("rejected producer must not strand the queued follower");

            assert_eq!(provider.call_count().await, 1);
            assert!(runtime
                .storage()
                .read_recent_tool_executions(10)
                .unwrap()
                .is_empty());
            let entries = runtime.storage().latest_queue_entries().unwrap();
            assert_eq!(entries.len(), 2);
            assert!(entries.iter().any(|entry| {
                entry.message_id == stale_id && entry.status == QueueEntryStatus::Dropped
            }));
            assert!(entries.iter().any(|entry| {
                entry.message_id == follower.id && entry.status == QueueEntryStatus::Processed
            }));
            assert_eq!(runtime.agent_state().await.unwrap().pending, 0);
            assert_eq!(
                runtime.latest_work_item(&owner.id).await.unwrap(),
                Some(owner)
            );
            let events = runtime.storage().read_recent_events(usize::MAX).unwrap();
            let rejected = events
                .iter()
                .filter(|event| event.kind == "scheduler_authority_input_rejected")
                .collect::<Vec<_>>();
            assert_eq!(rejected.len(), 1);
            assert_eq!(rejected[0].data["message_id"], stale_id);
            assert_eq!(
                rejected[0].data["reason"],
                "canonical_autonomous_work_item_revision_stale"
            );
            assert_eq!(rejected[0].data["queue_disposition"], "dropped");
            let execution = runtime
                .runtime_db()
                .transitions()
                .load_execution_protocol_state_if_initialized("default")
                .unwrap()
                .unwrap();
            assert!(!execution
                .attempts
                .contains_key(&scheduler_executor::canonical_activation_id(&stale_id)));
            let turns = runtime.storage().read_recent_turns(10).unwrap();
            assert_eq!(turns.len(), 1);
            assert!(turns[0].input_message_ids.contains(&follower.id));
            assert!(!turns[0].input_message_ids.contains(&stale_id));
            assert_eq!(
                turns[0].terminal.as_ref().unwrap().kind,
                TurnTerminalKind::Completed
            );
            let briefs = runtime.storage().read_recent_briefs(10).unwrap();
            assert_eq!(briefs.len(), 1);
            assert_eq!(briefs[0].kind, BriefKind::Result);
            assert_eq!(
                briefs[0].related_message_id.as_deref(),
                Some(follower.id.as_str())
            );
            assert_eq!(turns[0].produced_brief_ids, vec![briefs[0].id.clone()]);
        }
    }
}
