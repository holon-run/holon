async fn pause_deletion_before_private_purge(host: &RuntimeHost, mut job: AgentDeletionJob) {
    if job.archive_path.is_none() {
        // Give internal cleanup fixtures the same pre-purge observation barrier.
        job.archive_path = Some(
            host.config()
                .data_dir
                .join("archives")
                .join(format!("{}.tar.gz", job.deletion_id)),
        );
        host.runtime_db().agent_deletions().update(&job).unwrap();
    }
    fs::create_dir_all(job.archive_path.as_ref().unwrap()).unwrap();
    assert!(host
        .execute_deletion_job(job.clone())
        .await
        .unwrap_err()
        .to_string()
        .contains("phase Archive"));
    let paused = host
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(&job.agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(paused.phase, AgentDeletionPhase::Archive);
    assert_eq!(paused.status, AgentDeletionStatus::RetryableFailed);
}

async fn resume_deletion_after_archive_failure(host: &RuntimeHost, agent_id: &str) {
    let mut job = host
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(agent_id)
        .unwrap()
        .unwrap();
    fs::remove_dir(job.archive_path.as_ref().unwrap()).unwrap();
    job.next_attempt_at = Some(Utc::now());
    host.runtime_db().agent_deletions().update(&job).unwrap();
    host.execute_deletion_job(job).await.unwrap();
    crate::runtime_db::agent_deletion_cleanup::assert_private_history_cleared(
        host.runtime_db(),
        agent_id,
    )
    .unwrap();
}

#[tokio::test]
async fn deletion_terminalizes_triggered_waits_and_external_operation_ownership() {
    use crate::runtime_db::DestructiveOperationRecord;

    for legacy_index_proof in [false, true] {
        let fixture = provider_test_config(Some("dummy-token"));
        let host = RuntimeHost::new(fixture.config).unwrap();
        host.default_runtime().await.unwrap();
        let agent_id = "unconsumed-deletion";
        host.create_named_agent(agent_id, None).await.unwrap();
        let now = Utc::now();
        let turn = crate::types::TurnRecord::new(agent_id, "operation-turn", 1);
        host.agent_storage(agent_id)
            .unwrap()
            .append_turn(&turn)
            .unwrap();
        let mut wait = WaitConditionRecord {
            id: "triggered-before-deletion".into(),
            agent_id: agent_id.into(),
            work_item_id: None,
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::External,
            source: None,
            subject_ref: None,
            waiting_for: "outside event".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some(turn.turn_id.clone()),
            trigger_message_id: None,
            triggered_at: None,
        };
        host.runtime_db().wait_conditions().upsert(&wait).unwrap();
        wait.status = WaitConditionStatus::Triggered;
        wait.trigger_message_id = Some("unconsumed-wake".into());
        wait.triggered_at = Some(now);
        host.runtime_db().wait_conditions().upsert(&wait).unwrap();
        let verification_marker = host.config().data_dir.join("must-not-verify");
        let phases = [
            DestructiveOperationPhase::Planned,
            DestructiveOperationPhase::Scheduled,
            DestructiveOperationPhase::DaemonInterrupted,
            DestructiveOperationPhase::Verified,
        ];
        for (index, phase) in phases.iter().enumerate() {
            host.runtime_db()
                .destructive_operations()
                .plan_or_get(&DestructiveOperationRecord {
                    operation_id: format!("owned-operation-{index}"),
                    owner_turn_id: turn.turn_id.clone(),
                    owner_work_item_id: None,
                    command_digest: "fixture".into(),
                    phase: *phase,
                    verification_target: format!("touch {}", verification_marker.display()),
                    recovery_policy: "verify_only".into(),
                    created_at: now,
                    updated_at: now,
                })
                .unwrap();
        }
        let (_, mut job, _) = host
            .begin_public_agent_deletion(agent_id, false, "test")
            .await
            .unwrap();
        if legacy_index_proof {
            crate::runtime_db::agent_deletion_cleanup::settle_execution(
                host.runtime_db(),
                agent_id,
                1,
                now,
            )
            .unwrap();
            // Model an old proof that omitted unresolved waits and operations.
            host.runtime_db()
                .transaction(|tx| {
                    tx.execute(
                        "UPDATE wait_conditions SET status = 'triggered', cancelled_at = NULL,
                         payload_json = ?1 WHERE wait_condition_id = ?2",
                        rusqlite::params![serde_json::to_string(&wait)?, wait.id],
                    )?;
                    tx.execute(
                        "UPDATE destructive_operations SET phase = 'scheduled'
                         WHERE operation_id = 'owned-operation-1'",
                        [],
                    )?;
                    Ok(())
                })
                .unwrap();
            job.phase = AgentDeletionPhase::Index;
            host.runtime_db().agent_deletions().update(&job).unwrap();
        }
        pause_deletion_before_private_purge(&host, job).await;
        let cancelled = host
            .runtime_db()
            .wait_conditions()
            .latest_for_agent(agent_id)
            .unwrap()
            .into_iter()
            .find(|record| record.id == wait.id)
            .unwrap();
        assert_eq!(cancelled.status, WaitConditionStatus::Cancelled);
        assert!(cancelled.cancelled_at.is_some());
        assert_eq!(cancelled.trigger_message_id, wait.trigger_message_id);
        for (index, phase) in phases.iter().enumerate() {
            let operation = host
                .runtime_db()
                .destructive_operations()
                .by_id(&format!("owned-operation-{index}"))
                .unwrap()
                .unwrap();
            assert_eq!(
                operation.phase,
                if *phase == DestructiveOperationPhase::Verified {
                    DestructiveOperationPhase::Verified
                } else {
                    DestructiveOperationPhase::OwnerClosed
                }
            );
        }
        assert!(!verification_marker.exists());
        resume_deletion_after_archive_failure(&host, agent_id).await;
        host.create_public_named_agent_with_name(
            agent_id,
            None,
            None,
            None,
            None,
            AgentBootstrapExecution::Inline,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            host.agent_identity_record(agent_id)
                .unwrap()
                .unwrap()
                .incarnation,
            2
        );
        assert!(host
            .runtime_db()
            .wait_conditions()
            .latest_for_agent(agent_id)
            .unwrap()
            .is_empty());
        assert!(host
            .runtime_db()
            .destructive_operations()
            .by_id("owned-operation-1")
            .unwrap()
            .is_none());
        let new_turn = crate::types::TurnRecord::new(agent_id, "replacement-turn", 1);
        host.agent_storage(agent_id)
            .unwrap()
            .append_turn(&new_turn)
            .unwrap();
        for (index, phase) in phases.iter().enumerate() {
            let replay = DestructiveOperationRecord {
                operation_id: format!("owned-operation-{index}"),
                owner_turn_id: new_turn.turn_id.clone(),
                owner_work_item_id: None,
                command_digest: "fixture".into(),
                phase: *phase,
                verification_target: "true".into(),
                recovery_policy: "verify_only".into(),
                created_at: now,
                updated_at: now,
            };
            assert!(host
                .runtime_db()
                .destructive_operations()
                .plan_and_schedule(&replay)
                .unwrap_err()
                .to_string()
                .contains("destructive_operation_owner_closed"));
        }
        host.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn recreated_agent_initializes_each_incarnation_without_old_bootstrap_replay() {
    let fixture = provider_test_config(Some("dummy-token"));
    let host = RuntimeHost::new(fixture.config).unwrap();
    let agent_id = "clean-slate-intro";
    host.default_runtime().await.unwrap();
    let mut previous: Option<AgentBootstrapRecord> = None;
    for incarnation in 1..=34 {
        host.create_public_named_agent_with_name(
            agent_id,
            None,
            None,
            None,
            None,
            AgentBootstrapExecution::Inline,
            true,
        )
        .await
        .unwrap();
        let runtime = host.get_public_agent(agent_id).await.unwrap();
        let bootstrap = host
            .runtime_db()
            .agent_bootstraps()
            .latest(agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(bootstrap.incarnation, Some(incarnation));
        let messages = runtime.storage().read_recent_messages(10).unwrap();
        assert_eq!(messages.len(), 1, "replacement must not inherit history");
        assert_eq!(
            messages[0].id,
            format!("agent_bootstrap_message:{agent_id}:{incarnation}")
        );
        if let Some(mut old) = previous.take() {
            assert!(host
                .apply_agent_initial_message_bootstrap(&old)
                .await
                .is_err());
            assert!(host
                .record_agent_bootstrap_result(
                    &mut old,
                    AgentBootstrapStepKind::InitialMessage,
                    Ok(()),
                )
                .is_err());
            assert_eq!(
                host.runtime_db()
                    .agent_bootstraps()
                    .latest(agent_id)
                    .unwrap(),
                Some(bootstrap.clone())
            );
        }
        previous = Some(bootstrap);
        if incarnation < 34 {
            let (_, job, _) = host
                .begin_public_agent_deletion(agent_id, false, "test")
                .await
                .unwrap();
            host.execute_deletion_job(job).await.unwrap();
        }
    }
}

#[tokio::test]
async fn deleting_a_provider_blocked_turn_leaves_no_working_state_after_recreation() {
    let fixture = provider_test_config(Some("dummy-token"));
    let started = Arc::new(Notify::new());
    let host = RuntimeHost::new_with_provider(
        fixture.config,
        Arc::new(BlockingProvider {
            started: started.clone(),
        }),
    )
    .unwrap();
    let agent_id = "deleted-provider-turn";
    host.default_runtime().await.unwrap();
    host.create_public_named_agent_with_name(
        agent_id,
        None,
        None,
        None,
        None,
        AgentBootstrapExecution::Inline,
        false,
    )
    .await
    .unwrap();
    let runtime = host.get_public_agent(agent_id).await.unwrap();
    let started_wait = started.notified();
    tokio::pin!(started_wait);
    started_wait.as_mut().enable();
    runtime
        .enqueue(MessageEnvelope::new(
            agent_id,
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "block until this incarnation is deleted".into(),
            },
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), &mut started_wait)
        .await
        .expect("the old incarnation must enter the provider");
    let before = host
        .runtime_db()
        .conversation()
        .summary_snapshot(agent_id, 20, None, "test", "operator")
        .unwrap()
        .unwrap();
    assert_eq!(before.value.active_turns.len(), 1);

    let (_, job, _) = host
        .begin_public_agent_deletion(agent_id, false, "test")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.execute_deletion_job(job))
        .await
        .expect("deletion must not wait for a blocked provider")
        .unwrap();
    host.create_public_named_agent_with_name(
        agent_id,
        None,
        None,
        None,
        None,
        AgentBootstrapExecution::Inline,
        false,
    )
    .await
    .unwrap();
    let after = host
        .runtime_db()
        .conversation()
        .summary_snapshot(agent_id, 20, None, "test", "operator")
        .unwrap()
        .unwrap();
    assert!(after.value.active_turns.is_empty());
    assert!(after.value.turns.is_empty());
    assert!(after.value.pending_inputs.is_empty());
    let replacement = host.get_public_agent(agent_id).await.unwrap();
    assert!(replacement
        .storage()
        .read_recent_messages(100)
        .unwrap()
        .is_empty());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn bootstrap_admission_retry_does_not_duplicate_an_already_enqueued_intro() {
    let fixture = provider_test_config(Some("dummy-token"));
    let host = RuntimeHost::new(fixture.config).unwrap();
    let agent_id = "intro-before-receipt";
    host.create_public_named_agent_with_name(
        agent_id,
        None,
        None,
        None,
        None,
        AgentBootstrapExecution::Inline,
        true,
    )
    .await
    .unwrap();
    let mut bootstrap = host
        .runtime_db()
        .agent_bootstraps()
        .latest(agent_id)
        .unwrap()
        .unwrap();
    // Model the crash window after enqueue, before the step receipt.
    bootstrap.initial_message.status = AgentBootstrapStepStatus::Pending;
    host.runtime_db()
        .agent_bootstraps()
        .upsert(&bootstrap)
        .unwrap();
    host.reconcile_agent_bootstrap(agent_id).await.unwrap();
    host.reconcile_agent_bootstrap(agent_id).await.unwrap();
    let runtime = host.get_public_agent(agent_id).await.unwrap();
    assert_eq!(runtime.storage().read_recent_messages(10).unwrap().len(), 1);
    assert_eq!(
        host.runtime_db()
            .agent_bootstraps()
            .latest(agent_id)
            .unwrap()
            .unwrap()
            .initial_message
            .status,
        AgentBootstrapStepStatus::Succeeded
    );
}

#[tokio::test]
async fn deletion_archive_is_opt_in_and_is_not_replacement_context() {
    let (_home, host) = test_host();
    let agent_id = "explicit-history-archive";
    host.create_public_named_agent_with_name(
        agent_id,
        None,
        None,
        None,
        None,
        AgentBootstrapExecution::Inline,
        false,
    )
    .await
    .unwrap();
    let old_home = host.agent_data_dir(agent_id);
    fs::write(old_home.join("memory/self.md"), "old private memory").unwrap();
    let (_, job, _) = host
        .begin_public_agent_deletion_with_archive(agent_id, false, "test", true)
        .await
        .unwrap();
    let archive = job.archive_path.clone().unwrap();
    host.execute_deletion_job(job).await.unwrap();
    assert!(archive.is_file());
    assert!(!old_home.exists());
    host.create_public_named_agent_with_name(
        agent_id,
        None,
        None,
        None,
        None,
        AgentBootstrapExecution::Inline,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        host.agent_identity_record(agent_id)
            .unwrap()
            .unwrap()
            .incarnation,
        2
    );
    assert!(!fs::read_to_string(old_home.join("memory/self.md"))
        .unwrap_or_default()
        .contains("old private memory"));
    assert!(host
        .get_public_agent(agent_id)
        .await
        .unwrap()
        .storage()
        .read_recent_messages(10)
        .unwrap()
        .is_empty());
    assert!(archive.is_file(), "explicit archive is user-retained");
}

#[tokio::test]
async fn failed_deletion_archive_preserves_source_and_blocks_recreation() {
    let (_home, host) = test_host();
    let agent_id = "archive-failure-source";
    host.create_public_named_agent_with_name(
        agent_id,
        None,
        None,
        None,
        None,
        AgentBootstrapExecution::Inline,
        false,
    )
    .await
    .unwrap();
    let old_home = host.agent_data_dir(agent_id);
    fs::write(old_home.join("memory/self.md"), "preserve on failure").unwrap();
    let (_, job, _) = host
        .begin_public_agent_deletion_with_archive(agent_id, false, "test", true)
        .await
        .unwrap();
    // An invalid existing destination cannot be replaced by an implicit backup.
    fs::create_dir_all(job.archive_path.as_ref().unwrap()).unwrap();
    let _ = host.execute_deletion_job(job).await;
    let (_, current) = host.public_agent_deletion_status(agent_id).unwrap();
    let current = current.unwrap();
    assert_eq!(current.status, AgentDeletionStatus::RetryableFailed);
    assert_eq!(current.phase, AgentDeletionPhase::Archive);
    assert_eq!(
        fs::read_to_string(old_home.join("memory/self.md")).unwrap(),
        "preserve on failure"
    );
    assert!(host
        .create_public_named_agent_with_name(
            agent_id,
            None,
            None,
            None,
            None,
            AgentBootstrapExecution::Inline,
            false,
        )
        .await
        .is_err());
}
