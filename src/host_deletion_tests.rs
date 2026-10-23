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
