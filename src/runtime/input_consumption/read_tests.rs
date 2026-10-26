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
async fn idle_pending_preview_matches_prompt_reply_dedup_and_omission() -> Result<()> {
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
            let expected_replies =
                usize::from(count > 0 && anchor.id != entries[0].reply.as_ref().unwrap().id);
            assert_eq!(
                rendered.matches("received_agent_message:").count(),
                expected_replies
            );
            if count > limit {
                assert!(rendered
                    .ends_with("- omitted_results: 1 (use task query tools for the full set)"));
            }
        }
    }
    Ok(())
}

async fn pending_fixture(
    harness: &LifecycleHarness,
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
    for index in 0..2 {
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
