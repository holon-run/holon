use super::resource_observation::ResourceObservation;
use super::*;
use crate::{
    config::AppConfig, diagnostics::io_probe::DenyResourceReaders, host::RuntimeHost,
    provider::StubProvider, types::WorkItemPlanStatus,
};

#[tokio::test]
async fn actual_foreground_and_background_shell_invalidate_at_start_and_os_wait_without_long_writer(
) -> Result<()> {
    use crate::types::{CommandTaskSpec, ExecCommandDuplicatePolicy, ExecCommandOutcome};
    let (home, host, runtime) = fixture().await?;
    let preparation = runtime.hold_resource_preparation_for_test().await?;
    for background in [false, true] {
        let started = home.path().join(format!("shell-started-{background}"));
        let release = home.path().join(format!("shell-release-{background}"));
        let before = runtime.inner.resources.registry.generation_for_test()?;
        let command = format!(
            "printf started > '{}'; while ! test -e '{}'; do sleep 0.01; done; printf finished",
            started.display(),
            release.display()
        );
        let mut request = Some(tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .execute_exec_command(
                        CommandTaskSpec {
                            cmd: command,
                            workdir: None,
                            shell: None,
                            login: false,
                            tty: false,
                            yield_time_ms: if background { 1 } else { 10_000 },
                            max_output_tokens: None,
                            accepts_input: false,
                            terminal_reentry: false,
                        },
                        ExecCommandDuplicatePolicy::StartNew,
                        &AuthorityClass::OperatorInstruction,
                        None,
                    )
                    .await
            }
        }));
        tokio::time::timeout(Duration::from_secs(10), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        let running_generation = runtime.inner.resources.registry.generation_for_test()?;
        assert_eq!(running_generation, before + 1);
        if background {
            let result = tokio::time::timeout(Duration::from_secs(10), request.take().unwrap())
                .await
                .context("background shell promotion did not finish")???;
            assert!(matches!(
                result.outcome,
                ExecCommandOutcome::PromotedToTask { .. }
            ));
        }
        std::fs::write(release, "exit")?;
        if !background {
            let result = tokio::time::timeout(Duration::from_secs(10), request.take().unwrap())
                .await
                .context("foreground shell completion did not finish")???;
            assert!(matches!(
                result.outcome,
                ExecCommandOutcome::Completed {
                    exit_status: Some(0),
                    ..
                }
            ));
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while runtime
                .inner
                .resources
                .registry
                .generation_for_test()
                .unwrap()
                == running_generation
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        assert_eq!(
            runtime.inner.resources.registry.generation_for_test()?,
            running_generation + 1
        );
    }
    drop(preparation);
    tokio::time::timeout(Duration::from_secs(10), host.shutdown())
        .await
        .context("shell invalidation host shutdown did not finish")??;
    Ok(())
}

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost, RuntimeHandle)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = None;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let runtime = host.default_runtime().await?;
    runtime.await_initial_resources().await;
    {
        let _writer = runtime.begin_context_resource_write()?;
        let home = runtime.agent_home();
        std::fs::create_dir_all(home.join("memory"))?;
        std::fs::create_dir_all(home.join("notes"))?;
        std::fs::create_dir_all(home.join("skills/resource-skill"))?;
        std::fs::write(
            home.join("AGENTS.md"),
            "# RESOURCE_GUIDANCE\nLoaded resource contract\n",
        )?;
        std::fs::write(home.join("memory/operator.md"), "RESOURCE_MEMORY\n")?;
        std::fs::write(
            home.join("notes/resource.md"),
            "# RESOURCE_NOTE\nA real nonempty note\n",
        )?;
        std::fs::write(home.join("skills/resource-skill/SKILL.md"),
            "---\nname: ResourceSkill\ndescription: RESOURCE_SKILL_DESCRIPTION\n---\nA real skill\n")?;
    }
    runtime.refresh_context_resources().await?;
    Ok((home, host, runtime))
}

#[tokio::test]
async fn private_resource_writes_preserve_another_agents_ready_observation() -> Result<()> {
    let (_home, host, writer_runtime) = fixture().await?;
    host.create_named_agent("resource-peer", None).await?;
    let peer = host.get_or_create_agent("resource-peer").await?;
    let original = peer.ensure_context_resources().await?;
    let preparation = peer.hold_resource_preparation_for_test().await?;
    let generation = peer.inner.resources.registry.generation_for_test()?;
    let writer = writer_runtime.begin_context_resource_write()?;
    std::fs::write(
        writer_runtime.agent_home().join("AGENTS.md"),
        "PRIVATE_WRITE",
    )?;
    let denied = DenyResourceReaders::new();
    let prompt = tokio::time::timeout(
        Duration::from_secs(3),
        peer.preview_prompt(
            "unrelated agent".into(),
            AuthorityClass::OperatorInstruction,
        ),
    )
    .await??;
    assert!(Arc::ptr_eq(
        &original,
        &prompt._resource_read_material.as_ref().unwrap().observation
    ));
    assert_eq!(
        peer.inner.resources.registry.generation_for_test()?,
        generation
    );
    assert_eq!(denied.attempts(), 0);
    drop(writer);
    assert_eq!(
        peer.inner.resources.registry.generation_for_test()?,
        generation
    );
    assert!(Arc::ptr_eq(
        &original,
        &tokio::time::timeout(Duration::from_secs(3), peer.ensure_context_resources()).await??
    ));
    drop((denied, preparation));
    let changed = writer_runtime.ensure_context_resources().await?;
    assert_eq!(
        changed
            .body()
            .guidance
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "PRIVATE_WRITE"
    );
    host.shutdown().await
}

#[tokio::test]
async fn shared_guidance_write_invalidates_consumers_and_preserves_held_snapshots() -> Result<()> {
    let home = tempfile::tempdir()?;
    let user = home.path().join("user");
    std::fs::create_dir_all(user.join(".agents"))?;
    let guidance = user.join(".agents/AGENTS.md");
    std::fs::write(&guidance, "OLD_SHARED_GUIDANCE")?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = Some(user);
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let first = host.default_runtime().await?;
    host.create_named_agent("shared-resource-peer", None)
        .await?;
    let second = host.get_or_create_agent("shared-resource-peer").await?;
    let old_first = first.ensure_context_resources().await?;
    let old_second = second.ensure_context_resources().await?;
    let held_first = first.hold_resource_preparation_for_test().await?;
    let held_second = second.hold_resource_preparation_for_test().await?;
    let writer = first.begin_context_resource_write_for_paths(vec![guidance.clone()])?;
    std::fs::write(&guidance, "NEW_SHARED_GUIDANCE")?;
    for runtime in [&first, &second] {
        assert!(runtime
            .inner
            .resources
            .gate
            .ready_for(&runtime.inner.resources.registry)
            .is_err());
        assert!(capture_loaded_prompt(runtime, "during shared write")
            .await
            .is_err());
    }
    drop((writer, held_first, held_second));
    for (runtime, old) in [(&first, old_first), (&second, old_second)] {
        let prompt = runtime
            .preview_prompt(
                "after shared write".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await?;
        assert_eq!(
            prompt
                .loaded_agents_md
                .user_global_source
                .as_ref()
                .unwrap()
                .content,
            "NEW_SHARED_GUIDANCE"
        );
        assert_eq!(
            old.body()
                .guidance
                .user_global_source
                .as_ref()
                .unwrap()
                .content,
            "OLD_SHARED_GUIDANCE"
        );
        assert!(!Arc::ptr_eq(
            &old,
            &prompt._resource_read_material.as_ref().unwrap().observation
        ));
    }
    // An absolute patch target can be outside the caller's execution root.
    crate::tool::tools::apply_patch_tool::execute(
        &first, first.instance_agent_id(), &AuthorityClass::OperatorInstruction,
        &serde_json::json!({"patch": format!("--- {0}\n+++ {0}\n@@ -1,1 +1,1 @@\n-NEW_SHARED_GUIDANCE\n+PATCHED_SHARED_GUIDANCE\n", guidance.display())}),
    ).await?;
    let prompt = second
        .preview_prompt(
            "after another agents patch".into(),
            AuthorityClass::OperatorInstruction,
        )
        .await?;
    assert!(prompt
        .loaded_agents_md
        .user_global_source
        .as_ref()
        .unwrap()
        .content
        .contains("PATCHED_SHARED_GUIDANCE"));
    host.shutdown().await
}

#[cfg(unix)]
#[tokio::test]
async fn patch_through_linked_skill_refreshes_peer_without_mutating_held_observation() -> Result<()>
{
    let (home, host, first) = fixture().await?;
    host.create_named_agent("linked-skill-peer", None).await?;
    let peer = host.get_or_create_agent("linked-skill-peer").await?;
    peer.await_initial_resources().await;
    // Outside both AgentHomes and without configured user-global skill roots.
    let library = home.path().join("shared/demo");
    std::fs::create_dir_all(&library)?;
    std::fs::write(
        library.join("SKILL.md"),
        "---\nname: LinkedDemo\ndescription: OLD_LINKED_SKILL\n---\nShared skill\n",
    )?;
    for runtime in [&first, &peer] {
        let _writer = runtime.begin_context_resource_write()?;
        std::fs::create_dir_all(runtime.agent_home().join("skills"))?;
        std::os::unix::fs::symlink(&library, runtime.agent_home().join("skills/demo"))?;
    }
    first.refresh_context_resources().await?;
    peer.refresh_context_resources().await?;
    // A pending observer notification must not erase aliases on its cache hit.
    peer.observe_context_resources_for_test().await?;
    let old = peer.ensure_context_resources().await?;
    let description = |observation: &ResourceObservation| {
        observation
            .body()
            .skills
            .discoverable_skills
            .iter()
            .find(|entry| entry.name == "LinkedDemo")
            .unwrap()
            .description
            .clone()
    };
    assert_eq!(description(&old), "OLD_LINKED_SKILL");
    let preparation = peer.hold_resource_preparation_for_test().await?;
    let before = peer.inner.resources.registry.generation_for_test()?;
    let target = first.agent_home().join("skills/demo/SKILL.md");
    crate::tool::tools::apply_patch_tool::execute(
        &first, first.instance_agent_id(), &AuthorityClass::OperatorInstruction,
        &serde_json::json!({"patch": format!("--- {0}\n+++ {0}\n@@ -1,5 +1,5 @@\n ---\n name: LinkedDemo\n-description: OLD_LINKED_SKILL\n+description: NEW_LINKED_SKILL\n ---\n Shared skill\n", target.display())}),
    ).await?;
    assert!(peer.inner.resources.registry.generation_for_test()? > before);
    assert_eq!(description(&old), "OLD_LINKED_SKILL");
    drop(preparation);
    let changed = peer.ensure_context_resources().await?;
    assert_eq!(description(&changed), "NEW_LINKED_SKILL");
    assert!(!Arc::ptr_eq(&old, &changed));
    {
        let denied = DenyResourceReaders::new();
        let current = peer.ensure_context_resources().await?;
        assert!(Arc::ptr_eq(&current, &changed));
        assert_eq!(denied.attempts(), 0);
    }
    host.shutdown().await
}

#[tokio::test]
async fn resource_write_during_domain_preparation_preserves_the_captured_read() -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    let before = runtime.ensure_context_resources().await?;
    runtime
        .inner
        .context_capture_checkpoint
        .after_resources
        .store(true, Ordering::SeqCst);
    let preparation = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .preview_prompt(
                    "hold this resource read".into(),
                    AuthorityClass::OperatorInstruction,
                )
                .await
        }
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        runtime.inner.context_capture_checkpoint.reached.notified(),
    )
    .await?;
    {
        let _writer = runtime.begin_context_resource_write()?;
        std::fs::write(runtime.agent_home().join("AGENTS.md"), "NEXT_RESOURCE_READ")?;
    }
    runtime
        .inner
        .context_capture_checkpoint
        .release
        .notify_one();
    let prompt = tokio::time::timeout(Duration::from_secs(5), preparation).await???;
    assert!(Arc::ptr_eq(
        &before,
        &prompt._resource_read_material.as_ref().unwrap().observation
    ));
    assert!(prompt
        .loaded_agents_md
        .agent_source
        .as_ref()
        .unwrap()
        .content
        .contains("RESOURCE_GUIDANCE"));
    let next = runtime
        .preview_prompt(
            "capture the new resource".into(),
            AuthorityClass::OperatorInstruction,
        )
        .await?;
    assert_eq!(
        next.loaded_agents_md.agent_source.as_ref().unwrap().content,
        "NEXT_RESOURCE_READ"
    );
    assert!(!Arc::ptr_eq(
        &before,
        &next._resource_read_material.as_ref().unwrap().observation
    ));
    host.shutdown().await
}

#[tokio::test]
async fn resource_conflicts_process_original_input_without_runtime_rebuild() -> Result<()> {
    for (configuration_change, conflicts) in [(false, 1), (false, 6), (true, 6)] {
        let (_home, host, runtime) = fixture().await?;
        let paused = runtime.pause_run_loop_for_monitor_test().await;
        let message = enqueue_resource_race(&runtime).await?;
        drop(paused);
        race_resource_changes(&runtime, conflicts, configuration_change).await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if runtime
                    .storage()
                    .latest_queue_entries()?
                    .iter()
                    .any(|entry| {
                        entry.message_id == message.id
                            && entry.status == QueueEntryStatus::Processed
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("the original queued input must process after resource churn")??;
        let events = runtime.storage().read_recent_events(256)?;
        assert!(events.iter().all(|event| !matches!(
            event.kind.as_str(),
            "agent_runtime_loop_failed" | "runtime_loop_recovered"
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "queue_entry_claimed"
                    && event.data["message_id"] == message.id)
                .count(),
            1
        );
        let loaded = host
            .try_get_loaded_runtime(runtime.instance_agent_id())
            .await
            .context("the original runtime must remain loaded")?;
        assert!(Arc::ptr_eq(&runtime.inner, &loaded.inner));
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn resource_conflict_wait_releases_owner_and_allows_shutdown_without_claim() -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    let paused = runtime.pause_run_loop_for_monitor_test().await;
    let message = enqueue_resource_race(&runtime).await?;
    drop(paused);
    race_resource_changes(&runtime, 3, false).await?;
    // The loop releases this read gate when it waits. A pending writer obtains
    // it before the next poll, keeping the source unclaimed for shutdown.
    let paused = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.pause_run_loop_for_monitor_test(),
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(5), runtime.request_service_shutdown()).await??;
    drop(paused);
    tokio::time::timeout(Duration::from_secs(5), host.shutdown()).await??;
    let queued = runtime
        .storage()
        .latest_queue_entries()?
        .into_iter()
        .find(|entry| entry.message_id == message.id)
        .context("the accepted source must remain durable")?;
    assert_eq!(queued.status, QueueEntryStatus::Queued);
    assert!(runtime
        .storage()
        .read_recent_events(256)?
        .iter()
        .all(|event| !matches!(
            event.kind.as_str(),
            "queue_entry_claimed"
                | "turn_started"
                | "agent_runtime_loop_failed"
                | "runtime_loop_recovered"
        )));
    Ok(())
}

async fn enqueue_resource_race(runtime: &RuntimeHandle) -> Result<MessageEnvelope> {
    runtime
        .inner
        .context_capture_checkpoint
        .before_resources
        .store(true, Ordering::SeqCst);
    runtime
        .enqueue(
            MessageEnvelope::new(
                runtime.instance_agent_id(),
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "retain this source through resource churn".into(),
                },
            )
            .with_admission(
                crate::types::MessageDeliverySurface::HttpControlPrompt,
                crate::types::AdmissionContext::ControlAuthenticated,
            ),
        )
        .await
}

async fn race_resource_changes(
    runtime: &RuntimeHandle,
    conflicts: usize,
    configuration_change: bool,
) -> Result<()> {
    for attempt in 0..conflicts {
        tokio::time::timeout(
            Duration::from_secs(10),
            runtime.inner.context_capture_checkpoint.reached.notified(),
        )
        .await
        .with_context(|| format!("resource conflict checkpoint {attempt}"))?;
        if configuration_change {
            runtime.inner.resources.gate.publish_config(|| {
                let mut config = runtime.inner.config_snapshot.load_full().as_ref().clone();
                config.resource_generation += 1;
                runtime.inner.config_snapshot.store(Arc::new(config));
                runtime.inner.resources.registry.invalidate(
                    super::resource_observation::ResourceInvalidation::ConfigurationChange,
                )?;
                Ok(())
            })?;
        } else {
            // Each real write finishes before capture resumes. No error is
            // injected and no physical writer is held across the retry.
            let _writer = runtime.begin_context_resource_write()?;
            std::fs::write(
                runtime.agent_home().join("AGENTS.md"),
                format!("# Completed resource write {attempt}\n"),
            )?;
        }
        assert!(runtime
            .storage()
            .read_recent_events(256)?
            .iter()
            .all(|event| event.kind != "queue_entry_claimed"));
        runtime
            .inner
            .context_capture_checkpoint
            .before_resources
            .store(attempt + 1 < conflicts, Ordering::SeqCst);
        runtime
            .inner
            .context_capture_checkpoint
            .release
            .notify_one();
    }
    Ok(())
}

async fn capture_loaded_prompt(runtime: &RuntimeHandle, text: &str) -> Result<EffectivePrompt> {
    let message = MessageEnvelope::new(
        runtime.agent_id().await?,
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text { text: text.into() },
    );
    // Capture must reject stale resources, not run debug preparation's refresh.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.build_loaded_context_prompt(&message, None, None, false),
    )
    .await
    .expect("loaded capture must not wait for resource refresh")
}

#[tokio::test]
async fn actual_refresh_releases_template_scratch_before_retaining_large_skill_inventory(
) -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    {
        let _writer = runtime.begin_context_resource_write()?;
        let home = runtime.agent_home();
        for index in 0..64 {
            std::fs::create_dir_all(home.join(format!("skills/non-skill-{index}")))?;
        }
        let template = home.join("agent_templates/scratch-template");
        std::fs::create_dir_all(&template)?;
        std::fs::write(template.join("AGENTS.md"), "# Scratch template\n")?;
        std::fs::write(
            template.join("template.toml"),
            "schema='1'\nid='scratch-template'\nname='Scratch template'\n",
        )?;
        // The lexer reserves from input length even for this single comment.
        // The inventory alone fits the unchanged phase quota, but its retained
        // Host candidate and this parser's scratch cannot coexist at that quota.
        std::fs::write(
            template.join("skills.toml"),
            format!("skills=[]\n#{}", "a".repeat(7000)),
        )?;
    }
    runtime.refresh_context_resources().await?;
    let prompt = runtime
        .preview_prompt("registry hit".into(), AuthorityClass::OperatorInstruction)
        .await?;
    let observation = &prompt._resource_read_material.as_ref().unwrap().observation;
    assert!(observation
        .body()
        .skills
        .agent_templates_catalog
        .iter()
        .any(|entry| entry.template_id == "scratch-template"));
    assert!(observation
        .body()
        .skills
        .discoverable_skills
        .iter()
        .any(|entry| entry.name == "ResourceSkill"));
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn actual_loaded_context_uses_four_nonempty_resources_with_denied_readers() -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    let warm = runtime
        .preview_prompt("warm".into(), AuthorityClass::OperatorInstruction)
        .await?;
    let observation = warm
        ._resource_read_material
        .as_ref()
        .unwrap()
        .observation
        .clone();
    assert!(observation
        .body()
        .guidance
        .agent_source
        .as_ref()
        .unwrap()
        .content
        .contains("RESOURCE_GUIDANCE"));
    assert!(observation
        .body()
        .memory
        .operator_source
        .as_ref()
        .unwrap()
        .content
        .contains("RESOURCE_MEMORY"));
    assert!(observation
        .body()
        .notes
        .as_ref()
        .unwrap()
        .contains("RESOURCE_NOTE"));
    assert!(observation
        .body()
        .skills
        .discoverable_skills
        .iter()
        .any(|entry| entry.name == "ResourceSkill"));
    let denied = DenyResourceReaders::new();
    let prompt = runtime
        .preview_prompt("registry hit".into(), AuthorityClass::OperatorInstruction)
        .await?;
    assert_eq!(denied.attempts(), 0);
    assert!(Arc::ptr_eq(
        &observation,
        &prompt._resource_read_material.as_ref().unwrap().observation
    ));
    assert!(prompt
        .loaded_agents_md
        .agent_source
        .as_ref()
        .unwrap()
        .content
        .contains("RESOURCE_GUIDANCE"));
    assert!(prompt
        .loaded_agent_memory
        .operator_source
        .as_ref()
        .unwrap()
        .content
        .contains("RESOURCE_MEMORY"));
    assert!(prompt.rendered_context_attachment.contains("RESOURCE_NOTE"));
    drop(denied);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn explicit_context_preparation_reenters_racing_writer_and_bounds_repeated_conflicts(
) -> Result<()> {
    for writers in [1, 3] {
        let (_home, host, runtime) = fixture().await?;
        let before = runtime
            .preview_prompt(
                "before racing writer".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await?;
        runtime
            .inner
            .context_capture_checkpoint
            .before_resources
            .store(true, Ordering::SeqCst);
        let mut preparation = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .preview_prompt("racing writer".into(), AuthorityClass::OperatorInstruction)
                    .await
            }
        });
        for index in 0..writers {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                runtime.inner.context_capture_checkpoint.reached.notified(),
            )
            .await?;
            let writer = runtime.begin_context_resource_write()?;
            std::fs::write(
                runtime.agent_home().join("AGENTS.md"),
                format!("# RACING_RESOURCE_WRITER_{index}\n"),
            )?;
            runtime
                .inner
                .context_capture_checkpoint
                .before_resources
                .store(index + 1 < writers, Ordering::SeqCst);
            runtime
                .inner
                .context_capture_checkpoint
                .release
                .notify_one();
            if index < 2 {
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(50), &mut preparation)
                        .await
                        .is_err(),
                    "explicit preparation must await the physical writer"
                );
            }
            let owner = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                runtime.inner.mutation_owner.reserve(),
            )
            .await??;
            let refresh = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                runtime.hold_resource_refresh_for_test(),
            )
            .await??;
            drop((owner, refresh, writer));
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), preparation).await??;
        if writers == 1 {
            let prompt = result?;
            assert!(prompt
                .loaded_agents_md
                .agent_source
                .as_ref()
                .unwrap()
                .content
                .contains("RACING_RESOURCE_WRITER_0"));
        } else {
            let error = match result {
                Err(error) => error,
                Ok(_) => anyhow::bail!("three writer conflicts must remain a typed rejection"),
            };
            assert!(
                error
                    .downcast_ref::<crate::host::context_resources::ContextResourceWritersActive>()
                    .is_some(),
                "{error:#}"
            );
        }
        assert!(before
            .loaded_agents_md
            .agent_source
            .as_ref()
            .unwrap()
            .content
            .contains("RESOURCE_GUIDANCE"));
        host.shutdown().await?;
        assert!(runtime
            .preview_prompt(
                "Closing still rejects".into(),
                AuthorityClass::OperatorInstruction
            )
            .await
            .is_err());
    }
    Ok(())
}

#[tokio::test]
async fn explicit_resource_preparation_waits_for_writer_without_holding_owner_or_refresh(
) -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    let writer = runtime.begin_context_resource_write()?;
    std::fs::write(
        runtime.agent_home().join("AGENTS.md"),
        "# EXPLICIT_WRITER_HANDOFF\n",
    )?;
    assert!(
        capture_loaded_prompt(&runtime, "pure capture still refuses")
            .await
            .is_err()
    );
    let mut preparation = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.ensure_context_resources().await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut preparation)
            .await
            .is_err()
    );
    let owner = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runtime.inner.mutation_owner.reserve(),
    )
    .await??;
    let refresh = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runtime.hold_resource_refresh_for_test(),
    )
    .await??;
    drop((owner, refresh));
    drop(writer);
    let observation =
        tokio::time::timeout(std::time::Duration::from_secs(5), preparation).await???;
    assert!(observation
        .body()
        .guidance
        .agent_source
        .as_ref()
        .unwrap()
        .content
        .contains("EXPLICIT_WRITER_HANDOFF"));
    host.shutdown().await?;
    assert!(runtime
        .inner
        .resources
        .gate
        .wait_for_writers(&runtime.inner.resources.registry)
        .await
        .is_err());
    assert!(runtime.ensure_context_resources().await.is_err());
    Ok(())
}

#[tokio::test]
async fn actual_known_write_refuses_capture_and_preserves_old_prompt_and_clone() -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    let prompt = runtime
        .preview_prompt("before".into(), AuthorityClass::OperatorInstruction)
        .await?;
    let clone = prompt.clone();
    let old = prompt
        ._resource_read_material
        .as_ref()
        .unwrap()
        .observation
        .clone();
    let writer = runtime.begin_context_resource_write()?;
    std::fs::write(
        runtime.agent_home().join("AGENTS.md"),
        "NEW_RESOURCE_GUIDANCE\n",
    )?;
    assert!(capture_loaded_prompt(&runtime, "during").await.is_err());
    assert!(prompt
        .loaded_agents_md
        .agent_source
        .as_ref()
        .unwrap()
        .content
        .contains("RESOURCE_GUIDANCE"));
    drop(writer);
    let new = runtime.refresh_context_resources().await?;
    assert!(!Arc::ptr_eq(&old, &new));
    assert!(clone
        .loaded_agents_md
        .agent_source
        .as_ref()
        .unwrap()
        .content
        .contains("RESOURCE_GUIDANCE"));
    let after = runtime
        .preview_prompt("after".into(), AuthorityClass::OperatorInstruction)
        .await?;
    assert!(after
        .loaded_agents_md
        .agent_source
        .as_ref()
        .unwrap()
        .content
        .contains("NEW_RESOURCE_GUIDANCE"));
    host.shutdown().await?;
    assert!(runtime
        .preview_prompt("closed".into(), AuthorityClass::OperatorInstruction)
        .await
        .is_err());
    Ok(())
}

#[tokio::test]
async fn actual_owner_busy_keeps_ready_then_dirty_until_explicit_retry() -> Result<()> {
    use super::resource_observation::{ResourceFreshness, ResourceInvalidation};
    let (_home, host, runtime) = fixture().await?;
    let before = runtime
        .preview_prompt("before busy".into(), AuthorityClass::OperatorInstruction)
        .await?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    // Force I/O without changing the published source; Busy never poisons Ready.
    let source = before
        ._resource_read_material
        .as_ref()
        .unwrap()
        .observation
        .source()
        .clone();
    let registry = &runtime.inner.resources.registry;
    let error = registry
        .refresh(
            &source,
            || {
                crate::diagnostics::resource_io::charge_owned_metadata(4096, 64)?;
                Ok(Default::default())
            },
            || Ok(runtime.inner.mutation_owner.try_reserve()?),
            |_, _| Ok(()),
        )
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<mutation_owner::MutationOwnerError>(),
        Some(mutation_owner::MutationOwnerError::Busy)
    ));
    assert_eq!(registry.freshness()?, ResourceFreshness::Ready);
    registry.invalidate(ResourceInvalidation::ExplicitRefresh)?;
    assert!(runtime
        .try_refresh_context_resources_for_test()
        .await
        .is_err());
    assert_eq!(
        registry.freshness()?,
        ResourceFreshness::Dirty(ResourceInvalidation::ExplicitRefresh)
    );
    drop(owner);
    runtime.refresh_context_resources().await?;
    assert_eq!(registry.freshness()?, ResourceFreshness::Ready);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn actual_loaded_work_plan_is_a_zero_read_registry_hit() -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    let work = runtime
        .create_work_item(
            "loaded plan".into(),
            Some(WorkItemPlanStatus::NeedsInput),
            Some("# ORIGINAL_PLAN\nNonempty original body\n".into()),
            vec![],
        )
        .await?;
    runtime.pick_work_item(work.id.clone()).await?;
    runtime.refresh_context_resources().await?;
    let warm = runtime
        .preview_prompt("warm plan".into(), AuthorityClass::OperatorInstruction)
        .await?;
    assert!(warm.rendered_context_attachment.contains("ORIGINAL_PLAN"));
    let original = warm
        ._resource_read_material
        .as_ref()
        .unwrap()
        .observation
        .clone();
    let clone = warm.clone();
    let denied = DenyResourceReaders::new();
    let prompt = runtime
        .preview_prompt(
            "loaded plan is immutable".into(),
            AuthorityClass::OperatorInstruction,
        )
        .await?;
    assert_eq!(denied.attempts(), 0);
    assert!(prompt.rendered_context_attachment.contains("ORIGINAL_PLAN"));
    assert!(Arc::ptr_eq(
        &original,
        &prompt._resource_read_material.as_ref().unwrap().observation
    ));
    drop(denied);
    let path = work.plan_artifact.as_ref().unwrap().path.clone();
    std::fs::write(&path, "# EDITED_PLAN\nexternal edit\n")?;
    runtime.refresh_context_resources().await?;
    let edited = runtime
        .preview_prompt("edited".into(), AuthorityClass::OperatorInstruction)
        .await?;
    assert!(edited.rendered_context_attachment.contains("EDITED_PLAN"));
    assert!(clone.rendered_context_attachment.contains("ORIGINAL_PLAN"));
    std::fs::write(&path, "")?;
    let empty = runtime.refresh_context_resources().await?;
    let crate::work_item_plan::PlanArtifactQuery::Available(artifact) =
        &empty.body().plans[&work.id]
    else {
        anyhow::bail!("empty plan is an available observation");
    };
    assert_eq!(artifact.bytes, 0);
    std::fs::remove_file(&path)?;
    let missing = runtime.refresh_context_resources().await?;
    assert_eq!(
        missing.body().plans[&work.id],
        crate::work_item_plan::PlanArtifactQuery::Missing
    );
    assert!(!path.exists());
    let absent = runtime
        .preview_prompt("missing".into(), AuthorityClass::OperatorInstruction)
        .await?;
    assert!(!absent.rendered_context_attachment.contains("EDITED_PLAN"));
    assert!(!absent.rendered_context_attachment.contains("ORIGINAL_PLAN"));
    let unchanged = runtime
        .update_work_item_fields(work.id.clone(), None, None, None, None, None)
        .await?;
    assert_eq!(unchanged.objective, work.objective);
    assert!(!path.exists());
    assert!(runtime
        .update_work_item_fields(
            work.id.clone(),
            Some("must not recreate missing plan".into()),
            None,
            None,
            None,
            None,
        )
        .await
        .is_err());
    assert!(!path.exists());
    std::fs::create_dir(&path)?;
    assert!(runtime.refresh_context_resources().await.is_err());
    assert!(runtime
        .preview_prompt("unreadable".into(), AuthorityClass::OperatorInstruction)
        .await
        .is_err());
    std::fs::remove_dir(&path)?;
    std::fs::write(&path, "RESTORED_PLAN\n")?;
    runtime.refresh_context_resources().await?;
    host.shutdown().await?;
    assert!(clone.rendered_context_attachment.contains("ORIGINAL_PLAN"));
    drop((prompt, warm, original, clone));
    Ok(())
}

#[tokio::test]
async fn actual_work_plan_quota_rejects_before_file_work_or_receipt_side_effects() -> Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"},"runtime":{"state_memory":{"history_per_agent_bytes":1048576,"pending_per_agent_bytes":1048576}}}"#,
    )?;
    let mut config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = None;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let runtime = host.default_runtime().await?;
    runtime.await_initial_resources().await;
    let connection = rusqlite::Connection::open(runtime.inner.runtime_db.path())?;
    let counts = || -> Result<(i64, i64)> {
        Ok((
            connection.query_row("SELECT COUNT(*) FROM work_items", [], |row| row.get(0))?,
            connection.query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |row| {
                row.get(0)
            })?,
        ))
    };
    let original = counts()?;
    for repeats in [8 * 1024, 32 * 1024] {
        let plan = "budget plan\n".repeat(repeats);
        let work = runtime
            .create_work_item(
                "observational plan".into(),
                None,
                Some(plan.clone()),
                vec![],
            )
            .await?;
        assert_eq!(
            std::fs::read_to_string(work.plan_artifact.as_ref().unwrap().path.clone())?,
            plan
        );
    }
    assert_eq!(counts()?.0, original.0 + 2);
    assert!(counts()?.1 > original.1);
    let before = counts()?;
    let directory = runtime.agent_home().join("work-items");
    let existed = directory.exists();
    let children = || -> Result<Vec<_>> {
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut paths = std::fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        Ok(paths)
    };
    let previous = children()?;
    let header = runtime.context_resource_material()?.finish()?;
    let identity = runtime.agent_identity_view().await?;
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: identity.agent_id,
        identity_incarnation: identity.incarnation,
    };
    for repeats in [8 * 1024, 32 * 1024] {
        let material = crate::diagnostics::resource_io::ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &scope,
            header.0,
            header.1,
        )?;
        let failure = material.clone();
        let error = runtime
            .create_work_item_with_plan_material(
                "first file quota".into(),
                "budget plan\n".repeat(repeats),
                material,
            )
            .await
            .unwrap_err();
        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        assert_eq!(descriptor.code, "context_resource_exhausted");
        assert!(descriptor.retryable);
        assert!(failure.finish().is_err());
        assert_eq!(counts()?, before);
        assert_eq!(directory.exists(), existed);
        assert_eq!(children()?, previous);
    }
    let retry = runtime
        .create_work_item(
            "small retry".into(),
            None,
            Some("SMALL_PLAN".into()),
            vec![],
        )
        .await?;
    assert_eq!(
        std::fs::read_to_string(retry.plan_artifact.as_ref().unwrap().path.clone())?,
        "SMALL_PLAN"
    );
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn actual_cancelled_work_plan_file_keeps_physical_owner_until_shutdown_drains() -> Result<()>
{
    let (_home, host, runtime) = fixture().await?;
    let gate = host.context_resources();
    let pause = gate.pause_next_physical_writer();
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .create_work_item(
                    "cancel physical plan".into(),
                    None,
                    Some("PRESERVED_BODY".into()),
                    vec![],
                )
                .await
        }
    });
    pause.wait().await;
    assert!(gate.ready().is_err());
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let close = host.shutdown();
    tokio::pin!(close);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), close.as_mut())
            .await
            .is_err()
    );
    pause.release();
    close.await?;
    Ok(())
}

#[tokio::test]
async fn actual_registry_close_and_final_prompt_clone_release_scoped_and_host_material(
) -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    let work = runtime
        .create_work_item(
            "retained plan".into(),
            Some(WorkItemPlanStatus::NeedsInput),
            Some("FINAL_HOLDER_PLAN".into()),
            vec![],
        )
        .await?;
    runtime.pick_work_item(work.id).await?;
    runtime.refresh_context_resources().await?;
    let ledger = runtime.inner.input_evidence_budget.clone();
    let prompt = runtime
        .preview_prompt("pin original".into(), AuthorityClass::OperatorInstruction)
        .await?;
    let weak = Arc::downgrade(&prompt._resource_read_material.as_ref().unwrap().observation);
    let clone = prompt.clone();
    assert!(clone
        .rendered_context_attachment
        .contains("FINAL_HOLDER_PLAN"));
    let first_allocation = Arc::downgrade(
        prompt
            ._resource_read_material
            .as_ref()
            .unwrap()
            .allocation
            .as_ref()
            .unwrap(),
    );
    let second_allocation = Arc::downgrade(
        clone
            ._resource_read_material
            .as_ref()
            .unwrap()
            .allocation
            .as_ref()
            .unwrap(),
    );
    host.shutdown().await?;
    drop((runtime, host));
    assert!(weak.upgrade().is_some());
    let before = ledger.report()?.observed_bytes;
    drop(prompt);
    assert!(first_allocation.upgrade().is_none());
    assert!(second_allocation.upgrade().is_some());
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes < before);
    drop(clone);
    assert!(second_allocation.upgrade().is_none());
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn actual_source_ack_completes_during_refresh_io_and_late_source_cannot_install() -> Result<()>
{
    let (home, host, runtime) = fixture().await?;
    let pause = runtime.pause_next_resource_refresh();
    let refresh = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.refresh_context_resources().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), pause.wait()).await?;
    let root = home.path().join("changed-source");
    std::fs::create_dir_all(&root)?;
    std::fs::write(
        root.join("AGENTS.md"),
        "# CHANGED_SOURCE_GUIDANCE\nNew workspace source\n",
    )?;
    let workspace =
        crate::types::WorkspaceEntry::new("changed-resource-source", root.clone(), None);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        runtime.attach_workspace(&workspace).await?;
        runtime
            .enter_workspace(
                &workspace,
                crate::system::WorkspaceProjectionKind::CanonicalRoot,
                crate::system::WorkspaceAccessMode::ExclusiveWrite,
                None,
                None,
            )
            .await
    })
    .await??;
    assert!(capture_loaded_prompt(&runtime, "source not ready")
        .await
        .is_err());
    pause.release();
    let _obsolete_or_retried = refresh.await?;
    let observation = runtime.refresh_context_resources().await?;
    assert_eq!(
        observation.source().execution_root.as_deref(),
        Some(root.as_path())
    );
    assert!(observation
        .body()
        .guidance
        .workspace_source
        .as_ref()
        .unwrap()
        .content
        .contains("CHANGED_SOURCE_GUIDANCE"));
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn actual_config_and_local_invalidation_reject_io_candidates_frozen_before_publication(
) -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    for config_change in [true, false] {
        let pause = runtime.pause_next_resource_refresh();
        let refresh = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.refresh_context_resources().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), pause.wait()).await?;
        if config_change {
            runtime.inner.resources.gate.publish_config(|| {
                let mut config = runtime.inner.config_snapshot.load_full().as_ref().clone();
                config.resource_generation += 1;
                runtime.inner.config_snapshot.store(Arc::new(config));
                Ok(())
            })?;
        } else {
            let _writer = runtime.begin_context_resource_write()?;
            std::fs::write(
                runtime.agent_home().join("AGENTS.md"),
                "# LOCAL_INVALIDATION_GUIDANCE\n",
            )?;
        }
        assert!(capture_loaded_prompt(&runtime, "new generation not ready")
            .await
            .is_err());
        let mut preview = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .preview_prompt(
                        "prepare new generation".into(),
                        AuthorityClass::OperatorInstruction,
                    )
                    .await
            }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut preview)
                .await
                .is_err()
        );
        pause.release();
        let _obsolete_or_retried =
            tokio::time::timeout(std::time::Duration::from_secs(10), refresh).await??;
        let prepared =
            tokio::time::timeout(std::time::Duration::from_secs(10), preview).await???;
        assert_eq!(
            prepared
                ._resource_read_material
                .as_ref()
                .unwrap()
                .observation
                .source()
                .config_generation,
            runtime.inner.config_snapshot.load().resource_generation
        );
        let observation = runtime.refresh_context_resources().await?;
        assert_eq!(
            observation.source().config_generation,
            runtime.inner.config_snapshot.load().resource_generation
        );
        if !config_change {
            assert!(observation
                .body()
                .guidance
                .agent_source
                .as_ref()
                .unwrap()
                .content
                .contains("LOCAL_INVALIDATION_GUIDANCE"));
        }
    }
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn summary_re_prepares_after_the_published_source_moves_during_a_refresh() -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    runtime.ensure_context_resources().await?;
    // A refresh that loses the source race fails with the typed source change
    // and leaves the registry without a ready observation.
    let pause = runtime.pause_next_resource_refresh();
    let refresh = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.refresh_context_resources().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), pause.wait()).await?;
    runtime.inner.resources.gate.publish_config(|| {
        let mut config = runtime.inner.config_snapshot.load_full().as_ref().clone();
        config.resource_generation += 1;
        runtime.inner.config_snapshot.store(Arc::new(config));
        Ok(())
    })?;
    pause.release();
    let _lost_race = tokio::time::timeout(std::time::Duration::from_secs(10), refresh).await??;
    // The typed failure stays typed, so the summary re-prepares instead of
    // failing with an opaque refresh error.
    let summary = runtime.agent_summary().await?;
    let (summary, _material) = summary.into_parts();
    assert_eq!(summary.identity.agent_id, runtime.agent_id().await?);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn summary_resource_preparation_returns_retryable_for_an_in_flight_publication() -> Result<()>
{
    let (_home, host, runtime) = fixture().await?;
    let mut mutation = runtime.inner.mutation_owner.reserve().await?;
    mutation.begin("summary-resource-publication-race".into())?;
    let denied = DenyResourceReaders::new();
    let error = tokio::time::timeout(Duration::from_secs(1), runtime.agent_summary())
        .await?
        .err()
        .context("pending publication must not return a summary")?;
    assert!(error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    assert_eq!(denied.attempts(), 0);
    assert!(runtime.inner.mutation_owner.capture().is_err());
    mutation.definitely_not_committed()?;
    drop(mutation);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn context_resource_preparation_waits_for_an_in_flight_publication() -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    let mut mutation = runtime.inner.mutation_owner.reserve().await?;
    mutation.begin("resource-preparation-publication-race".into())?;
    let mut read = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.ensure_context_resources().await.map(|_| ()) }
    });
    let early = tokio::time::timeout(std::time::Duration::from_millis(50), &mut read).await;
    mutation.definitely_not_committed()?;
    drop(mutation);
    assert!(
        early.is_err(),
        "resource preparation must wait for an in-flight publication: {early:?}"
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), read).await???;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn loaded_context_re_prepares_after_the_published_source_moves_before_the_capture(
) -> Result<()> {
    for config_change in [true, false] {
        let (_home, host, runtime) = fixture().await?;
        runtime.ensure_context_resources().await?;
        // The capture stops right before the resource read, so the published source
        // can move between the preparation and the capture.
        runtime
            .inner
            .context_capture_checkpoint
            .before_resources
            .store(true, Ordering::SeqCst);
        let mut preparation = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .preview_prompt(
                        "after the source moved".into(),
                        AuthorityClass::OperatorInstruction,
                    )
                    .await
            }
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            runtime.inner.context_capture_checkpoint.reached.notified(),
        )
        .await?;
        if config_change {
            runtime.inner.resources.gate.publish_config(|| {
                let mut config = runtime.inner.config_snapshot.load_full().as_ref().clone();
                config.resource_generation += 1;
                runtime.inner.config_snapshot.store(Arc::new(config));
                Ok(())
            })?;
        } else {
            let _writer = runtime.begin_context_resource_write()?;
            std::fs::write(
                runtime.agent_home().join("AGENTS.md"),
                "WRITE_FINISHED_BEFORE_CAPTURE",
            )?;
        }

        runtime
            .inner
            .context_capture_checkpoint
            .release
            .notify_one();
        let prompt =
            tokio::time::timeout(std::time::Duration::from_secs(10), &mut preparation).await???;
        assert_eq!(
            prompt
                ._resource_read_material
                .as_ref()
                .unwrap()
                .observation
                .source()
                .config_generation,
            runtime.inner.config_snapshot.load().resource_generation
        );
        if !config_change {
            assert_eq!(
                prompt
                    .loaded_agents_md
                    .agent_source
                    .as_ref()
                    .unwrap()
                    .content,
                "WRITE_FINISHED_BEFORE_CAPTURE"
            );
        }
        host.shutdown().await?;
    }
    Ok(())
}
