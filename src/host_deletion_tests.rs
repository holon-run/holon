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
                Utc::now(),
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

#[tokio::test]
async fn host_restart_does_not_rebuild_bootstrap_row_for_deleted_tombstone() {
    let fixture = provider_test_config(Some("dummy-token"));
    let host = RuntimeHost::new(fixture.config.clone()).unwrap();
    let agent_id = "restart-bootstrap-tombstone";
    host.default_runtime().await.unwrap();
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
    assert!(host
        .runtime_db()
        .agent_bootstraps()
        .latest(agent_id)
        .unwrap()
        .is_some());

    let (_, job, _) = host
        .begin_public_agent_deletion(agent_id, false, "test")
        .await
        .unwrap();
    host.execute_deletion_job(job).await.unwrap();
    let completed = host
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, AgentDeletionStatus::Completed);
    assert_eq!(
        host.agent_identity_record(agent_id)
            .unwrap()
            .unwrap()
            .status,
        AgentRegistryStatus::Deleted
    );
    assert!(host
        .runtime_db()
        .agent_bootstraps()
        .latest(agent_id)
        .unwrap()
        .is_none());
    drop(host);

    // Restart semantics: constructing a fresh Host on the same data dir must
    // not restore the purged bootstrap row for the Deleted tombstone.
    let restarted = RuntimeHost::new(fixture.config).unwrap();
    assert!(restarted
        .runtime_db()
        .agent_bootstraps()
        .latest(agent_id)
        .unwrap()
        .is_none());
    assert_eq!(
        restarted
            .agent_identity_record(agent_id)
            .unwrap()
            .unwrap()
            .status,
        AgentRegistryStatus::Deleted
    );
    let tombstone_job = restarted
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(tombstone_job.status, AgentDeletionStatus::Completed);
    assert_eq!(tombstone_job.deletion_id, completed.deletion_id);
    let outcome = restarted
        .scan_legacy_deletion_residue_batch()
        .await
        .unwrap();
    assert_eq!(outcome.created, 0);
    assert_eq!(
        restarted
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(agent_id)
            .unwrap()
            .unwrap()
            .status,
        AgentDeletionStatus::Completed
    );
    crate::runtime_db::agent_deletion_cleanup::assert_private_history_cleared(
        restarted.runtime_db(),
        agent_id,
    )
    .unwrap();
}

#[tokio::test]
async fn cleanup_repair_repurges_residue_after_completed_purge() {
    let fixture = provider_test_config(Some("dummy-token"));
    let host = RuntimeHost::new(fixture.config).unwrap();
    let agent_id = "repair-repurge-bootstrap-residue";
    host.default_runtime().await.unwrap();
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
    let (_, job, _) = host
        .begin_public_agent_deletion(agent_id, false, "test")
        .await
        .unwrap();
    host.execute_deletion_job(job).await.unwrap();
    crate::runtime_db::agent_deletion_cleanup::assert_private_history_cleared(
        host.runtime_db(),
        agent_id,
    )
    .unwrap();

    // Replay the incident: a legacy writer restores a purged private row
    // after the deletion already completed.
    host.runtime_db()
        .agent_bootstraps()
        .upsert(&AgentBootstrapRecord::legacy_ready(agent_id))
        .unwrap();

    let outcome = host.scan_legacy_deletion_residue_batch().await.unwrap();
    assert_eq!(outcome.created, 1);
    let repair = host
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(repair.mode, AgentDeletionMode::CleanupRepair);

    host.execute_deletion_job(repair.clone()).await.unwrap();

    // The repair converged: the residue is purged again and the tombstone
    // job completes instead of retrying at the Quiesce gate forever.
    assert!(host
        .runtime_db()
        .agent_bootstraps()
        .latest(agent_id)
        .unwrap()
        .is_none());
    let finished = host
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(finished.status, AgentDeletionStatus::Completed);
    assert_eq!(finished.deletion_id, repair.deletion_id);
    assert_eq!(
        host.agent_identity_record(agent_id)
            .unwrap()
            .unwrap()
            .status,
        AgentRegistryStatus::Deleted
    );
    crate::runtime_db::agent_deletion_cleanup::assert_private_history_cleared(
        host.runtime_db(),
        agent_id,
    )
    .unwrap();
    let repeated = host.scan_legacy_deletion_residue_batch().await.unwrap();
    assert_eq!(repeated.created, 0);
}

#[tokio::test]
async fn daemon_coordinator_paces_legacy_residue_drain() {
    let (_home, host) = test_host();

    fn residue_cursor(host: &RuntimeHost) -> Option<String> {
        match host.runtime_db().connection().unwrap().query_row(
            "SELECT value FROM runtime_metadata WHERE key = 'legacy_deletion_repair_cursor'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(value) => Some(value),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(error) => panic!("failed to read legacy residue cursor: {error}"),
        }
    }

    fn consumed_identities(host: &RuntimeHost, cursor: &str) -> i64 {
        host.runtime_db()
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_identities WHERE agent_id <= ?1",
                [cursor],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    }

    // Seed enough identities that one scan pass needs several batches, so
    // the coordinator's drain pacing is observable.
    for index in 0..256u32 {
        let identity = AgentIdentityRecord::new(
            format!("residue-pace-{index:03}"),
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.runtime_db()
            .agent_identities()
            .upsert(&identity)
            .unwrap();
    }
    for index in 0..48 {
        retained_cleanup_fixture(&host, &format!("residue-observed-{index:03}"));
    }
    // Start from a persisted batch cursor, as after a daemon restart. Surface
    // fixture/scan errors directly rather than treating them as timing failures.
    let first = host.scan_legacy_deletion_residue_batch().await.unwrap();
    assert_eq!(
        first.scanned,
        crate::deletion::LEGACY_DELETION_REPAIR_BATCH_LIMIT
    );
    assert!(first.cursor.is_some());
    assert_eq!(residue_cursor(&host), first.cursor);
    assert_eq!(
        consumed_identities(&host, first.cursor.as_deref().unwrap()),
        16
    );
    host.spawn_daemon_deletion_coordinator();

    // A paced drain (LEGACY_RESIDUE_DRAIN_INTERVAL = 200ms) must keep the
    // pass in progress after one second. An unpaced drain re-arming the
    // shared sweep at ~1ms finishes every batch in milliseconds, clears the
    // cursor, and burns a core on large databases (#3884).
    let started = tokio::time::Instant::now();
    let mut saw_cursor = false;
    let mut samples = Vec::new();
    while started.elapsed() < Duration::from_secs(1) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let cursor = residue_cursor(&host);
        saw_cursor |= cursor.is_some();
        samples.push((started.elapsed(), cursor));
    }
    assert!(
        saw_cursor,
        "coordinator lost the legacy residue cursor: {samples:?}; remaining identities={}",
        consumed_identities(&host, "zzzz")
    );
    let cursor =
        residue_cursor(&host).expect("paced drain must not finish the whole scan pass within 1s");
    let consumed = consumed_identities(&host, &cursor);
    assert!(
        consumed <= 128,
        "unexpected drain progress after 1s: {consumed} identities"
    );
    let observed: i64 = host
        .runtime_db()
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM subagent_cleanup_observations
             WHERE child_agent_id LIKE 'residue-observed-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        observed, 16,
        "residue continuation must not repeat responsibility observations"
    );

    // Check progress with a generous deadline, independently of 1s throughput.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while residue_cursor(&host).is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "paced legacy residue drain never completed the scan pass"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    host.shutdown_daemon_deletion_coordinator().await;
}

#[tokio::test]
async fn daemon_coordinator_runs_later_residue_without_repeating_maintenance() {
    let (_home, host) = test_host();
    for index in 0..48 {
        let identity = AgentIdentityRecord::new(
            format!("later-residue-{index:03}"),
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.runtime_db()
            .agent_identities()
            .upsert(&identity)
            .unwrap();
    }
    let agent_id = "zz-later-residue";
    let mut deleted = AgentIdentityRecord::new(
        agent_id,
        AgentKind::Default,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    deleted.status = AgentRegistryStatus::Deleted;
    host.append_agent_identity(&deleted).unwrap();
    host.runtime_db()
        .agent_identities()
        .upsert(&deleted)
        .unwrap();
    host.runtime_db()
        .agent_bootstraps()
        .upsert(&AgentBootstrapRecord::legacy_ready(agent_id))
        .unwrap();

    let agents_root = host.config().data_dir.join("agents");
    let startup_trash = agents_root.join("startup.deleting_trash");
    fs::create_dir_all(&startup_trash).unwrap();
    host.spawn_daemon_deletion_coordinator();
    tokio::time::timeout(Duration::from_secs(5), async {
        while startup_trash.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("startup maintenance sweep must run");
    // Once the startup sweep finishes, continuation batches must not run it
    // again. The tombstone is beyond the startup batch and needs a new wake.
    let later_trash = agents_root.join("later.deleting_trash");
    fs::create_dir_all(&later_trash).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if host
                .runtime_db()
                .agent_deletions()
                .latest_for_agent(agent_id)
                .unwrap()
                .is_some_and(|job| job.status == AgentDeletionStatus::Completed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("later residue job must finish without waiting for the 60s sweep");
    assert!(
        later_trash.exists(),
        "residue drain repeated trash maintenance"
    );
    assert!(host
        .runtime_db()
        .agent_bootstraps()
        .latest(agent_id)
        .unwrap()
        .is_none());
    host.shutdown_daemon_deletion_coordinator().await;
}

#[tokio::test]
async fn daemon_coordinator_notified_before_residue_cursor_commit() {
    let (_home, host) = test_host();
    let agent_id = "a-partial-residue";
    let mut deleted = AgentIdentityRecord::new(
        agent_id,
        AgentKind::Default,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    deleted.status = AgentRegistryStatus::Deleted;
    host.runtime_db()
        .agent_identities()
        .upsert(&deleted)
        .unwrap();
    for index in 0..20 {
        let identity = AgentIdentityRecord::new(
            format!("z-partial-residue-{index:03}"),
            AgentKind::Default,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.runtime_db()
            .agent_identities()
            .upsert(&identity)
            .unwrap();
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            host.inner.daemon_deletion_notify.notified()
        )
        .await
        .is_err(),
        "fixture must not leave a deletion wake"
    );
    host.runtime_db()
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_residue_cursor_commit
             BEFORE INSERT ON runtime_metadata
             WHEN NEW.key = 'legacy_deletion_repair_cursor'
             BEGIN SELECT RAISE(ABORT, 'injected legacy residue cursor failure'); END;",
        )
        .unwrap();

    let error = host
        .scan_legacy_deletion_residue_batch()
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("injected legacy residue cursor failure"),
        "{error:#}"
    );
    let admitted = host
        .runtime_db()
        .agent_deletions()
        .latest_for_agent(agent_id)
        .unwrap()
        .expect("admission must commit before the cursor failure");
    assert_eq!(admitted.status, AgentDeletionStatus::Pending);
    tokio::time::timeout(
        Duration::from_millis(100),
        host.inner.daemon_deletion_notify.notified(),
    )
    .await
    .expect("committed admission must notify even when the scan later fails");
}

#[tokio::test]
async fn daemon_coordinator_defers_failed_residue_scan_to_safety_sweep() {
    let (_home, host) = test_host();
    let identity = AgentIdentityRecord::new(
        "broken-residue",
        AgentKind::Default,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    host.runtime_db()
        .agent_identities()
        .upsert(&identity)
        .unwrap();
    host.runtime_db()
        .connection()
        .unwrap()
        .execute(
            "UPDATE agent_identities SET payload_json = '{}' WHERE agent_id = ?1",
            [&identity.agent_id],
        )
        .unwrap();
    assert!(host.scan_legacy_deletion_residue_batch().await.is_err());
    for index in 0..16 {
        retained_cleanup_fixture(&host, &format!("failed-scan-observed-{index:03}"));
    }
    host.spawn_daemon_deletion_coordinator();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let observed: i64 = host
                .runtime_db()
                .connection()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM subagent_cleanup_observations
                     WHERE child_agent_id LIKE 'failed-scan-observed-%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if observed == 16 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scan errors must not prevent responsibility observations");
    host.runtime_db()
        .agent_identities()
        .upsert(&identity)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let cursor_count: i64 = host
        .runtime_db()
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM runtime_metadata WHERE key = 'legacy_deletion_repair_cursor'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        cursor_count, 0,
        "failed scan must not schedule a drain retry"
    );
    host.shutdown_daemon_deletion_coordinator().await;
}
