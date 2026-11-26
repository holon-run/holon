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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn startup_retires_legacy_destructive_operations_without_executing_them() -> Result<()> {
    use crate::types::{ToolExecutionRecord, ToolExecutionStatus, TurnRecord};
    use holon_domain::{TurnTerminalKind, TurnTerminalSummary};
    use rusqlite::params;
    use serde_json::json;

    let (_home, host) = canonical_test_host();
    let config = host.config().as_ref().clone();
    let db_path = host.runtime_db().path().to_path_buf();
    let ledger_exists = |db: &rusqlite::Connection| -> rusqlite::Result<bool> {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'destructive_operations')",
            [],
            |row| row.get(0),
        )
    };
    assert!(!ledger_exists(&rusqlite::Connection::open(&db_path)?)?);
    let runtime = host.default_runtime().await?;
    let work = runtime
        .create_work_item("existing work".into(), None, None, Vec::new())
        .await?;
    let agent_id = &work.agent_id;
    let storage = host.agent_storage(agent_id)?;
    let now = Utc::now();
    let legacy = [
        ("old-scheduled", "scheduled"),
        ("old-interrupted", "daemon_interrupted"),
        ("old-verified", "verified"),
    ];
    let finished = TurnTerminalSummary {
        kind: TurnTerminalKind::Completed,
        reason: None,
        no_brief_reason: None,
        completed_at: now,
        duration_ms: 0,
    };
    let interrupted = TurnTerminalSummary {
        kind: TurnTerminalKind::Interrupted,
        reason: Some("startup_recovery/destructive_operation_verify_only".into()),
        ..finished.clone()
    };
    for (index, (id, phase)) in legacy.iter().enumerate() {
        let mut turn = TurnRecord::new(agent_id, *id, index as u64 + 1);
        turn.current_work_item_id = Some(work.id.clone());
        turn.terminal = match *phase {
            "daemon_interrupted" => Some(interrupted.clone()),
            "verified" => Some(finished.clone()),
            _ => None,
        };
        storage.append_turn(&turn)?;
    }

    let marker_dir = tempfile::tempdir()?;
    let command_marker = marker_dir.path().join("operation-ran");
    let verify_marker = marker_dir.path().join("verification-ran");
    let command = format!("printf marker > '{}'", command_marker.display());
    let verification = format!("printf marker > '{}'", verify_marker.display());
    let receipt = ToolExecutionRecord {
        id: "old-tool-receipt".into(),
        agent_id: agent_id.clone(),
        work_item_id: Some(work.id.clone()),
        turn_index: 3,
        turn_id: Some("old-verified".into()),
        tool_name: "ScheduleDestructiveOperation".into(),
        created_at: now,
        completed_at: Some(now),
        duration_ms: 0,
        authority_class: AuthorityClass::OperatorInstruction,
        status: ToolExecutionStatus::Success,
        input: json!({"command": command}),
        output: json!({"phase": "scheduled"}),
        summary: "historical dispatch receipt".into(),
        call_binding: None,
        invocation_surface: None,
    };
    storage.append_tool_execution(&receipt)?;
    drop(runtime);
    drop(storage);
    host.shutdown().await?;
    drop(host);

    {
        let db = rusqlite::Connection::open(&db_path)?;
        // Restore only the retired v120 schema pieces in the disposable fixture.
        let migration = holon_storage::migrations::MIGRATIONS
            .iter()
            .find(|migration| migration.name == "destructive_operation_fences")
            .unwrap();
        db.execute_batch(migration.sql)?;
        db.execute_batch(
            "ALTER TABLE destructive_operations ADD COLUMN command_digest TEXT NOT NULL DEFAULT '';
             DELETE FROM schema_migrations WHERE name = 'retire_destructive_operation_scheduler';
             INSERT INTO runtime_metadata (key, value, created_at, updated_at)
             VALUES ('destructive_operation_closed:old-closed', '1', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);",
        )?;
        for (id, phase) in legacy {
            db.execute(
                "INSERT INTO destructive_operations
                 (operation_id, owner_turn_id, owner_work_item_id, phase, verification_target,
                  recovery_policy, created_at, updated_at, command_digest)
                 VALUES (?1,?1,?2,?3,?4,'verify_only',?5,?5,'fixture-digest')",
                params![id, work.id, phase, verification, now.to_rfc3339()],
            )?;
            assert_eq!(
                db.execute(
                    "UPDATE turn_records
                     SET payload_json = json_set(payload_json, '$.destructive_operation_id', ?1)
                     WHERE turn_id = ?1",
                    [id],
                )?,
                1,
            );
        }
    }

    for startup in 0..2 {
        let host = RuntimeHost::new_with_provider(
            config.clone(),
            Arc::new(StubProvider::new("done")),
        )?;
        host.recover_orphaned_queue_claims_at_startup().await?;
        assert!(!command_marker.exists());
        assert!(!verify_marker.exists());
        let db = host.runtime_db();
        db.transaction(|tx| {
            assert!(!ledger_exists(tx)?);
            assert_eq!(
                tx.query_row(
                    "SELECT COUNT(*) FROM runtime_metadata WHERE key GLOB 'destructive_operation_closed:*'",
                    [],
                    |row| row.get::<_, u64>(0),
                )?,
                0,
            );
            assert_eq!(
                tx.query_row(
                    "SELECT COUNT(*) FROM turn_records
                     WHERE json_type(payload_json,'$.destructive_operation_id') IS NOT NULL",
                    [],
                    |row| row.get::<_, u64>(0),
                )?,
                0,
            );
            Ok(())
        })?;
        for (id, phase) in legacy {
            let turn = db.turn_records().by_id(Some(agent_id), id)?.unwrap();
            assert_eq!(turn.current_work_item_id.as_deref(), Some(work.id.as_str()));
            let terminal = turn.terminal.unwrap();
            if phase == "scheduled" {
                assert_eq!(terminal.kind, TurnTerminalKind::Interrupted);
                assert_eq!(
                    terminal.reason.as_deref(),
                    Some("startup_recovery/external_side_effect_indeterminate"),
                );
            } else if phase == "daemon_interrupted" {
                assert_eq!(terminal, interrupted);
            } else {
                assert_eq!(terminal, finished);
            }
        }
        let preserved = db
            .evidence()
            .tool_execution_by_id(agent_id, &receipt.id)?
            .unwrap();
        assert_eq!(preserved.input, receipt.input);
        assert_eq!(preserved.output, receipt.output);
        assert_eq!(preserved.status, receipt.status);
        let runtime = host.default_runtime().await?;
        let (_, picked) = runtime.pick_work_item(work.id.clone()).await?;
        assert_eq!(picked.id, work.id);
        let continued = runtime
            .update_work_item_fields(
                work.id.clone(),
                Some(format!("authorized follow-up after startup {startup}")),
                None,
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(continued.objective, format!("authorized follow-up after startup {startup}"));
        host.shutdown().await?;
    }
    Ok(())
}

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn deletion_terminalizes_triggered_waits() {
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
            // Model an old proof that omitted unresolved waits.
            host.runtime_db()
                .transaction(|tx| {
                    tx.execute(
                        "UPDATE wait_conditions SET status = 'triggered', cancelled_at = NULL,
                         payload_json = ?1 WHERE wait_condition_id = ?2",
                        rusqlite::params![serde_json::to_string(&wait)?, wait.id],
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
        host.shutdown().await.unwrap();
    }
}

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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
    // Restore deliberately corrupted storage without asking the identity writer
    // to accept an invalid previous incarnation.
    host.runtime_db()
        .connection()
        .unwrap()
        .execute(
            "UPDATE agent_identities SET payload_json = ?1 WHERE agent_id = ?2",
            rusqlite::params![serde_json::to_string(&identity).unwrap(), identity.agent_id],
        )
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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn operator_delete_status_covers_admitted_parent_supervised_child() {
    let (_home, host) = test_host();
    let child = retained_cleanup_fixture(&host, "tmp_child_operator_delete_status");
    let (_, admitted, _) = host
        .begin_public_agent_deletion(&child.agent_id, false, "test")
        .await
        .expect("operator deletion must admit a private parent-supervised child");
    let (identity, job) = host
        .public_agent_deletion_status(&child.agent_id)
        .expect("deletion status must read the same surface that admitted the job");
    assert_eq!(identity.agent_id, child.agent_id);
    assert_eq!(
        job.expect("admitted job must be visible").deletion_id,
        admitted.deletion_id
    );
}

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn operator_delete_status_reads_deleted_parent_supervised_child_tombstone() {
    let (_home, host) = test_host();
    let child = retained_cleanup_fixture(&host, "tmp_child_operator_delete_tombstone");
    let (_, job, _) = host
        .begin_public_agent_deletion(&child.agent_id, false, "test")
        .await
        .unwrap();
    host.execute_deletion_job(job).await.unwrap();
    let (identity, job) = host
        .public_agent_deletion_status(&child.agent_id)
        .expect("deleted tombstone must remain observable on the operator delete surface");
    assert_eq!(identity.status, AgentRegistryStatus::Deleted);
    assert_eq!(identity.visibility, AgentVisibility::Private);
    assert_eq!(job.unwrap().status, AgentDeletionStatus::Completed);
}

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn operator_delete_status_keeps_public_self_owned_surface() {
    let (_home, host) = test_host();
    let agent_id = "public-delete-status";
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
    let (_, job, _) = host
        .begin_public_agent_deletion(agent_id, false, "test")
        .await
        .unwrap();
    let (_, status) = host
        .public_agent_deletion_status(agent_id)
        .expect("public self-owned deletion status must keep working");
    assert_eq!(status.unwrap().deletion_id, job.deletion_id);
}

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn operator_delete_status_does_not_cover_unrelated_private_agent() {
    let (_home, host) = test_host();
    let identity = AgentIdentityRecord::new(
        "private-standalone",
        AgentKind::Named,
        AgentVisibility::Private,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PrivateChild,
        None,
        None,
    );
    host.append_agent_identity(&identity).unwrap();
    assert!(matches!(
        host.public_agent_deletion_status("private-standalone"),
        Err(PublicAgentError::Private { .. })
    ));
}

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn operator_delete_status_reads_private_agent_job_after_supervision_ends() {
    let (_home, host) = test_host();
    let identity = AgentIdentityRecord::new(
        "private-stranded-job",
        AgentKind::Named,
        AgentVisibility::Private,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PrivateChild,
        None,
        None,
    );
    host.append_agent_identity(&identity).unwrap();
    host.runtime_db()
        .agent_identities()
        .upsert(&identity)
        .unwrap();
    let (_, job, created) = host
        .runtime_db()
        .agent_deletions()
        .begin(&identity.agent_id, identity.revision, "operator", false)
        .unwrap();
    assert!(created);

    let (visible_identity, visible_job) = host
        .operator_agent_deletion_status(&identity.agent_id)
        .expect("operator read must stay reachable after the agent lost its delete surface");
    assert_eq!(visible_identity.agent_id, identity.agent_id);
    assert_eq!(
        visible_job.expect("stranded job must be readable").deletion_id,
        job.deletion_id
    );
    assert!(matches!(
        host.public_agent_deletion_status(&identity.agent_id),
        Err(PublicAgentError::Private { .. })
    ));
}
