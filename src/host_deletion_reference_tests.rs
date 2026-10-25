// Included from host.rs's tests module.
mod deletion_reference_tests {
    use super::*;

    const SOURCE_CHILD: &str = "source-bound-child";

    fn fence_target(host: &RuntimeHost, agent_id: &str) -> crate::types::AgentDeletionJob {
        let identity = host.agent_identity_record(agent_id).unwrap().unwrap();
        let (deleting, job, created) = host
            .runtime_db()
            .agent_deletions()
            .begin(
                agent_id,
                identity.revision,
                "deletion-reference-test",
                false,
            )
            .unwrap();
        assert!(created);
        assert_eq!(deleting.status, AgentRegistryStatus::Deleting);
        host.cache_agent_identity(&deleting).unwrap();
        job
    }

    fn purge_target(host: &RuntimeHost, agent_id: &str, incarnation: u64) {
        use crate::runtime_db::agent_deletion_cleanup;

        // These fixtures have no loaded target runtime or live external resources.
        agent_deletion_cleanup::settle_execution(
            host.runtime_db(),
            agent_id,
            incarnation,
            Utc::now(),
        )
        .unwrap();
        agent_deletion_cleanup::purge_private_history(
            host.runtime_db(),
            agent_id,
            incarnation,
            Utc::now(),
        )
        .unwrap();
        agent_deletion_cleanup::assert_private_history_cleared(host.runtime_db(), agent_id)
            .unwrap();
    }

    fn assert_deleted_result(result: &ChildTaskTerminalResult, incarnation: u64) {
        assert_eq!(result.status, TaskStatus::Cancelled);
        let detail = result.task_detail.as_ref().expect("cancellation evidence");
        assert_eq!(detail["target_deleted"], true);
        assert_eq!(detail["target_incarnation"], incarnation);
        assert_eq!(detail["cancellation_reason"], "agent_deleted");
        assert!(result.text.contains("was deleted"), "{}", result.text);
        assert!(!result.text.contains("original-source-result"));
        assert!(!result.text.contains("replacement-result"));
    }

    fn assert_cancelled_task_result(host: &RuntimeHost, task: &TaskRecord) -> MessageEnvelope {
        assert_eq!(task.status, TaskStatus::Cancelled);
        let detail = task.detail.as_ref().unwrap();
        assert_eq!(detail["target_deleted"], true);
        assert_eq!(detail["target_incarnation"], 1);
        assert_eq!(detail["cancellation_reason"], "agent_deleted");
        assert_eq!(task.target().unwrap().incarnation, Some(1));
        let results = host
            .runtime_db()
            .messages()
            .all(Some(&task.agent_id))
            .unwrap()
            .into_iter()
            .filter(|message| {
                message.kind == MessageKind::TaskResult
                    && matches!(&message.origin, MessageOrigin::Task { task_id } if task_id == &task.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1, "one cancellation result per parent task");
        let result = results.into_iter().next().unwrap();
        assert_eq!(task.parent_message_id.as_deref(), Some(result.id.as_str()));
        assert_eq!(result.agent_id, task.agent_id);
        let metadata = result.metadata.as_ref().unwrap();
        assert_eq!(metadata["task_status"], "cancelled");
        assert_eq!(
            metadata["task_detail"]["cancellation_reason"],
            "agent_deleted"
        );
        assert_eq!(metadata["task_detail"]["target_incarnation"], 1);
        assert!(matches!(&result.body, MessageBody::Text { text }
            if text.contains("was deleted") && !text.contains("replacement-result")));
        assert!(
            host.runtime_db()
                .queue_entries()
                .latest(&result.id)
                .unwrap()
                .is_some(),
            "the result must be admitted to the parent queue"
        );
        result
    }

    fn delivery_ids(host: &RuntimeHost) -> Vec<String> {
        host.runtime_db()
            .with_read(|connection| {
                let mut statement = connection.prepare(
                    "SELECT delivery_id FROM agent_message_deliveries ORDER BY delivery_id",
                )?;
                let ids = statement
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(ids)
            })
            .unwrap()
    }

    #[tokio::test]
    async fn pending_supervised_invocation_cancels_at_fence_and_after_private_purge() {
        let (_home, host, task) = source_bound_invocation_fixture();
        host.runtime_db().tasks().upsert(&task).unwrap();
        let now = Utc::now();
        host.agent_storage(SOURCE_CHILD)
            .unwrap()
            .append_wait_condition(&WaitConditionRecord {
                id: "source-child-still-waiting".into(),
                agent_id: SOURCE_CHILD.into(),
                work_item_id: None,
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::Task,
                source: None,
                subject_ref: Some("unfinished-child-work".into()),
                waiting_for: "original invocation continuation".into(),
                wake_sources: vec![WakeSource::TaskResult {
                    task_id: "unfinished-child-work".into(),
                }],
                continuation: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,
                turn_id: Some("original-source-turn".into()),
                trigger_message_id: None,
                triggered_at: None,
            })
            .unwrap();
        assert!(matches!(
            host.resolve_supervised_invocation(&task, SOURCE_CHILD, false)
                .await
                .unwrap(),
            ChildInvocationResolution::Pending(ChildInvocationReason::AwaitingContinuation)
        ));

        fence_target(&host, SOURCE_CHILD);
        for purged in [false, true] {
            if purged {
                purge_target(&host, SOURCE_CHILD, 1);
                assert!(host
                    .runtime_db()
                    .messages()
                    .all(Some(SOURCE_CHILD))
                    .unwrap()
                    .is_empty());
                assert!(host
                    .runtime_db()
                    .turn_records()
                    .by_id(Some(SOURCE_CHILD), "original-source-turn")
                    .unwrap()
                    .is_none());
            }
            let resolution = host
                .resolve_supervised_invocation(&task, SOURCE_CHILD, false)
                .await
                .unwrap();
            match resolution {
                ChildInvocationResolution::Ready(result) => assert_deleted_result(&result, 1),
                other => panic!("purged={purged}: expected cancellation, got {other:?}"),
            }
            // Resolution must not rewrite the caller's retained history.
            assert_eq!(
                host.runtime_db().tasks().latest(&task.id).unwrap(),
                Some(task.clone())
            );
            assert!(host.try_get_loaded_runtime(SOURCE_CHILD).await.is_none());
        }
    }

    #[tokio::test]
    async fn legacy_delivery_invocation_cancels_after_its_ledger_is_purged() {
        let (_home, host, mut task) = source_bound_invocation_fixture();
        let parent = host.default_runtime().await.unwrap();
        parent.wait_for_bootstrap().await.unwrap();
        let target = host.agent_identity_record(SOURCE_CHILD).unwrap().unwrap();
        // Pre-generation delivery monitors only retained the child id.
        task.detail
            .as_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("target");
        parent.storage().append_task(&task).unwrap();
        assert_eq!(task.target().unwrap().incarnation, None);
        let admitted = host
            .invoke_existing_agent(
                &task,
                &target.agent_id,
                "old request".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await
            .unwrap();
        let delivery_id = admitted.delivery_id.expect("accepted invocation delivery");
        let delivery = host
            .runtime_db()
            .agent_message_deliveries()
            .latest(&delivery_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            delivery.caller.current_task_id.as_deref(),
            Some(task.id.as_str())
        );
        assert_eq!(delivery.correlation_id.as_deref(), Some(task.id.as_str()));

        let job = fence_target(&host, &target.agent_id);
        let fenced = tokio::time::timeout(
            Duration::from_secs(5),
            host.await_invocation_delivery_terminal_result(
                &target.agent_id,
                &delivery_id,
                &task.id,
                false,
            ),
        )
        .await
        .expect("a fenced legacy invocation must resolve")
        .unwrap();
        assert_deleted_result(&fenced, 1);
        host.execute_deletion_job(job).await.unwrap();
        assert_eq!(
            host.agent_identity_record(&target.agent_id)
                .unwrap()
                .unwrap()
                .status,
            AgentRegistryStatus::Deleted
        );
        assert!(host
            .runtime_db()
            .agent_message_deliveries()
            .latest(&delivery_id)
            .unwrap()
            .is_none());
        let purged = tokio::time::timeout(
            Duration::from_secs(5),
            host.await_invocation_delivery_terminal_result(
                &target.agent_id,
                &delivery_id,
                &task.id,
                false,
            ),
        )
        .await
        .expect("purged delivery evidence must not strand the caller")
        .unwrap();
        assert_deleted_result(&purged, 1);
        assert_eq!(
            host.runtime_db().tasks().latest(&task.id).unwrap(),
            Some(task)
        );
        assert!(host
            .try_get_loaded_runtime(&target.agent_id)
            .await
            .is_none());
        host.shutdown().await.unwrap();
    }

    #[test]
    fn missing_future_and_ambiguous_legacy_targets_are_not_deletion_evidence() {
        let (_home, host) = test_host();
        for incarnation in [None, Some(1), Some(2)] {
            assert!(host
                .deleted_target_result("never-created", incarnation)
                .unwrap()
                .is_none());
        }
        for status in [
            AgentRegistryStatus::Active,
            AgentRegistryStatus::Deleting,
            AgentRegistryStatus::Deleted,
        ] {
            for current in [1, 2] {
                let mut identity = AgentIdentityRecord::new(
                    format!("deletion-evidence-{status:?}-{current}"),
                    AgentKind::Named,
                    AgentVisibility::Public,
                    AgentOwnership::SelfOwned,
                    AgentProfilePreset::PublicNamed,
                    None,
                    None,
                );
                identity.incarnation = current;
                identity.status = status;
                if status == AgentRegistryStatus::Deleted {
                    identity.deleted_at = Some(identity.updated_at);
                }
                // Independent imported identity snapshots, never a retired -> active update.
                host.runtime_db()
                    .agent_identities()
                    .upsert(&identity)
                    .unwrap();
                assert!(
                    host.deleted_target_result(&identity.agent_id, Some(current + 1))
                        .unwrap()
                        .is_none(),
                    "a future generation is not deleted: {identity:?}"
                );
                let legacy = host
                    .deleted_target_result(&identity.agent_id, None)
                    .unwrap();
                if current == 1 && status != AgentRegistryStatus::Active {
                    assert_deleted_result(
                        &legacy.expect("unambiguous first-generation legacy target"),
                        1,
                    );
                } else {
                    assert!(
                        legacy.is_none(),
                        "legacy binding must not guess: {identity:?}"
                    );
                }
                let exact = host
                    .deleted_target_result(&identity.agent_id, Some(current))
                    .unwrap();
                if status == AgentRegistryStatus::Active {
                    assert!(exact.is_none());
                } else {
                    assert_deleted_result(&exact.unwrap(), current);
                }
            }
        }
    }

    #[tokio::test]
    async fn supervised_resolver_does_not_cancel_missing_or_future_target_identity() {
        let (_home, host, task) = source_bound_invocation_fixture();
        let mut missing = task.clone();
        missing.detail.as_mut().unwrap()["child_agent_id"] = json!("never-created");
        missing.detail.as_mut().unwrap()["target"]["agent_id"] = json!("never-created");
        assert!(matches!(
            host.resolve_supervised_invocation(&missing, "never-created", false)
                .await
                .unwrap(),
            ChildInvocationResolution::RecoveryBlocked(ChildInvocationReason::IdentityMismatch)
        ));
        fence_target(&host, SOURCE_CHILD);
        let mut future = task;
        future.detail.as_mut().unwrap()["target"]["incarnation"] = json!(2);
        assert!(matches!(
            host.resolve_supervised_invocation(&future, SOURCE_CHILD, false)
                .await
                .unwrap(),
            ChildInvocationResolution::RecoveryBlocked(ChildInvocationReason::IdentityMismatch)
        ));
        assert!(host.try_get_loaded_runtime("never-created").await.is_none());
        assert!(host.try_get_loaded_runtime(SOURCE_CHILD).await.is_none());
    }

    #[tokio::test]
    async fn captured_invocation_generation_never_uses_replacement_terminal_evidence() {
        let (_home, host, task) = source_bound_invocation_fixture();
        host.runtime_db().tasks().upsert(&task).unwrap();
        let job = fence_target(&host, SOURCE_CHILD);
        host.execute_deletion_job(job).await.unwrap();
        let fresh = host
            .runtime_db()
            .agent_identities()
            .reincarnate_with_bootstrap_and_relations(
                SOURCE_CHILD,
                "deletion-reference-test",
                |old| {
                    let mut fresh = old.clone();
                    fresh.incarnation = old.incarnation + 1;
                    fresh.revision = old.revision + 1;
                    fresh.status = AgentRegistryStatus::Active;
                    fresh.deleted_at = None;
                    fresh.created_at = Utc::now();
                    fresh.updated_at = fresh.created_at;
                    let bootstrap = crate::types::AgentBootstrapRecord::new(
                        SOURCE_CHILD,
                        crate::types::AgentBootstrapDesiredState {
                            template: None,
                            catalog_agent_home: None,
                            workspace: None,
                            model_resolution: None,
                            initial_message: None,
                            default_initial_message: false,
                        },
                    );
                    let relations =
                        supervised_creation_records(&fresh, &task.agent_id, &task.id, None);
                    (fresh, bootstrap, relations)
                },
            )
            .unwrap();
        host.cache_agent_identity(&fresh).unwrap();
        assert_eq!(fresh.incarnation, 2);
        let mut state = AgentState::new(SOURCE_CHILD);
        state.turn_index = 1;
        state.last_turn_terminal = Some(crate::types::TurnTerminalRecord {
            turn_id: "replacement-turn".into(),
            turn_index: 1,
            kind: TurnTerminalKind::Completed,
            reason: None,
            last_assistant_message: Some("replacement-result".into()),
            no_brief_reason: None,
            checkpoint: None,
            completed_at: Utc::now(),
            duration_ms: 1,
        });
        host.agent_storage(SOURCE_CHILD)
            .unwrap()
            .write_agent(&state)
            .unwrap();
        seed_supervised_child_terminal(&host, &task.id, SOURCE_CHILD);
        // The shared seed helper creates a legacy first-generation source. Bind
        // this replacement source explicitly so its positive control is valid.
        let mut sources = host
            .runtime_db()
            .messages()
            .supervised_invocation_sources(SOURCE_CHILD, &task.id)
            .unwrap();
        assert_eq!(sources.len(), 1);
        let mut source = sources.pop().unwrap();
        source.metadata.as_mut().unwrap()["child_incarnation"] = json!(2);
        host.runtime_db()
            .messages()
            .upsert_with_index_changes(&source, &[])
            .unwrap();

        let mut current_task = task.clone();
        current_task.detail.as_mut().unwrap()["target"]["incarnation"] = json!(2);
        assert!(
            matches!(host.resolve_supervised_invocation(&current_task, SOURCE_CHILD, false)
            .await.unwrap(), ChildInvocationResolution::Ready(result)
            if result.status == TaskStatus::Completed && result.text == "replacement-result")
        );
        let mut ambiguous_legacy = current_task.clone();
        ambiguous_legacy.detail.as_mut().unwrap()["target"]
            .as_object_mut()
            .unwrap()
            .remove("incarnation");
        assert!(matches!(
            host.resolve_supervised_invocation(&ambiguous_legacy, SOURCE_CHILD, false)
                .await
                .unwrap(),
            ChildInvocationResolution::RecoveryBlocked(ChildInvocationReason::IdentityMismatch)
        ));
        match host
            .resolve_supervised_invocation(&task, SOURCE_CHILD, false)
            .await
            .unwrap()
        {
            ChildInvocationResolution::Ready(result) => assert_deleted_result(&result, 1),
            other => panic!("old generation must cancel, got {other:?}"),
        }
        assert_eq!(
            host.agent_identity_record(SOURCE_CHILD).unwrap(),
            Some(fresh)
        );
        assert_eq!(
            host.agent_storage(SOURCE_CHILD)
                .unwrap()
                .read_agent()
                .unwrap(),
            Some(state)
        );
        assert!(host.try_get_loaded_runtime(SOURCE_CHILD).await.is_none());
    }

    #[tokio::test]
    async fn captured_generation_is_rechecked_after_activation_handoff() {
        let (_home, host, _task) = source_bound_invocation_fixture();
        let handoff = host.inner.bootstrap_handoff_lock.lock().await;
        let activating = host.clone();
        let activation = tokio::spawn(async move {
            activating.activate_agent_observed_for_incarnation(
                SOURCE_CHILD, RuntimeActivationReason::ChildSupervision, None, Some(1),
            ).await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while host.inner.runtime_operations.lock().unwrap().tasks.is_empty() {
                tokio::task::yield_now().await;
            }
        }).await.expect("activation must reach the owned cold-operation handoff");
        // Model the deleter/recreator holding the same handoff while an old
        // invocation has already passed its initial identity observation.
        let mut replacement = host.agent_identity_record(SOURCE_CHILD).unwrap().unwrap();
        replacement.incarnation = 2;
        replacement.revision += 1;
        replacement.updated_at = Utc::now();
        host.runtime_db().agent_identities().upsert(&replacement).unwrap();
        host.cache_agent_identity(&replacement).unwrap();
        drop(handoff);
        let result = tokio::time::timeout(Duration::from_secs(5), activation).await.unwrap().unwrap();
        let error = result.err().expect("old generation cannot activate its replacement");
        assert_eq!(describe_runtime_error(&error).code, "agent_incarnation_mismatch");
        assert!(host.try_get_loaded_runtime(SOURCE_CHILD).await.is_none());
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn parent_bootstrap_publishes_one_cancelled_child_result_after_private_purge() {
        let (_home, host, fixture_task) = source_bound_invocation_fixture();
        let mut task = fixture_task.clone();
        task.kind = TaskKind::ChildAgentTask;
        task.recovery = Some(TaskRecoverySpec::ChildAgentTask {
            summary: "recover original invocation".into(),
            prompt: "must never be resent".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        });
        host.agent_storage(&task.agent_id)
            .unwrap()
            .append_task(&task)
            .unwrap();
        fence_target(&host, SOURCE_CHILD);
        purge_target(&host, SOURCE_CHILD, 1);

        let parent = host.default_runtime().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), parent.wait_for_bootstrap())
            .await
            .expect("parent recovery must settle purged targets")
            .unwrap();
        // Bootstrap completion is the barrier; this helper reads an already terminal task.
        let terminal = wait_for_terminal_task(&parent, &task.id).await;
        let result = assert_cancelled_task_result(&host, &terminal);
        assert_eq!(
            result.id,
            format!("message:child-invocation-result:{}", task.id)
        );
        assert!(host.try_get_loaded_runtime(SOURCE_CHILD).await.is_none());
        let config = host.config().as_ref().clone();
        host.shutdown().await.unwrap();
        drop(parent);
        drop(host);

        let restarted =
            RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
        let parent = restarted.default_runtime().await.unwrap();
        parent.wait_for_bootstrap().await.unwrap();
        let persisted = wait_for_terminal_task(&parent, &task.id).await;
        assert_eq!(persisted, terminal);
        assert_eq!(assert_cancelled_task_result(&restarted, &persisted), result);
        assert!(restarted
            .try_get_loaded_runtime(SOURCE_CHILD)
            .await
            .is_none());
        restarted.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recovered_message_wait_cancels_deleted_or_recreated_target_without_resend() {
        for phase in ["deleting", "deleted", "recreated"] {
            for bound_delivery in [false, true] {
                let (_home, host) = test_host();
                let parent = host.default_runtime().await.unwrap();
                parent.wait_for_bootstrap().await.unwrap();
                let target_id = "message-wait-deletion-target";
                host.create_named_agent(target_id, None).await.unwrap();
                let mut task = test_child_supervision_task(
                    &host.config().default_agent_id,
                    "message-wait-deletion-task",
                );
                let receipt =
                    send_agent_message(&parent, target_id, &task.id, "original request").await;
                assert_eq!(
                    receipt.outcome,
                    crate::types::AgentMessageDeliveryOutcome::Accepted
                );
                let cursor = host
                    .runtime_db()
                    .agent_message_deliveries()
                    .rowid(&receipt.delivery_id)
                    .unwrap()
                    .unwrap();
                task.kind = TaskKind::AgentMessageWait;
                task.detail = Some(json!({
                    "target_agent_id": target_id,
                    "target": {"kind":"agent_message_wait", "agent_id":target_id, "incarnation":1},
                }));
                if bound_delivery {
                    task.detail.as_mut().unwrap()["request_delivery_id"] =
                        json!(receipt.delivery_id);
                    task.detail.as_mut().unwrap()["message_wait_after_delivery_rowid"] =
                        json!(cursor);
                }
                task.recovery = Some(TaskRecoverySpec::AgentMessageWait {
                    summary: "wait for original target".into(),
                    message: "must never be resent".into(),
                    target_agent_id: target_id.into(),
                    authority_class: AuthorityClass::OperatorInstruction,
                    delivery_id: bound_delivery.then(|| receipt.delivery_id.clone()),
                    after_delivery_rowid: bound_delivery.then_some(cursor),
                    reply_deadline_at: None,
                    requester_incarnation: None,
                });
                parent.storage().append_task(&task).unwrap();
                let job = fence_target(&host, target_id);
                if phase != "deleting" {
                    host.execute_deletion_job(job).await.unwrap();
                    assert!(host
                        .runtime_db()
                        .agent_message_deliveries()
                        .latest(&receipt.delivery_id)
                        .unwrap()
                        .is_none());
                }
                if phase == "recreated" {
                    let replacement = host.create_public_named_agent_with_name(
                        target_id, None, None, None, None,
                        AgentBootstrapExecution::Inline, false,
                    ).await.unwrap();
                    assert_eq!(replacement.identity.incarnation, 2);
                    let target = host.get_public_agent(target_id).await.unwrap();
                    // Keep a row above the purged cursor, then admit a real replacement reply.
                    send_agent_message(
                        &target,
                        &task.agent_id,
                        "replacement-cursor-anchor",
                        "new generation",
                    )
                    .await;
                    let reply = send_agent_message(
                        &target,
                        &task.agent_id,
                        "replacement-reply",
                        "replacement-result",
                    )
                    .await;
                    assert_eq!(
                        reply.outcome,
                        crate::types::AgentMessageDeliveryOutcome::Accepted
                    );
                    assert!(
                        host.runtime_db()
                            .agent_message_deliveries()
                            .rowid(&reply.delivery_id)
                            .unwrap()
                            .unwrap()
                            > cursor
                    );
                    let cancelled = tokio::time::timeout(
                        Duration::from_secs(5),
                        host.await_agent_message(&task.agent_id, target_id, cursor, Some(1), Some(&task.id)),
                    )
                    .await
                    .expect("old generation cannot consume a replacement reply")
                    .unwrap();
                    assert_deleted_result(&cancelled, 1);
                }
                let identity_before = host.agent_identity_record(target_id).unwrap().unwrap();
                let deliveries_before = delivery_ids(&host);
                // Both a persisted cursor and the older pre-receipt recovery shape must cancel.
                for _ in 0..2 {
                    let (reattached, remaining) = tokio::time::timeout(
                        Duration::from_secs(5),
                        parent.recover_supervised_child_tasks(vec![task.clone(), task.clone()]),
                    )
                    .await
                    .expect("deletion recovery must not resend or wait for the target")
                    .unwrap();
                    assert_eq!(reattached.len(), 2, "{phase}, bound={bound_delivery}");
                    assert!(remaining.is_empty(), "{phase}, bound={bound_delivery}");
                    let terminal = wait_for_terminal_task(&parent, &task.id).await;
                    let result = assert_cancelled_task_result(&host, &terminal);
                    assert_eq!(result.id, format!("message:agent-message-wait:{}", task.id));
                    assert_eq!(terminal.recovery, task.recovery);
                    if bound_delivery {
                        assert_eq!(
                            terminal.detail.as_ref().unwrap()["request_delivery_id"],
                            receipt.delivery_id
                        );
                        assert_eq!(
                            terminal.detail.as_ref().unwrap()["message_wait_after_delivery_rowid"],
                            cursor
                        );
                    }
                    assert_eq!(
                        delivery_ids(&host),
                        deliveries_before,
                        "recovery must not resend"
                    );
                    assert_eq!(
                        host.agent_identity_record(target_id).unwrap(),
                        Some(identity_before.clone())
                    );
                }
                if phase == "deleted" {
                    assert!(host.try_get_loaded_runtime(target_id).await.is_none());
                }
                host.shutdown().await.unwrap();
            }
        }
    }
}
