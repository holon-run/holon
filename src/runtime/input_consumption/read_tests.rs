use super::*;
use crate::context::inputs::ContextHistoryMessage;
use crate::runtime::tests::support::LifecycleHarness;

fn message() -> MessageEnvelope {
    MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "qualified idle Queue".into(),
        },
    )
}

#[tokio::test]
async fn idle_selection_snapshot_shares_original_bodies_and_only_admits_slots() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    for _ in 0..8 {
        runtime.enqueue(message()).await?;
    }
    let mutation = runtime.begin_mutation().await?;
    let root = mutation.capture_published_root()?;
    let material = material(runtime)?;
    let before = material.finish()?;
    let (state, messages, _) = mutation.idle_selection_snapshot(&material)?;
    assert!(Arc::ptr_eq(&state, &root.state));
    assert_eq!(messages.len(), 8);
    for (selected, original) in messages.iter().zip(mutation.queue.shared_messages()) {
        assert!(Arc::ptr_eq(selected, original));
    }
    let after = material.finish()?;
    assert_eq!(after.1 - before.1, 8 * (messages.len() + 1));
    assert_eq!(
        after.0 - before.0,
        u64::try_from(
            8 * (std::mem::size_of::<Vec<Arc<MessageEnvelope>>>()
                + messages.len() * std::mem::size_of::<Arc<MessageEnvelope>>())
        )?
    );
    Ok(())
}

#[tokio::test]
async fn idle_queue_scope_reuse_preserves_original_qualification_and_scope_fence() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let message = message();
    runtime.enqueue(message.clone()).await?;
    let material = material(runtime)?;
    runtime
        .with_idle_read(None, &material, |read| {
            let before = material.finish()?;
            let selected = read
                .queue(&message.id)?
                .context("qualified Queue missing")?;
            assert_eq!(selected.status, QueueEntryStatus::Queued);
            let shared = material.finish()?;
            let selection = crate::runtime_db::current_query::CurrentSelection {
                queue_ids: BTreeSet::from([message.id.clone()]),
                events_after: None,
            };
            let _independent = crate::runtime_db::current_query::prepare(
                read.tx,
                &read.scope,
                Some(&selection),
                &runtime.inner.input_evidence_budget,
                (read.byte_limit, read.record_limit),
            )?;
            let independent = material.finish()?;
            assert!(shared.1 - before.1 < independent.1 - shared.1);
            read.tx.execute(
                "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
                [&read.scope.agent_id],
            )?;
            let error = read.queue(&message.id).unwrap_err();
            assert_eq!(
                error.to_string(),
                "context scope changed after its validated read"
            );
            Err::<(), _>(error)
        })
        .expect_err("changed scope must invalidate Queue preparation");
    Ok(())
}

#[tokio::test]
async fn idle_message_predicates_reuse_body_and_keep_owning_admission_and_scope() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut source = message();
    source.body = MessageBody::Text {
        text: "large qualified predicate body".repeat(4096),
    };
    runtime.enqueue(source.clone()).await?;
    let material = material(runtime)?;
    runtime
        .with_idle_read(None, &material, |read| {
            let original = read
                .with_message(&source.id, |message| {
                    assert_eq!(message.body, source.body);
                    Ok(message as *const MessageEnvelope as usize)
                })?
                .context("qualified Message missing")?;
            let prepared = material.finish()?;
            for _ in 0..8 {
                assert_eq!(
                    read.with_message(&source.id, |message| {
                        Ok(message as *const MessageEnvelope as usize)
                    })?,
                    Some(original)
                );
            }
            assert_eq!(material.finish()?, prepared);
            let phase = ResourceReadMaterial::with_test_limits(
                &runtime.inner.input_evidence_budget,
                &read.scope,
                1_000_000,
                64,
            )?;
            let header = phase.finish()?;
            {
                let _active = phase.enter();
                assert_eq!(
                    read.with_message(&source.id, |message| Ok(message.id == source.id))?,
                    Some(true)
                );
                assert_eq!(phase.finish()?, header);
                assert!(read.message(&source.id).is_err());
                for id in [source.id.as_str(), "unprepared"] {
                    let error = read.with_message(id, |_| Ok(())).unwrap_err();
                    assert_eq!(
                        error
                            .downcast_ref::<crate::runtime_error::RuntimeError>()
                            .unwrap()
                            .descriptor()
                            .code,
                        "context_resource_exhausted"
                    );
                }
            }
            assert!(phase.finish().is_err());
            read.tx.execute(
                "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
                [&read.scope.agent_id],
            )?;
            let error = read.with_message(&source.id, |_| Ok(())).unwrap_err();
            assert_eq!(
                error.to_string(),
                "context scope changed after its validated read"
            );
            Err::<(), _>(error)
        })
        .expect_err("changed scope must invalidate a cached Message predicate");
    Ok(())
}

#[tokio::test]
async fn idle_shared_message_keeps_original_body_and_capture_until_last_holder() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut source = message();
    source.body = MessageBody::Json {
        value: serde_json::json!({"items": vec![false; 2048]}),
    };
    runtime.enqueue(source.clone()).await?;
    let material = material(runtime)?;
    let selected = runtime.with_idle_read(None, &material, |read| {
        let original = read
            .with_message(&source.id, |message| Ok(message as *const MessageEnvelope))?
            .context("qualified Message missing")?;
        let phase = ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &read.scope,
            1_000_000,
            64,
        )?;
        let header = phase.finish()?;
        let selected = phase.read(|| {
            let selected = read
                .shared_message(&source.id)?
                .context("Message missing")?;
            assert_eq!(&*selected as *const MessageEnvelope, original);
            for _ in 0..8 {
                let again = read
                    .shared_message(&source.id)?
                    .context("Message missing")?;
                assert!(Arc::ptr_eq(&selected.fact, &again.fact));
                assert!(Arc::ptr_eq(&selected._capture, &again._capture));
            }
            Ok(selected)
        })?;
        assert_eq!(phase.finish()?, header);
        phase
            .read(|| read.message(&source.id))
            .expect_err("an owning copy must still be admitted");
        assert!(phase.read(|| read.shared_message(&source.id)).is_err());
        read.tx.execute(
            "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
            [&read.scope.agent_id],
        )?;
        assert_eq!(
            read.shared_message(&source.id).unwrap_err().to_string(),
            "context scope changed after its validated read"
        );
        Ok(selected)
    })?;
    let weak = Arc::downgrade(&selected._capture);
    let last = selected.clone();
    material.finish()?;
    drop(material);
    drop(selected);
    assert_eq!(last.body, source.body);
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn idle_shared_turn_retains_original_capture_and_owning_and_scope_fences() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let turn = TurnRecord::new("default", "idle-shared-turn", 1);
    let prepared = crate::runtime_db::prepared_input_evidence::PreparedInputEvidence::turn(
        scope,
        "idle-shared-turn-writer".into(),
        runtime.now(),
        turn.clone(),
    )?;
    runtime
        .runtime_db()
        .evidence()
        .commit_prepared_input_evidence(&prepared)?;
    let material = material(runtime)?;
    let selected = runtime.with_idle_read(None, &material, |read| {
        let selected = read.shared_turn(&turn.turn_id)?.context("Turn missing")?;
        let Fact::Turn(original) = &selected.fact.record else {
            bail!("Turn has another family");
        };
        assert_eq!(
            &*selected as *const TurnRecord,
            original as *const TurnRecord
        );
        let phase = ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &read.scope,
            1_000_000,
            64,
        )?;
        let header = phase.finish()?;
        phase.read(|| {
            for _ in 0..8 {
                let again = read.shared_turn(&turn.turn_id)?.context("Turn missing")?;
                assert!(Arc::ptr_eq(&selected.fact, &again.fact));
                assert!(Arc::ptr_eq(&selected._capture, &again._capture));
            }
            Ok(())
        })?;
        assert_eq!(phase.finish()?, header);
        phase
            .read(|| read.turn(&turn.turn_id))
            .expect_err("owning Turn copies must still be admitted");
        assert!(phase.read(|| read.shared_turn(&turn.turn_id)).is_err());
        read.tx.execute(
            "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
            [&read.scope.agent_id],
        )?;
        assert_eq!(
            read.shared_turn(&turn.turn_id).unwrap_err().to_string(),
            "context scope changed after its validated read"
        );
        Ok(selected)
    })?;
    let weak = Arc::downgrade(&selected._capture);
    let last = selected.clone();
    material.finish()?;
    drop((selected, material));
    assert_eq!(*last, turn);
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn idle_pending_preview_matches_prompt_reply_dedup_without_truncation() -> Result<()> {
    let harness = LifecycleHarness::new();
    let (_, mut pending) = pending_fixture(&harness).await?;
    let limit = crate::runtime_db::task_result_settlement::TASK_RESULT_SETTLEMENT_ADMISSION_LIMIT;
    let reply = message();
    let shared_reply = ContextHistoryMessage::owned(reply.clone())?;
    for entry in &mut pending {
        let mut message = (**entry.message.as_ref().unwrap()).clone();
        message.task_id = Some(entry.record.task_id.clone());
        message.metadata = Some(serde_json::json!({
            "task_id": entry.record.task_id,
            "task_kind": crate::types::AGENT_MESSAGE_WAIT_TASK_KIND,
            "task_status": "completed",
            "task_detail": {
                "reply_content_source": "original_message", "message_id": reply.id,
            },
        }));
        entry.message = Some(ContextHistoryMessage::owned(message)?);
        entry.reply = Some(shared_reply.clone());
    }
    for anchor in [message(), reply] {
        for count in [0, 1, 2, limit + 1] {
            let entries = pending
                .iter()
                .cycle()
                .take(count)
                .cloned()
                .enumerate()
                .map(|(index, mut entry)| {
                    entry.record.result_identity = format!("preview-result-{index}");
                    entry
                })
                .collect::<Vec<_>>();
            let previews = entries
                .iter()
                .map(|entry| {
                    crate::context::pending_task_result_preview(
                        &entry.record,
                        entry.message.as_deref(),
                        entry.reply.as_deref(),
                    )
                })
                .collect::<Vec<_>>();
            let rendered =
                crate::context::render_pending_task_results_from_snapshot(&entries, &anchor)
                    .unwrap_or_default();
            assert_eq!(
                crate::context::pending_task_results_preview_bytes(previews.iter(), &anchor),
                rendered.len()
            );
            assert_eq!(
                crate::context::pending_task_results_preview_measure(previews.iter(), &anchor),
                (rendered.len(), rendered.chars().count())
            );
            assert_eq!(rendered.matches("- result_identity:").count(), count);
            let expected_replies =
                usize::from(count > 0 && anchor.id != entries[0].reply.as_ref().unwrap().id);
            assert_eq!(
                rendered.matches("received_agent_message:").count(),
                expected_replies
            );
            assert!(!rendered.contains("omitted_results"));
            let mut duplicates = previews.iter().collect::<Vec<_>>();
            duplicates.extend(previews.iter());
            assert_eq!(
                crate::context::render_pending_task_result_previews(
                    duplicates.into_iter(),
                    &anchor
                )
                .unwrap_or_default(),
                rendered
            );
        }
    }
    Ok(())
}

async fn pending_fixture(
    harness: &LifecycleHarness,
) -> Result<(String, Vec<crate::context::PendingTaskResult>)> {
    pending_fixture_count(harness, 2).await
}

async fn pending_fixture_count(
    harness: &LifecycleHarness,
    count: usize,
) -> Result<(String, Vec<crate::context::PendingTaskResult>)> {
    use crate::types::{
        AdmissionContext, MessageDeliverySurface, TaskKind, TaskRecord, TaskStatus,
        WorkItemPlanStatus,
    };
    let runtime = harness.runtime();
    let owner = runtime
        .create_work_item(
            "idle read coverage".into(),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await?;
    let mut pending = Vec::new();
    for index in 0..count {
        let task_id = format!("idle-read-result-{index}");
        let mut message = MessageEnvelope::new(
            "default",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task_id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: format!("结果 {index}\n{}", "é".repeat(128)),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        message.created_at = harness.now();
        message.work_item_id = Some(owner.id.clone());
        message.metadata = Some(serde_json::json!({
            "task_id": task_id, "task_kind": "command_task", "task_status": "completed",
            "task_result_id": message.id, "work_item_id": owner.id,
        }));
        let task = TaskRecord {
            id: task_id.clone(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Completed,
            created_at: harness.now(),
            updated_at: harness.now(),
            parent_message_id: Some(message.id.clone()),
            work_item_id: Some(owner.id.clone()),
            summary: None,
            recovery: None,
            detail: Some(serde_json::json!({
                "rejoin_obligation_id": task_id, "rejoin_generation": 1,
                "parent_turn_id": "idle-read-parent", "terminal_reentry": true,
            })),
        };
        message.task_id = Some(task_id.clone());
        message
            .metadata
            .as_mut()
            .context("fixture metadata missing")?["task_detail"] =
            serde_json::to_value(&task.detail)?;
        runtime
            .commit_terminal_task_result(&task, "task_completed", &message)
            .await?;
        let record = runtime
            .runtime_db()
            .task_result_settlements()
            .latest_for_message(&message.id)?
            .context("fixture Result missing")?;
        let message = runtime
            .storage()
            .read_message_by_id(&message.id)?
            .context("fixture Message missing")?;
        pending.push(crate::context::PendingTaskResult {
            record,
            message: Some(ContextHistoryMessage::owned(message)?),
            reply: None,
        });
    }
    Ok((owner.id, pending))
}

#[tokio::test]
async fn idle_result_candidates_only_prepare_requested_originals() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, pending) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    runtime
        .with_idle_read(None, &material, |read| {
            assert!(read.task_reads.borrow().is_empty());
            let selected = read.qualify_result(&pending[0].record)?;
            assert_eq!(&*selected, &pending[0].record);
            let unselected = TaskKey::new(
                TaskFamily::ResultSettlement,
                &pending[1].record.result_identity,
            );
            assert!(!read.task_reads.borrow().contains_key(&unselected));
            assert!(selected._capture.lookup(&unselected).is_err());
            assert!(selected
                ._capture
                .lookup(&TaskKey::new(TaskFamily::Task, &pending[1].record.task_id))
                .is_err());
            assert!(!selected._capture.coverage.active_complete);
            assert!(!selected._capture.coverage.unsettled_complete);
            let mut forged = pending[1].record.clone();
            forged.task_id.push('!');
            let error = read.qualify_result(&forged).unwrap_err();
            assert_eq!(
                error.to_string(),
                "idle selected Result changed within read cut"
            );
            assert_eq!(
                &*read.qualify_result(&pending[1].record)?,
                &pending[1].record
            );
            read.tx.execute(
                "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
                [&read.scope.agent_id],
            )?;
            let error = read
                .same_turn_candidate_ids("idle-read-parent", harness.now())
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "context scope changed after its validated read"
            );
            Err::<(), _>(error)
        })
        .expect_err("changed scope must invalidate candidate discovery");
    Ok(())
}

#[tokio::test]
async fn live_result_discovery_only_materializes_consumed_originals() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, pending) = pending_fixture_count(&harness, 34).await?;
    let cutoff = harness.now();
    let expected = runtime
        .runtime_db()
        .task_result_settlements()
        .same_turn_candidates("default", "idle-read-parent", cutoff, 32)?;
    assert_eq!(expected.len(), 32);
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let before = material.finish()?;
        let ids = read.same_turn_candidate_ids("idle-read-parent", cutoff)?;
        let after = material.finish()?;
        assert_eq!(
            ids,
            expected
                .iter()
                .map(|record| record.result_identity.clone())
                .collect::<Vec<_>>()
        );
        // ResourceReadMaterial charges the conservative eightfold expansion.
        assert_eq!(after.1 - before.1, 32 * 8);
        assert_eq!(
            after.0 - before.0,
            ids.iter().map(|id| id.len() as u64 + 128).sum::<u64>() * 8
        );
        assert!(read.task_reads.borrow().is_empty());
        for (id, original) in ids.iter().zip(&expected).take(8) {
            let selected = read
                .same_turn_candidate(id, "idle-read-parent", cutoff)?
                .context("live candidate missing")?;
            assert_eq!(&*selected, original);
            assert!(!selected._capture.coverage.active_complete);
            assert!(!selected._capture.coverage.unsettled_complete);
            for unselected in &expected[8..] {
                assert!(selected
                    ._capture
                    .lookup(&TaskKey::new(
                        TaskFamily::ResultSettlement,
                        &unselected.result_identity
                    ))
                    .is_err());
                assert!(selected
                    ._capture
                    .lookup(&TaskKey::new(TaskFamily::Task, &unselected.task_id))
                    .is_err());
            }
        }
        for unselected in pending
            .iter()
            .filter(|entry| !ids[..8].contains(&entry.record.result_identity))
        {
            assert!(!read.task_reads.borrow().contains_key(&TaskKey::new(
                TaskFamily::ResultSettlement,
                &unselected.record.result_identity
            )));
        }
        assert!(read
            .same_turn_candidate("forged-result", "idle-read-parent", cutoff)?
            .is_none());
        assert!(read
            .same_turn_candidate(&ids[0], "another-parent", cutoff)?
            .is_none());
        assert!(read
            .same_turn_candidate(
                &ids[0],
                "idle-read-parent",
                expected[0].created_at - chrono::Duration::nanoseconds(1)
            )?
            .is_none());
        Ok(())
    })?;
    Ok(())
}

#[tokio::test]
async fn idle_result_source_keys_admit_one_bounded_set_before_allocation() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, pending) = pending_fixture_count(&harness, 9).await?;
    let selected = &pending[..8];
    let grouped_material = material(runtime)?;
    let grouped = runtime.with_idle_read(None, &grouped_material, |read| {
        let changes: u64 = read
            .tx
            .query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let before = grouped_material.finish()?;
        read.prepare_result_sources(
            selected
                .iter()
                .map(|entry| entry.record.result_identity.as_str()),
        )?;
        let after = grouped_material.finish()?;
        let first = read
            .record(&selected[0].record.result_identity)?
            .context("first selected Result missing")?;
        for entry in selected {
            let record = read
                .record(&entry.record.result_identity)?
                .context("selected Result missing")?;
            assert!(Arc::ptr_eq(&first._capture, &record._capture));
        }
        assert!(!read.task_reads.borrow().contains_key(&TaskKey::new(
            TaskFamily::ResultSettlement,
            &pending[8].record.result_identity,
        )));
        for _ in 0..INPUT_BATCH_SCAN {
            read.prepare_result_sources(
                selected
                    .iter()
                    .map(|entry| entry.record.result_identity.as_str()),
            )?;
            assert_eq!(grouped_material.finish()?, after);
        }
        let phase = ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &read.scope,
            1_000_000,
            64,
        )?;
        assert!(phase
            .read(|| read.prepare_result_sources(std::iter::once(
                pending[8].record.result_identity.as_str(),
            )))
            .is_err());
        assert!(!read.task_reads.borrow().contains_key(&TaskKey::new(
            TaskFamily::ResultSettlement,
            &pending[8].record.result_identity,
        )));
        assert_eq!(
            read.tx
                .query_row("SELECT total_changes()", [], |row| row.get::<_, u64>(0))?,
            changes
        );
        Ok((after.0 - before.0, after.1 - before.1))
    })?;
    let separate_material = material(runtime)?;
    let separate = runtime.with_idle_read(None, &separate_material, |read| {
        let before = separate_material.finish()?;
        for entry in selected {
            read.prepare_result_sources(std::iter::once(entry.record.result_identity.as_str()))?;
        }
        let after = separate_material.finish()?;
        Ok((after.0 - before.0, after.1 - before.1))
    })?;
    assert!(grouped.0 < separate.0, "{grouped:?} vs {separate:?}");
    assert!(grouped.1 < separate.1, "{grouped:?} vs {separate:?}");
    Ok(())
}

#[tokio::test]
async fn idle_recovery_predicates_share_work_and_one_task_capture() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, pending) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    runtime
        .with_idle_read(None, &material, |read| {
            let changes: i64 = read
                .tx
                .query_row("SELECT total_changes()", [], |row| row.get(0))?;
            read.prepare_result_sources(
                pending
                    .iter()
                    .map(|entry| entry.record.result_identity.as_str()),
            )?;
            let keys: Vec<_> = pending
                .iter()
                .map(|entry| {
                    TaskKey::new(TaskFamily::ResultSettlement, &entry.record.result_identity)
                })
                .collect();
            let tasks = read.tasks(keys[0].clone())?;
            assert!(Arc::ptr_eq(&tasks, &read.tasks(keys[1].clone())?));
            let work = read
                .with_work(&owner, |value| Ok(value as *const WorkItemRecord as usize))?
                .context("Work missing")?;
            read.validate_task_observation(tasks.revision)?;
            let usage = material.finish()?;
            for _ in 0..INPUT_BATCH_SCAN {
                read.prepare_result_sources(
                    pending
                        .iter()
                        .map(|entry| entry.record.result_identity.as_str()),
                )?;
                read.validate_task_observation(tasks.revision)?;
                assert_eq!(
                    read.with_work(&owner, |value| Ok(value as *const WorkItemRecord as usize))?,
                    Some(work)
                );
                for entry in &pending {
                    assert_eq!(
                        read.record(&entry.record.result_identity)?.as_deref(),
                        Some(&entry.record)
                    );
                }
            }
            assert_eq!(material.finish()?, usage);
            assert_eq!(
                read.tx
                    .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))?,
                changes
            );
            let phase = ResourceReadMaterial::with_test_limits(
                &runtime.inner.input_evidence_budget,
                &read.scope,
                1_000_000,
                64,
            )?;
            let header = phase.finish()?;
            {
                let _active = phase.enter();
                assert_eq!(read.with_work(&owner, |_| Ok(()))?, Some(()));
                read.validate_task_observation(tasks.revision)?;
                assert_eq!(phase.finish()?, header);
                assert!(read
                    .with_work(&owner, |value| {
                        resource_io::admit_cloned_value(value)?;
                        Ok(value.clone())
                    })
                    .is_err());
                assert!(read.with_work(&owner, |_| Ok(())).is_err());
            }
            assert!(phase.finish().is_err());
            read.tx.execute(
                "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
                [&read.scope.agent_id],
            )?;
            let error = read.validate_task_observation(tasks.revision).unwrap_err();
            assert_eq!(
                error.to_string(),
                "context scope changed after its validated read"
            );
            assert!(read
                .prepare_result_sources(
                    pending
                        .iter()
                        .map(|entry| entry.record.result_identity.as_str()),
                )
                .is_err());
            assert!(read.with_work(&owner, |_| Ok(())).is_err());
            Err::<(), _>(error)
        })
        .expect_err("changed scope must invalidate recovery predicates");
    Ok(())
}

#[tokio::test]
async fn idle_work_and_task_reads_do_not_write_published_coverage() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, pending) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let before: i64 = read
            .tx
            .query_row("SELECT total_changes()", [], |row| row.get(0))?;
        assert_eq!(
            read.with_work(&owner, |work| Ok(work.id == owner))?,
            Some(true)
        );
        assert_eq!(
            read.record(&pending[0].record.result_identity)?.as_deref(),
            Some(&pending[0].record)
        );
        assert!(!read.sender_has_wait("no-original-reply")?);
        assert_eq!(
            read.tx
                .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))?,
            before,
            "selection must not publish another coverage"
        );
        Ok(())
    })
}

#[tokio::test]
async fn idle_pending_capacity_trials_reuse_borrowed_projection() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, pending) = pending_fixture(&harness).await?;
    let records = pending
        .iter()
        .map(|entry| entry.record.clone())
        .collect::<Vec<_>>();
    let anchor = message();
    let expected = crate::context::render_pending_task_results_from_snapshot(&pending, &anchor)
        .context("pending render missing")?
        .len();
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        assert_eq!(read.pending_bytes(&anchor, &records)?, expected);
        let usage = material.finish()?;
        for _ in 0..INPUT_BATCH_SCAN {
            assert_eq!(read.pending_bytes(&anchor, &records)?, expected);
            assert_eq!(
                material.finish()?,
                usage,
                "trial must not copy or re-render source bodies"
            );
        }
        for record in &records {
            let original = read
                .record(&record.result_identity)?
                .context("Result missing")?;
            let previews = read.pending_previews.borrow();
            let cached = &previews[&record.result_identity].original;
            assert!(Arc::ptr_eq(&original.fact, &cached.fact));
            assert!(Arc::ptr_eq(&original._capture, &cached._capture));
        }
        let mut changed = records.clone();
        changed[0].task_status = "failed".into();
        assert_eq!(
            read.pending_bytes(&anchor, &changed)
                .unwrap_err()
                .to_string(),
            "idle pending Result changed within read cut"
        );
        Ok(())
    })
}

#[tokio::test]
async fn idle_pending_canonical_comparison_rejects_every_field_change() -> Result<()> {
    use crate::domain::execution_protocol::TaskRejoinSource;
    use crate::runtime_db::task_result_settlement::{
        TaskResultSettlementDisposition as Disposition, TaskResultSettlementState as State,
    };
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, pending) = pending_fixture(&harness).await?;
    let original = &pending[0].record;
    let changes: &[(&str, fn(&mut TaskResultSettlementRecord))] = &[
        ("result_identity", |r| r.result_identity.push('!')),
        ("agent_id", |r| r.agent_id.push('!')),
        ("task_id", |r| r.task_id.push('!')),
        ("message_id", |r| r.message_id.push('!')),
        ("work_item_id", |r| {
            r.work_item_id.get_or_insert_default().push('!')
        }),
        ("rejoin_generation", |r| r.rejoin_generation += 1),
        ("source", |r| match &mut r.source {
            TaskRejoinSource::ParentTurn { parent_turn_id } => parent_turn_id.push('!'),
            TaskRejoinSource::CommandCreation {
                command_creation_id,
            } => command_creation_id.push('!'),
        }),
        ("task_status", |r| r.task_status.push('!')),
        ("state", |r| {
            r.state = if r.state == State::PersistedPending {
                State::CallerAdmitted
            } else {
                State::PersistedPending
            }
        }),
        ("activation_id", |r| {
            r.activation_id.get_or_insert_default().push('!')
        }),
        ("disposition", |r| {
            r.disposition = if r.disposition.is_some() {
                None
            } else {
                Some(Disposition::ModelDelivered)
            }
        }),
        ("created_at", |r| {
            r.created_at += chrono::Duration::seconds(1)
        }),
        ("updated_at", |r| {
            r.updated_at += chrono::Duration::seconds(1)
        }),
        ("admitted_at", |r| {
            r.admitted_at =
                Some(r.admitted_at.unwrap_or(r.created_at) + chrono::Duration::seconds(1))
        }),
        ("settled_at", |r| {
            r.settled_at = Some(r.settled_at.unwrap_or(r.created_at) + chrono::Duration::seconds(1))
        }),
        ("deferred_reason", |r| {
            r.deferred_reason.get_or_insert_default().push('!')
        }),
        ("deferred_at", |r| {
            r.deferred_at =
                Some(r.deferred_at.unwrap_or(r.created_at) + chrono::Duration::seconds(1))
        }),
        ("next_recheck_at", |r| {
            r.next_recheck_at =
                Some(r.next_recheck_at.unwrap_or(r.created_at) + chrono::Duration::seconds(1))
        }),
        ("recovery_epoch", |r| r.recovery_epoch += 1),
        ("recovery_attempts", |r| r.recovery_attempts += 1),
        ("last_recovery_root_attempt_id", |r| {
            r.last_recovery_root_attempt_id
                .get_or_insert_default()
                .push('!')
        }),
        ("last_recovery_admitted_at", |r| {
            r.last_recovery_admitted_at = Some(
                r.last_recovery_admitted_at.unwrap_or(r.created_at) + chrono::Duration::seconds(1),
            )
        }),
        ("recovery_paused_at", |r| {
            r.recovery_paused_at =
                Some(r.recovery_paused_at.unwrap_or(r.created_at) + chrono::Duration::seconds(1))
        }),
        ("recovery_diagnostic_at", |r| {
            r.recovery_diagnostic_at = Some(
                r.recovery_diagnostic_at.unwrap_or(r.created_at) + chrono::Duration::seconds(1),
            )
        }),
    ];
    assert!(same_pending_result(original, original));
    for (field, mutate) in changes {
        let mut changed = original.clone();
        mutate(&mut changed);
        assert!(
            !same_pending_result(original, &changed),
            "accepted changed {field}"
        );
    }
    let mut presentation = original.clone();
    presentation.source_projection = Some(IdleInputBatchItem {
        message_id: "trial-source".into(),
        source_ref: "message:trial-source".into(),
        rendered: "trial presentation".into(),
    });
    presentation.reply_projection = presentation.source_projection.clone();
    assert!(same_pending_result(original, &presentation));
    let anchor = message();
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        for (field, mutate) in changes {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(
                read.pending_measure_with_projected_candidate(
                    &anchor,
                    &[] as &[TaskResultSettlementRecord],
                    &changed,
                    presentation.source_projection.as_ref().unwrap(),
                    presentation.reply_projection.as_ref(),
                )
                .is_err(),
                "projection trial accepted changed {field}"
            );
        }
        Ok(())
    })
}

#[tokio::test]
async fn idle_projection_trials_borrow_candidates_without_retaining_trial_projections() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, pending) = pending_fixture(&harness).await?;
    let candidate = &pending[0].record;
    let mut prior = pending[1].record.clone();
    let reply = IdleInputBatchItem {
        message_id: "shared-trial-reply".into(),
        source_ref: "message:shared-trial-reply".into(),
        rendered: "共享回复 é\n".into(),
    };
    prior.source_projection = Some(IdleInputBatchItem {
        message_id: prior.message_id.clone(),
        source_ref: format!("message:{}", prior.message_id),
        rendered: "already selected\n".into(),
    });
    prior.reply_projection = Some(reply.clone());
    let records = [prior.clone(), prior];
    let mut source = IdleInputBatchItem {
        message_id: candidate.message_id.clone(),
        source_ref: format!("message:{}", candidate.message_id),
        rendered: String::new(),
    };
    let anchor = message();
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        read.pending_measure(&anchor, &records)?;
        // Warm only the canonical carrier/facts, never a candidate preview.
        read.pending_measure_with_projected_candidate(
            &anchor,
            &records,
            candidate,
            &source,
            Some(&reply),
        )?;
        for rendered in ["", "é", "你好\n", "changed presentation é\n"] {
            source.rendered = rendered.into();
            for trial_reply in [None, Some(&reply), Some(&source)] {
                let mut owned = candidate.clone();
                owned.source_projection = Some(source.clone());
                owned.reply_projection = trial_reply.cloned();
                let expected = records
                    .iter()
                    .chain(std::iter::once(&owned))
                    .map(|record| crate::context::pending_task_result_preview(record, None, None))
                    .collect::<Vec<_>>();
                let text =
                    crate::context::render_pending_task_result_previews(expected.iter(), &anchor)
                        .context("trial render missing")?;
                let before = material.finish()?;
                let measured = read.pending_measure_with_projected_candidate(
                    &anchor,
                    &records,
                    candidate,
                    &source,
                    trial_reply,
                )?;
                assert_eq!(
                    measured,
                    (
                        text.len(),
                        crate::context::estimate_text_tokens(&format!(
                            "[pending_task_results]\n{text}"
                        )),
                    )
                );
                let after = material.finish()?;
                assert_eq!(
                    after, before,
                    "projection trials borrow all fragments without allocating a preview",
                );
                let text_bytes = crate::runtime::input_evidence_budget::serialized_bytes(&(
                    candidate,
                    Some(&source),
                    trial_reply,
                    None::<&MessageEnvelope>,
                    None::<&MessageEnvelope>,
                ))? * 4;
                let cells = crate::context::PENDING_TASK_RESULT_PREVIEW_LINES + 16;
                let metadata_bytes = u64::try_from(
                    std::mem::size_of::<crate::context::PendingTaskResultPreview>()
                        + cells * std::mem::size_of::<String>(),
                )?;
                let preview_bytes = 8 * (text_bytes + metadata_bytes);
                let preview_records = 8 * (cells + 1);
                if rendered.is_empty() && trial_reply.is_none() {
                    let phase = ResourceReadMaterial::with_test_limits(
                        &runtime.inner.input_evidence_budget,
                        &read.scope,
                        1_000_000,
                        64,
                    )?;
                    let header = phase.finish()?;
                    drop(phase);
                    for short in [0, 1] {
                        let phase = ResourceReadMaterial::with_test_limits(
                            &runtime.inner.input_evidence_budget,
                            &read.scope,
                            header.0 + preview_bytes - short,
                            header.1 + preview_records,
                        )?;
                        let _active = phase.enter();
                        let trial = read.pending_preview_with_projections(
                            candidate,
                            Some(&source),
                            trial_reply,
                        );
                        if short == 0 {
                            let trial = trial?;
                            assert_eq!(
                                crate::context::render_pending_task_result_previews(
                                    std::iter::once(&trial),
                                    &anchor,
                                ),
                                crate::context::render_pending_task_result_previews(
                                    std::iter::once(
                                        expected.last().context("expected candidate missing")?,
                                    ),
                                    &anchor,
                                ),
                            );
                            assert_eq!(phase.finish()?.0, header.0 + preview_bytes);
                        } else {
                            assert!(trial.is_err(), "one byte short must reject the preview");
                            assert!(phase.finish().is_err(), "rejection must remain latched");
                            assert!(read
                                .pending_preview_with_projections(
                                    candidate,
                                    Some(&source),
                                    trial_reply,
                                )
                                .is_err());
                        }
                    }
                }
                assert!(!read
                    .pending_previews
                    .borrow()
                    .contains_key(&candidate.result_identity));
                assert!(read
                    .record(&candidate.result_identity)?
                    .context("canonical candidate missing")?
                    .source_projection
                    .is_none());
            }
        }
        Ok(())
    })
}

#[test]
fn idle_pending_projection_revision_binds_both_complete_presentations() -> Result<()> {
    let source = IdleInputBatchItem {
        message_id: "source".into(),
        source_ref: "message:source".into(),
        rendered: "共享 é\n{\"body\":\"[]\"}".repeat(1024),
    };
    let mut reply = source.clone();
    reply.message_id = "reply".into();
    let digest = pending_projections_digest(Some(&source), Some(&reply))?;
    assert_eq!(
        digest,
        pending_projections_digest(Some(&source.clone()), Some(&reply.clone()))?
    );
    for field in 0..3 {
        let mut changed = reply.clone();
        match field {
            0 => changed.message_id.push('x'),
            1 => changed.source_ref.push('x'),
            _ => changed.rendered.push('界'),
        }
        assert_ne!(
            digest,
            pending_projections_digest(Some(&source), Some(&changed))?
        );
    }
    assert_ne!(
        digest,
        pending_projections_digest(Some(&reply), Some(&source))?
    );
    assert_ne!(
        pending_projections_digest(None, Some(&source))?,
        pending_projections_digest(Some(&source), None)?
    );
    Ok(())
}

#[tokio::test]
async fn idle_pending_text_reuses_previews_and_requalifies_changed_projections() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, pending) = pending_fixture(&harness).await?;
    let records = pending
        .iter()
        .map(|entry| entry.record.clone())
        .collect::<Vec<_>>();
    let anchor = message();
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let expected = read.pending_text(&anchor, &records)?;
        let expected_tokens = expected.as_ref().map_or(0, |text| {
            crate::context::estimate_text_tokens(&format!("[pending_task_results]\n{text}"))
        });
        let expected_measure = (expected.as_ref().map_or(0, String::len), expected_tokens);
        assert_eq!(read.pending_measure(&anchor, &records)?, expected_measure);
        let current = "current é\n你好";
        let budget = projection::InputBudget {
            tokens: crate::context::estimate_text_tokens(&format!("[current_input]\n{current}"))
                + expected_tokens,
            bytes: usize::MAX,
        };
        for tokens in [budget.tokens - 1, budget.tokens, budget.tokens + 1] {
            let budget = projection::InputBudget { tokens, ..budget };
            assert_eq!(
                budget.fits_idle_tokens(current, expected_tokens),
                budget.fits_idle(current, expected.as_deref())
            );
        }
        for _ in 0..INPUT_BATCH_SCAN {
            let before = material.finish()?;
            assert_eq!(read.pending_measure(&anchor, &records)?, expected_measure);
            assert_eq!(
                material.finish()?,
                before,
                "byte/token trials allocate neither aggregate text nor dedup containers"
            );
            assert_eq!(read.pending_text(&anchor, &records)?, expected);
            let after = material.finish()?;
            assert_eq!(
                after.1 - before.1,
                8 * 7 * records.len(),
                "only aggregate references and formatter dedup slots are new"
            );
            assert_eq!(
                after.0 - before.0,
                8 * u64::try_from(
                    records.len()
                        * (std::mem::size_of::<&crate::context::PendingTaskResultPreview>()
                            + 6 * 64)
                        + expected.as_ref().map_or(0, String::len),
                )?,
                "only aggregate text, references and formatter dedup slots are new"
            );
        }
        let mut changed = records.clone();
        let source = pending[0]
            .message
            .as_deref()
            .context("fixture source missing")?;
        changed[0].source_projection = Some(
            super::super::projection::project_item(source, usize::MAX, usize::MAX)?
                .context("fixture projection missing")?,
        );
        let projected = read.pending_text(&anchor, &changed)?;
        changed[0]
            .source_projection
            .as_mut()
            .unwrap()
            .rendered
            .push_str("changed projection");
        let before = material.finish()?;
        let revised = read.pending_text(&anchor, &changed)?;
        assert_ne!(
            revised, projected,
            "a changed projection must invalidate the preview"
        );
        let after = material.finish()?;
        assert!(after.1 - before.1 > 8 * 7 * records.len());
        assert!(revised.unwrap().contains("changed projection"));
        changed[0].task_status = "unqualified-status".into();
        assert_eq!(
            read.pending_text(&anchor, &changed)
                .unwrap_err()
                .to_string(),
            "idle pending Result changed within read cut"
        );
        Ok(())
    })
}

#[tokio::test]
async fn idle_named_input_read_reuses_scope_and_body_without_recharging() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let message = message();
    runtime.enqueue(message.clone()).await?;
    let material = material(runtime)?;
    runtime
        .with_idle_read(None, &material, |read| {
            let mut request = read.input_request();
            request.insert_admitted_root(Family::Message, &message.id)?;
            let first = read.inputs(&request)?;
            let usage = material.finish()?;
            for _ in 0..INPUT_BATCH_SCAN {
                let next = read.inputs(&request)?;
                assert!(Arc::ptr_eq(&first, &next));
                assert_eq!(material.finish()?, usage);
            }
            assert!(first
                .lookup(&Key::new(Family::Message, &message.id))?
                .is_some());
            read.tx.execute(
                "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
                [&read.scope.agent_id],
            )?;
            let error = read.inputs(&request).unwrap_err();
            assert_eq!(
                error.to_string(),
                "context scope changed after its validated read"
            );
            // Roll back the fixture's intentionally stale scope.
            Err::<(), _>(error)
        })
        .expect_err("changed scope must invalidate cached reads");
    Ok(())
}

#[tokio::test]
async fn queue_head_starvation_uses_qualified_queue_age_and_rate_limit() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let message = message();
    runtime.enqueue(message.clone()).await?;
    let material = material(runtime)?;
    let entry = runtime
        .with_idle_read(None, &material, |read| read.queue(&message.id))?
        .context("qualified Queue missing")?;
    runtime.inner.storage.append_event(&AuditEvent::legacy(
        "scheduling_advisory",
        serde_json::json!({
            "kind": "unrelated_advisory",
            "evidence": ["message_id=another-head"],
            "details": vec![false; 65_537],
        }),
    ))?;
    let full_history = self::material(runtime)?;
    assert!(full_history
        .read(|| runtime.inner.storage.read_recent_events(64))
        .is_err());
    let executor = crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(runtime);
    let count = || -> Result<usize> {
        Ok(runtime
            .inner
            .storage
            .read_recent_events(64)?
            .iter()
            .filter(|event| {
                event.kind == "scheduling_advisory" && event.data["kind"] == "queue_head_starved"
            })
            .count())
    };
    harness
        .clock()
        .advance(std::time::Duration::from_millis(10 * 60 * 1000 - 1));
    material.read(|| executor.append_queue_head_starved_advisory_if_due(&entry, 1))?;
    assert_eq!(count()?, 0);
    harness.clock().advance(std::time::Duration::from_millis(1));
    material.read(|| executor.append_queue_head_starved_advisory_if_due(&entry, 1))?;
    assert_eq!(count()?, 1);
    material.read(|| executor.append_queue_head_starved_advisory_if_due(&entry, 1))?;
    assert_eq!(count()?, 1);
    Ok(())
}

fn material(runtime: &RuntimeHandle) -> Result<ResourceReadMaterial> {
    ResourceReadMaterial::new(
        &runtime.inner.input_evidence_budget,
        &crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: "default".into(),
            identity_incarnation: runtime.inner.identity_incarnation,
        },
    )
}

#[tokio::test]
async fn idle_result_addresses_share_one_qualified_capture() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, pending) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let changes: u64 = read
            .tx
            .query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let selected = read.unsettled(Some(&owner))?;
        assert_eq!(selected.len(), pending.len());
        let keys = pending
            .iter()
            .map(|entry| TaskKey::new(TaskFamily::ResultSettlement, &entry.record.result_identity))
            .collect::<Vec<_>>();
        let first = read.tasks(keys[0].clone())?;
        let usage = material.finish()?;
        for _ in 0..INPUT_BATCH_SCAN {
            for key in &keys {
                let captured = read.tasks(key.clone())?;
                assert!(Arc::ptr_eq(&first, &captured));
                let selected = read.record(&key.id)?.context("Result missing")?;
                assert!(Arc::ptr_eq(
                    &selected.fact,
                    captured.lookup(key)?.context("qualified Result missing")?,
                ));
                assert!(Arc::ptr_eq(&first, &selected._capture));
            }
            assert_eq!(material.finish()?, usage);
        }
        let after: u64 = read
            .tx
            .query_row("SELECT total_changes()", [], |row| row.get(0))?;
        assert_eq!(changes, after);
        Ok(())
    })
}

#[tokio::test]
async fn idle_source_task_loan_reuses_original_capture_at_exact_material_limits() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, _) = pending_fixture(&harness).await?;
    let measure = |material: &ResourceReadMaterial| -> Result<_> {
        runtime.with_idle_read(None, material, |read| {
            let records = read.unsettled(Some(&owner))?;
            let record = &records[0];
            read.shared_message(&record.message_id)?
                .context("Message missing")?;
            let before = material.finish()?;
            for _ in 0..8 {
                read.with_source_resolver(|resolver| {
                    let source = read.source_read.with_resolver(resolver)?;
                    // The Task is checked before this non-reply Message rejects the reply gate.
                    assert!(!source.reply_source_consumed(record)?);
                    Ok(())
                })?;
                assert_eq!(material.finish()?, before, "no second owning Task decode");
            }
            Ok(())
        })?;
        material.finish()
    };
    let measured = measure(&material(runtime)?)?;
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    assert_eq!(
        measure(&ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &scope,
            measured.0,
            measured.1,
        )?)?,
        measured,
    );
    for short in [
        ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &scope,
            measured.0 - 1,
            measured.1,
        )?,
        ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &scope,
            measured.0,
            measured.1 - 1,
        )?,
    ] {
        let error = measure(&short).unwrap_err();
        assert_eq!(
            crate::runtime_error::describe_runtime_error(&error).code,
            "context_resource_exhausted",
        );
        assert!(short.finish().is_err());
        assert!(measure(&short).is_err(), "resource failure remains latched");
    }
    Ok(())
}

#[tokio::test]
async fn idle_source_task_loan_checks_raw_sql_cut_and_actual_phase() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, _) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let records = read.unsettled(Some(&owner))?;
        let record = &records[0];
        read.shared_message(&record.message_id)?.context("Message missing")?;
        let capture = read.tasks(TaskKey::new(TaskFamily::ResultSettlement, &record.result_identity))?;
        let fact = capture.lookup(&TaskKey::new(TaskFamily::Task, &record.task_id))?
            .context("qualified Task missing")?;
        let matches = || crate::runtime_db::task_snapshot::qualified_task_source_matches_tx(
            read.tx, &read.scope, fact,
        );
        assert!(matches()?);
        {
            let mut connection = rusqlite::Connection::open_in_memory()?;
            let other_cut = connection.transaction()?;
            let resolve = |_family, _id: &str| Ok(None);
            let resolve_task = |_id: &str| Ok(Some(results::TaskResultTaskLoan::new(
                &other_cut, fact.clone(), capture.clone(), material.clone(),
            )));
            let resolver = results::TaskResultSourceResolver::with_scope_read(
                read.tx, &read.scope, &runtime.inner.input_evidence_budget,
                &read.scope_read, &resolve,
            )?.with_task_source(&resolve_task);
            assert!(read.source_read.with_resolver(&resolver)?
                .reply_source_consumed(record).is_err());
        }
        let other_phase = ResourceReadMaterial::new(&runtime.inner.input_evidence_budget, &read.scope)?;
        let error = other_phase.read(|| read.with_source_resolver(|resolver| {
            read.source_read.with_resolver(resolver)?.reply_source_consumed(record)
        })).unwrap_err();
        assert!(error.to_string().contains("resource phase"));
        assert!(matches()?);
        // SQL-only fields are part of the complete qualification witness too.
        let correlation: Option<String> = read.tx.query_row(
            "SELECT correlation_id FROM tasks WHERE task_id=?1", [&record.task_id],
            |row| row.get(0),
        )?;
        read.tx.execute("UPDATE tasks SET correlation_id='changed' WHERE task_id=?1", [&record.task_id])?;
        assert!(!matches()?);
        read.tx.execute(
            "UPDATE tasks SET correlation_id=?2 WHERE task_id=?1",
            params![record.task_id, correlation],
        )?;
        assert!(matches()?);
        read.tx.execute(
            "UPDATE tasks SET payload_json=json_set(payload_json,'$.summary','changed') WHERE task_id=?1",
            [&record.task_id],
        )?;
        assert!(!matches()?);
        Ok(())
    })
}

#[tokio::test]
async fn idle_source_reader_preserves_canonical_task_and_scope_checks() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, pending) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let records = read.unsettled(Some(&owner))?;
        assert_eq!(records.len(), pending.len());
        read.prepare_result_sources(records.iter().map(|record| record.result_identity.as_str()))?;
        let record = &records[0];
        assert!(read.source_matches(record, false)?);
        assert!(!read.source_matches(record, true)?);
        let mut forged = (**record).clone();
        forged.agent_id = "another-agent".into();
        assert_eq!(
            read.source_matches(&forged, false).unwrap_err().to_string(),
            "task result source reader belongs to another read cut"
        );
        let phase = ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &read.scope,
            1_000_000,
            64,
        )?;
        phase
            .read(|| {
                resource_io::charge_owned_metadata(0, 65)?;
                Ok(())
            })
            .expect_err("test phase must latch the original resource failure");
        assert!(phase.read(|| read.source_matches(record, false)).is_err());
        // A qualified Result is not a cached verdict about its durable Task.
        assert_eq!(
            read.tx.execute(
                "UPDATE tasks SET payload_json=json_set(payload_json,'$.status','running')
                 WHERE task_id=?1",
                [&record.task_id],
            )?,
            1
        );
        assert!(!read.source_matches(record, false)?);
        read.tx.execute(
            "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
            [&read.scope.agent_id],
        )?;
        assert_eq!(
            read.source_matches(record, false).unwrap_err().to_string(),
            "context scope changed after its validated read"
        );
        Ok(())
    })
}

#[tokio::test]
async fn idle_reply_messages_share_one_capture_without_expanding_selection() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, _) = pending_fixture_count(&harness, 9).await?;
    let grouped_material = material(runtime)?;
    let grouped = runtime.with_idle_read(None, &grouped_material, |read| {
        let records = read.unsettled(Some(&owner))?;
        assert_eq!(records.len(), 9);
        let selected = &records[..8];
        let changes: u64 = read
            .tx
            .query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let before = grouped_material.finish()?;
        read.prepare_reply_source_work(selected)?;
        let after = grouped_material.finish()?;
        assert_eq!(read.input_reads.borrow().len(), 1);
        let first = read
            .shared_message(&selected[0].message_id)?
            .context("first selected Message missing")?;
        for record in selected {
            let message = read
                .shared_message(&record.message_id)?
                .context("selected Message missing")?;
            assert!(Arc::ptr_eq(&first._capture, &message._capture));
            assert_eq!(message.id, record.message_id);
        }
        assert!(!first._capture.records.iter().any(|row| {
            row.locator.key.family == Family::Message && row.locator.key.id == records[8].message_id
        }));
        for _ in 0..8 {
            read.prepare_reply_source_work(selected)?;
            read.prepare_reply_source_work_records(selected.iter().map(|record| &**record))?;
            assert_eq!(grouped_material.finish()?, after);
        }
        assert_eq!(
            read.tx
                .query_row("SELECT total_changes()", [], |row| row.get::<_, u64>(0))?,
            changes
        );
        Ok((after.0 - before.0, after.1 - before.1))
    })?;
    let separate_material = material(runtime)?;
    let separate = runtime.with_idle_read(None, &separate_material, |read| {
        let records = read.unsettled(Some(&owner))?;
        assert_eq!(records.len(), 9);
        let before = separate_material.finish()?;
        for record in &records[..8] {
            read.shared_message(&record.message_id)?
                .context("separately selected Message missing")?;
        }
        let after = separate_material.finish()?;
        Ok((after.0 - before.0, after.1 - before.1))
    })?;
    assert!(grouped.0 < separate.0, "{grouped:?} vs {separate:?}");
    assert!(grouped.1 < separate.1, "{grouped:?} vs {separate:?}");
    runtime
        .with_idle_read(None, &material(runtime)?, |read| {
            let records = read.unsettled(Some(&owner))?;
            let selected = &records[..8];
            read.prepare_reply_source_work(selected)?;
            let failed = ResourceReadMaterial::with_test_limits(
                &runtime.inner.input_evidence_budget,
                &read.scope,
                1_000_000,
                64,
            )?;
            failed
                .read(|| {
                    resource_io::charge_owned_metadata(0, 65)?;
                    Ok(())
                })
                .expect_err("test phase must exhaust its record budget");
            assert!(failed
                .read(|| {
                    read.prepare_reply_source_work_records(selected.iter().map(|record| &**record))
                })
                .is_err());
            read.tx.execute(
                "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
                [&read.scope.agent_id],
            )?;
            let error = read.prepare_reply_source_work(selected).unwrap_err();
            assert_eq!(
                error.to_string(),
                "context scope changed after its validated read"
            );
            Err::<(), _>(error)
        })
        .expect_err("changed scope must invalidate the batch and roll back");
    Ok(())
}

#[tokio::test]
async fn idle_reply_work_batch_preserves_exact_relations_and_scope() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, pending) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let records = read.unsettled(Some(&owner))?;
        let reply = &pending[0].record.message_id;
        let capture = |tasks: BTreeSet<String>| {
            let mut capture = WorkCaptureRequest::current();
            capture.include_live = false;
            capture.nonopen_limit = 0;
            capture.required_task_ids = tasks;
            capture.required_trigger_message_ids.insert(reply.clone());
            capture
        };
        let tasks: BTreeSet<_> = records
            .iter()
            .map(|record| record.task_id.clone())
            .collect();
        let changes: u64 = read
            .tx
            .query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let before = material.finish()?;
        read.prepare_reply_work(capture(tasks.clone()))?;
        let grouped = material.finish()?;
        for _ in 0..INPUT_BATCH_SCAN {
            for task in &tasks {
                assert!(read.reply_work_covers(task, reply)?);
            }
            assert!(!read.reply_work_covers("unselected-task", reply)?);
            assert!(!read.reply_work_covers(tasks.first().unwrap(), "unselected-reply")?);
            assert_eq!(material.finish()?, grouped);
        }
        for task in &tasks {
            read.prepare_reply_work(capture(BTreeSet::from([task.clone()])))?;
        }
        let separate = material.finish()?;
        assert!(grouped.1 - before.1 < separate.1 - grouped.1);
        assert!(grouped.0 - before.0 < separate.0 - grouped.0);
        assert_eq!(
            read.tx
                .query_row("SELECT total_changes()", [], |row| row.get::<_, u64>(0))?,
            changes
        );
        assert!(!read.reply_work_covers(tasks.first().unwrap(), reply)?);
        assert!(read.reply_work_covers(tasks.last().unwrap(), reply)?);
        let failed = ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &read.scope,
            1_000_000,
            64,
        )?;
        failed
            .read(|| {
                resource_io::charge_owned_metadata(0, 65)?;
                Ok(())
            })
            .expect_err("test phase must exhaust its record budget");
        assert!(failed
            .read(|| read.reply_work_covers(tasks.last().unwrap(), reply))
            .is_err());
        read.tx.execute(
            "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
            [&read.scope.agent_id],
        )?;
        assert_eq!(
            read.reply_work_covers(tasks.last().unwrap(), reply)
                .unwrap_err()
                .to_string(),
            "context scope changed after its validated read"
        );
        Ok(())
    })
}

#[tokio::test]
async fn idle_recovery_filter_reuses_qualified_results_and_charges_only_slots() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, pending) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let records = read.unsettled(Some(&owner))?;
        assert_eq!(records.len(), pending.len());
        read.prepare_result_sources(records.iter().map(|record| record.result_identity.as_str()))?;
        let changes: u64 = read
            .tx
            .query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let snapshot = read.recovery_snapshot("normal-idle-input")?;
        for _ in 0..INPUT_BATCH_SCAN {
            let before = material.finish()?;
            let selected = read.filter_recovery_candidates(&snapshot, &records, harness.now())?;
            assert_eq!(selected.len(), records.len());
            for (selected, original) in selected.iter().zip(&records) {
                assert_eq!(&**selected, &**original);
                assert!(Arc::ptr_eq(&selected.fact, &original.fact));
                assert!(Arc::ptr_eq(&selected._capture, &original._capture));
            }
            let after = material.finish()?;
            assert_eq!(after.1 - before.1, 8 * records.len() * 2);
            assert_eq!(
                after.0 - before.0,
                u64::try_from(
                    8 * records.len()
                        * (std::mem::size_of::<std::borrow::Cow<'_, TaskResultSettlementRecord>>()
                            + std::mem::size_of::<QualifiedResultRead>())
                )?
            );
        }
        assert_eq!(
            read.tx
                .query_row("SELECT total_changes()", [], |row| row.get::<_, u64>(0))?,
            changes
        );
        let mut changed = (*records[0]).clone();
        changed.task_status = "forged".into();
        assert!(read.qualify_result(&changed).is_err());
        let phase = ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &read.scope,
            1_000_000,
            64 + 8 * records.len() * 2,
        )?;
        assert_eq!(phase.finish()?.1, 64, "original phase header");
        {
            let _active = phase.enter();
            let selected = read.filter_recovery_candidates(&snapshot, &records, harness.now())?;
            assert_eq!(selected.len(), records.len());
            assert_eq!(phase.finish()?.1, 64 + 8 * records.len() * 2);
            assert!(resource_io::admit_cloned_value(&*records[0]).is_err());
        }
        assert!(
            phase.finish().is_err(),
            "owned copies must still exhaust the original limit"
        );
        Ok(())
    })
}

#[tokio::test]
async fn idle_pending_selected_views_borrow_canonical_results_and_projection_sidecars() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, pending) = pending_fixture(&harness).await?;
    let anchor = message();
    let reply_id = "selected-shared-reply".to_owned();
    let mut projections = BTreeMap::new();
    let mut selected = BTreeSet::new();
    let mut reply_sources = BTreeMap::new();
    let mut owning = Vec::new();
    for entry in &pending {
        let record = &entry.record;
        selected.insert(record.result_identity.clone());
        reply_sources.insert(record.result_identity.clone(), reply_id.clone());
        projections.insert(
            record.message_id.clone(),
            IdleInputBatchItem {
                message_id: record.message_id.clone(),
                source_ref: format!("message:{}", record.message_id),
                rendered: format!("selected {} é\n", record.result_identity),
            },
        );
    }
    projections.insert(
        reply_id.clone(),
        IdleInputBatchItem {
            message_id: reply_id.clone(),
            source_ref: format!("message:{reply_id}"),
            rendered: "共享回复 é\n".into(),
        },
    );
    for entry in &pending {
        let mut record = entry.record.clone();
        record.source_projection = projections.get(&record.message_id).cloned();
        record.reply_projection = projections.get(&reply_id).cloned();
        owning.push(record);
    }
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let records = read.unsettled(Some(&owner))?;
        assert_eq!(records.len(), 2);
        let expected = read.pending_measure_views(
            &anchor,
            records.iter().map(|record| {
                owning
                    .iter()
                    .find(|expected| expected.result_identity == record.result_identity)
                    .expect("owning comparison missing")
                    .into()
            }),
        )?;
        let before = material.finish()?;
        for _ in 0..INPUT_BATCH_SCAN {
            let views = records.iter().map(|record| {
                let view = selected_pending_result(record, &selected, &projections, &reply_sources);
                assert!(std::ptr::eq(view.record, &**record));
                assert!(std::ptr::eq(
                    view.source_projection.unwrap(),
                    &projections[&record.message_id],
                ));
                assert!(std::ptr::eq(
                    view.reply_projection.unwrap(),
                    &projections[&reply_id],
                ));
                view
            });
            assert_eq!(read.pending_measure_views(&anchor, views)?, expected);
            assert_eq!(
                material.finish()?,
                before,
                "selected sidecars must not clone canonical Results or projection bodies",
            );
        }
        for record in &records {
            assert!(record.source_projection.is_none());
            assert!(record.reply_projection.is_none());
            let unselected =
                selected_pending_result(record, &BTreeSet::new(), &projections, &reply_sources);
            assert!(unselected.source_projection.is_none());
            assert!(unselected.reply_projection.is_none());
            let mut legacy = (**record).clone();
            legacy.state = results::TaskResultSettlementState::CallerAdmitted;
            let view = selected_pending_result(&legacy, &selected, &projections, &reply_sources);
            assert!(
                view.source_projection.is_none(),
                "legacy None must remain None"
            );
            assert!(
                view.reply_projection.is_none(),
                "legacy None must remain None"
            );
            legacy.source_projection = owning[0].source_projection.clone();
            let view = selected_pending_result(&legacy, &selected, &projections, &reply_sources);
            assert!(std::ptr::eq(
                view.source_projection.unwrap(),
                legacy.source_projection.as_ref().unwrap(),
            ));
        }
        let phase = ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &read.scope,
            1_000_000,
            64,
        )?;
        let _active = phase.enter();
        assert!(resource_io::charge_owned_metadata(0, usize::MAX).is_err());
        assert!(
            read.pending_measure_views(&anchor, std::iter::empty())
                .is_err(),
            "empty borrowed trials must preserve the failure latch"
        );
        assert!(
            read.pending_measure_with_projected_candidate(
                &anchor,
                &[] as &[TaskResultSettlementRecord],
                &records[0],
                &projections[&records[0].message_id],
                projections.get(&reply_id),
            )
            .is_err(),
            "allocation-free projected trials must preserve the failure latch"
        );
        assert!(phase.finish().is_err());
        Ok(())
    })
}

#[tokio::test]
async fn idle_pending_capacity_borrows_admitted_and_one_candidate() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (owner, _) = pending_fixture(&harness).await?;
    let anchor = message();
    let material = material(runtime)?;
    runtime.with_idle_read(None, &material, |read| {
        let records = read.unsettled(Some(&owner))?;
        assert_eq!(records.len(), 2);
        let expected = read.pending_measure(&anchor, &records)?;
        let usage = material.finish()?;
        for _ in 0..INPUT_BATCH_SCAN {
            assert_eq!(
                read.pending_measure_with_candidate(&anchor, &records[..1], Some(&records[1]))?,
                expected
            );
            assert_eq!(
                read.pending_measure_with_candidate(&anchor, &records, Some(&records[0]))?,
                expected,
                "a repeated candidate must retain original deduplication"
            );
            assert_eq!(
                material.finish()?,
                usage,
                "capacity trials must not copy admitted Results or allocate a trial list"
            );
        }
        resource_io::admit_cloned_value(&*records[1])?;
        let mut changed = (*records[1]).clone();
        changed.task_status.push('!');
        assert_eq!(
            read.pending_measure_with_candidate(&anchor, &records[..1], Some(&changed))
                .unwrap_err()
                .to_string(),
            "idle pending Result changed within read cut"
        );
        Ok(())
    })
}

#[tokio::test]
async fn idle_result_holder_retains_its_original_capture_after_read_cut() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, pending) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    let selected = runtime.with_idle_read(None, &material, |read| {
        read.record(&pending[0].record.result_identity)?
            .context("Result missing")
    })?;
    let capture = Arc::downgrade(&selected._capture);
    let holder = selected.clone();
    drop((selected, material));
    assert!(capture.upgrade().is_some());
    assert_eq!(holder.result_identity, pending[0].record.result_identity);
    drop(holder);
    assert!(capture.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn idle_overlapping_input_and_task_reads_reuse_the_same_cut() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (_, pending) = pending_fixture(&harness).await?;
    let material = material(runtime)?;
    runtime
        .with_idle_read(None, &material, |read| {
            let mut request = read.input_request();
            for entry in &pending {
                request.insert_admitted_root(Family::Message, &entry.record.message_id)?;
            }
            let inputs = read.inputs(&request)?;
            let key = TaskKey::new(
                TaskFamily::ResultSettlement,
                &pending[0].record.result_identity,
            );
            let tasks = read.tasks(key.clone())?;
            let usage = material.finish()?;
            for _ in 0..INPUT_BATCH_SCAN {
                let row = read
                    .fact(Family::Message, &pending[0].record.message_id)?
                    .context("qualified Message missing")?;
                assert!(Arc::ptr_eq(
                    &row,
                    inputs
                        .lookup(&Key::new(Family::Message, &pending[0].record.message_id))?
                        .context("selected Message missing")?,
                ));
                assert!(Arc::ptr_eq(&tasks, &read.tasks(key.clone())?));
                assert_eq!(material.finish()?, usage);
            }
            read.tx.execute(
                "UPDATE agent_states SET control_revision=control_revision+1 WHERE agent_id=?1",
                [&read.scope.agent_id],
            )?;
            let error = read.tasks(key).unwrap_err();
            assert_eq!(
                error.to_string(),
                "context scope changed after its validated read"
            );
            assert!(read
                .fact(Family::Message, &pending[0].record.message_id)
                .is_err());
            Err::<(), _>(error)
        })
        .expect_err("changed scope must invalidate every cached family");
    Ok(())
}

#[tokio::test]
async fn idle_queue_read_keeps_original_body_material_until_last_holder() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let message = message();
    runtime.enqueue(message.clone()).await?;
    let material = material(runtime)?;
    let record = runtime
        .with_idle_read(None, &material, |read| read.queue(&message.id))?
        .context("qualified Queue missing")?;
    assert_eq!(record.message_id, message.id);
    assert_eq!(record.status, QueueEntryStatus::Queued);
    let weak = record.0.weak_material();
    let last = record.clone();
    assert!(Arc::ptr_eq(&record.0, &last.0));
    material.finish()?;
    drop(material);
    drop(record);
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE queue_entries RENAME TO idle_queue_body_forbidden")?;
        Ok(())
    })?;
    assert_eq!(last.message_id, message.id);
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn idle_queue_read_refuses_raw_tampered_and_unproven_absent_addresses() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let message = message();
    runtime.enqueue(message.clone()).await?;
    let material = material(runtime)?;
    assert!(runtime
        .with_idle_read(None, &material, |read| read.queue("never-qualified"))
        .is_err());
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE queue_entries SET payload_json=json_set(payload_json,'$.status','interjected') WHERE message_id=?1",
            [&message.id],
        )?;
        Ok(())
    })?;
    let material = self::material(runtime)?;
    assert!(runtime
        .with_idle_read(None, &material, |read| read.queue(&message.id))
        .is_err());
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "DELETE FROM current_read_qualification WHERE family='queue' AND record_id=?1",
            [&message.id],
        )?;
        Ok(())
    })?;
    let material = self::material(runtime)?;
    assert!(runtime
        .with_idle_read(None, &material, |read| read.queue(&message.id))
        .is_err());
    Ok(())
}

#[tokio::test]
async fn idle_queue_read_preserves_proven_absence_and_rejects_incarnation_drift() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    // Establish the original Current namespace before inserting its negative fact.
    runtime.enqueue(message()).await?;
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "INSERT INTO current_read_qualification VALUES(?1,?2,'queue',?3,1,'absent',NULL)",
            params![
                "default",
                runtime.inner.identity_incarnation,
                "proven-absent"
            ],
        )?;
        Ok(())
    })?;
    let material = material(runtime)?;
    assert!(runtime
        .with_idle_read(None, &material, |read| read.queue("proven-absent"))?
        .is_none());
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',?2) WHERE agent_id=?1",
            params!["default", runtime.inner.identity_incarnation + 1],
        )?;
        Ok(())
    })?;
    let material = self::material(runtime)?;
    assert!(runtime
        .with_idle_read(None, &material, |read| read.queue("proven-absent"))
        .is_err());
    Ok(())
}
