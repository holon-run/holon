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
    for background in [false, true] {
        let started = home.path().join(format!("shell-started-{background}"));
        let release = home.path().join(format!("shell-release-{background}"));
        let before = runtime.inner.resources.gate.generation()?;
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
        let running_generation = runtime.inner.resources.gate.generation()?;
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
            while runtime.inner.resources.gate.generation().unwrap() == running_generation {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        assert_eq!(
            runtime.inner.resources.gate.generation()?,
            running_generation + 1
        );
    }
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
        .wait_for_writers()
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
    assert!(runtime.refresh_context_resources().await.is_err());
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
    for repeats in [8 * 1024, 32 * 1024] {
        let error = runtime
            .create_work_item(
                "first file quota".into(),
                None,
                Some("budget plan\n".repeat(repeats)),
                vec![],
            )
            .await
            .unwrap_err();
        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        assert_eq!(descriptor.code, "context_resource_exhausted");
        assert!(descriptor.retryable);
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
    assert!(gate.generation().is_err());
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
async fn actual_config_and_host_generation_reject_io_candidates_frozen_before_publication(
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
                "# HOST_GENERATION_GUIDANCE\n",
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
        assert_eq!(
            observation.source().host_generation,
            runtime.inner.resources.gate.generation()?
        );
        if !config_change {
            assert!(observation
                .body()
                .guidance
                .agent_source
                .as_ref()
                .unwrap()
                .content
                .contains("HOST_GENERATION_GUIDANCE"));
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

async fn resource_preparation_waits_for_publication(summary: bool) -> Result<()> {
    let (_home, host, runtime) = fixture().await?;
    let mut mutation = runtime.inner.mutation_owner.reserve().await?;
    mutation.begin("resource-preparation-publication-race".into())?;
    let mut read = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            if summary {
                runtime.agent_summary().await.map(|_| ())
            } else {
                runtime.ensure_context_resources().await.map(|_| ())
            }
        }
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
async fn summary_resource_preparation_waits_for_an_in_flight_publication() -> Result<()> {
    resource_preparation_waits_for_publication(true).await
}

#[tokio::test]
async fn context_resource_preparation_waits_for_an_in_flight_publication() -> Result<()> {
    resource_preparation_waits_for_publication(false).await
}

#[tokio::test]
async fn loaded_context_re_prepares_after_the_published_source_moves_before_the_capture(
) -> Result<()> {
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
    runtime.inner.resources.gate.publish_config(|| {
        let mut config = runtime.inner.config_snapshot.load_full().as_ref().clone();
        config.resource_generation += 1;
        runtime.inner.config_snapshot.store(Arc::new(config));
        Ok(())
    })?;
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
    host.shutdown().await?;
    Ok(())
}
