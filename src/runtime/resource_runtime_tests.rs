//! Real owner/Work/Context paths; refresh I/O is distinct from ordinary capture.
use super::resource_observation::{
    ResourceFreshness, ResourceInvalidation, ResourceObservationError,
};
use super::*;
use crate::runtime::tests::support::{context_config, tempdir};
use crate::types::{CommandTaskSpec, ExecCommandDuplicatePolicy, ExecCommandOutcome};

fn fixture(home: &Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new_isolated_resource_fixture(
        "resource-runtime",
        home.to_path_buf(),
        context_config(),
    )
}
fn message() -> MessageEnvelope {
    MessageEnvelope::new(
        "resource-runtime",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "render observed context".into(),
        },
    )
}

#[tokio::test]
async fn full_loaded_context_and_prompt_retain_observed_files_without_reader_capability(
) -> Result<()> {
    let home = tempdir()?;
    std::fs::write(home.path().join("AGENTS.md"), "resource-guidance-original")?;
    std::fs::create_dir_all(home.path().join("memory"))?;
    std::fs::write(
        home.path().join("memory/self.md"),
        "resource-memory-original",
    )?;
    std::fs::create_dir_all(home.path().join("notes"))?;
    std::fs::write(
        home.path().join("notes/observed.md"),
        "# resource-note-original\noriginal note summary",
    )?;
    std::fs::create_dir_all(home.path().join("skills/observed"))?;
    std::fs::write(
        home.path().join("skills/observed/SKILL.md"),
        "---\nname: observed-skill\ndescription: resource-skill-original\n---\nbody original",
    )?;
    let runtime = fixture(home.path())?;
    let work = runtime
        .create_work_item(
            "observed work".into(),
            None,
            Some("resource-plan-original".into()),
            Vec::new(),
        )
        .await?;
    runtime.pick_work_item(work.id.clone()).await?;
    // Warm the actual same-owner domains before denying only file readers.
    let initial = runtime
        .build_loaded_context_prompt(&message(), None, None, false)
        .await?;
    assert!(initial.resource_pins.is_some());
    assert!(initial
        .resource_pins
        .as_ref()
        .unwrap()
        .observation
        .body
        .skills
        .discoverable_skills
        .iter()
        .any(|skill| skill.description == "resource-skill-original"));
    // The original skills_catalog is a dynamic context attachment section.
    assert!(initial
        .rendered_context_attachment
        .contains("resource-skill-original"));
    assert!(initial
        .rendered_context_attachment
        .contains("resource-note-original"));
    assert!(initial
        .rendered_context_attachment
        .contains("resource-plan-original"));
    let pause = runtime.pause_resource_observer_for_test().await;
    std::fs::write(home.path().join("AGENTS.md"), "resource-guidance-edited")?;
    std::fs::write(
        crate::work_item_plan::plan_path(home.path(), &work.id),
        "resource-plan-edited",
    )?;
    // An unobserved external edit does not rewrite an existing observation.
    let frozen = crate::diagnostics::resource_io::deny_readers(
        runtime.build_loaded_context_prompt(&message(), None, None, false),
    )
    .await?;
    assert_eq!(
        frozen
            .loaded_agents_md
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "resource-guidance-original"
    );
    assert!(frozen
        .rendered_context_attachment
        .contains("resource-plan-original"));
    assert!(!frozen
        .rendered_context_attachment
        .contains("resource-plan-edited"));
    let old = frozen.resource_pins.as_ref().unwrap().clone();
    let old_epoch = old.observation.epoch;
    runtime.invalidate_context_resources(ResourceInvalidation::KnownWrite)?;
    assert!(matches!(
        runtime
            .capture_current_context_resources()
            .unwrap_err()
            .downcast_ref::<ResourceObservationError>(),
        Some(ResourceObservationError::NotReady(
            ResourceFreshness::Dirty(_)
        ))
    ));
    drop(pause);
    runtime.refresh_context_resources().await?;
    let fresh = crate::diagnostics::resource_io::deny_readers(runtime.build_loaded_context_prompt(
        &message(),
        None,
        None,
        false,
    ))
    .await?;
    assert!(fresh.resource_pins.as_ref().unwrap().observation.epoch > old_epoch);
    assert_eq!(
        fresh
            .loaded_agents_md
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "resource-guidance-edited"
    );
    assert!(fresh
        .rendered_context_attachment
        .contains("resource-plan-edited"));
    assert_eq!(
        old.observation
            .body
            .guidance
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "resource-guidance-original"
    );
    runtime.stop_context_resource_observer().await?;
    assert!(matches!(
        runtime.inner.context_resources.freshness()?,
        ResourceFreshness::Closed
    ));
    // The actual prepared prompt owns its old immutable data after release.
    assert_eq!(
        frozen
            .loaded_agent_memory
            .self_source
            .as_ref()
            .unwrap()
            .content,
        "resource-memory-original"
    );
    Ok(())
}

#[tokio::test]
async fn absent_plan_render_never_creates_a_file_and_observed_optional_failure_is_distinct(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = fixture(home.path())?;
    runtime.refresh_context_resources().await?;
    let mut item = WorkItemRecord::new("resource-runtime", "unrecorded plan", WorkItemState::Open);
    let plan_path = crate::work_item_plan::plan_path(home.path(), &item.id);
    let capture = runtime.capture_current_context_resources()?;
    let plan = crate::diagnostics::resource_io::deny_readers(async {
        crate::context::ContextResources::plan(capture.as_ref(), &item)
    })
    .await;
    assert!(plan.is_none());
    assert!(!plan_path.exists());
    // The explicit file workflow creates the actual artifact; render never does.
    let (artifact, material) =
        runtime.create_work_plan_file(&item, Some("actual workflow plan"))?;
    item.plan_artifact = Some(artifact);
    assert!(plan_path.is_file());
    drop(material);
    std::fs::create_dir(home.path().join("notes"))?;
    std::fs::remove_dir(home.path().join("notes"))?;
    std::fs::write(
        home.path().join("notes"),
        "a file cannot be a notes directory",
    )?;
    runtime.refresh_context_resources().await?;
    let failed = runtime.capture_current_context_resources()?;
    assert!(failed.observation.body.notes.is_none());
    assert!(failed.observation.body.notes_refresh_error.is_some());
    assert_eq!(
        failed.observation.source.scope.identity_incarnation,
        runtime.instance_incarnation()
    );
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn filesystem_refresh_holds_no_owner_and_late_workspace_install_is_fenced() -> Result<()> {
    let home = tempdir()?;
    let workspace = tempdir()?;
    std::fs::write(home.path().join("AGENTS.md"), "old original-home guidance")?;
    std::fs::write(workspace.path().join("AGENTS.md"), "new workspace guidance")?;
    let runtime = fixture(home.path())?;
    runtime.refresh_context_resources().await?;
    let old = runtime.capture_current_context_resources()?;
    let old_root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("original resource root missing")?;
    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let refresh_runtime = runtime.clone();
    let late = tokio::spawn(async move {
        refresh_runtime
            .refresh_resources_with_checkpoint_for_test(reached_tx, resume_rx)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), reached_rx).await??;
    // This real canonical State ACK must finish while file refresh is paused.
    let path = workspace.path().to_path_buf();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.update_agent_state(move |state| {
            state.attached_workspaces.push("resource-workspace".into());
            state.active_workspace_entry = Some(ActiveWorkspaceEntry {
                workspace_id: "resource-workspace".into(),
                workspace_anchor: path.clone(),
                execution_root_id: "resource-workspace:canonical".into(),
                execution_root: path.clone(),
                cwd: path.clone(),
                projection_kind: WorkspaceProjectionKind::CanonicalRoot,
                projection_metadata: None,
                occupancy_id: None,
                access_mode: crate::system::WorkspaceAccessMode::SharedRead,
            });
            Ok(())
        }),
    )
    .await??;
    resume_tx.send(())?;
    let error = late.await?.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::ObsoleteRefresh)
        ),
        "{error:#}"
    );
    assert_eq!(
        old.observation
            .body
            .guidance
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "old original-home guidance"
    );
    runtime.refresh_context_resources().await?;
    let current = runtime.capture_current_context_resources()?;
    assert_eq!(
        current.observation.source.workspace_id.as_deref(),
        Some("resource-workspace")
    );
    assert_eq!(
        current
            .observation
            .body
            .guidance
            .workspace_source
            .as_ref()
            .unwrap()
            .content,
        "new workspace guidance"
    );
    assert!(current.observation.epoch > old.observation.epoch);
    let stale = runtime
        .capture_context_resources_for_snapshot(&old_root.state, old_root.work.as_deref(), None)
        .unwrap_err();
    assert!(
        matches!(
            stale.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::SourceChanged)
        ),
        "capture must validate supplied old publication, rather than substitute latest State/Work"
    );
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn external_observer_advances_latest_observed_and_shutdown_awaits_it() -> Result<()> {
    let home = tempdir()?;
    std::fs::write(home.path().join("AGENTS.md"), "observer original")?;
    let runtime = fixture(home.path())?;
    runtime.refresh_context_resources().await?;
    let old = runtime.capture_current_context_resources()?;
    std::fs::write(home.path().join("AGENTS.md"), "observer external edit")?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let notified = runtime.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Ok(current) = runtime.capture_current_context_resources() {
                if current.observation.epoch > old.observation.epoch {
                    assert_eq!(
                        current
                            .observation
                            .body
                            .guidance
                            .agent_source
                            .as_ref()
                            .unwrap()
                            .content,
                        "observer external edit"
                    );
                    break;
                }
            }
            notified.await;
        }
    })
    .await?;
    runtime.stop_context_resource_observer().await?;
    assert!(runtime
        .refresh_context_resources()
        .await
        .unwrap_err()
        .downcast_ref::<ResourceObservationError>()
        .is_some());
    assert_eq!(
        old.observation
            .body
            .guidance
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "observer original"
    );
    Ok(())
}

#[tokio::test]
async fn summary_core_does_not_retain_unselected_plan_bodies_and_snapshot_never_recaptures_latest_work(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = fixture(home.path())?;
    let one = runtime
        .create_work_item(
            "selected resource work".into(),
            None,
            Some("selected original plan".into()),
            Vec::new(),
        )
        .await?;
    let two = runtime
        .create_work_item(
            "other resource work".into(),
            None,
            Some("unselected other plan".into()),
            Vec::new(),
        )
        .await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("resource test root missing")?;
    let resident = root.work.as_ref().context("resource test Work missing")?;
    let mut request = super::resident_work::WorkCaptureRequest::current();
    request.include_live = false;
    request.nonopen_limit = 0;
    request
        .mandatory_roots
        .insert(super::resident_work::WorkKey::new(
            super::resident_work::WorkFamily::Work,
            &one.id,
        ));
    let selected_work = resident.capture(&request)?;
    assert_eq!(selected_work.records.len(), 1);
    let summary =
        runtime.capture_context_resources_for_snapshot(&root.state, Some(resident), None)?;
    let selected = runtime.capture_context_resources_for_snapshot(
        &root.state,
        Some(resident),
        Some(&selected_work),
    )?;
    let one_weak = runtime
        .inner
        .context_resources
        .plan_weak_for_test(&one.id)?
        .context("selected plan not observed")?;
    let two_weak = runtime
        .inner
        .context_resources
        .plan_weak_for_test(&two.id)?
        .context("other plan not observed")?;
    assert!(crate::context::ContextResources::plan(&summary, &one).is_none());
    assert!(crate::context::ContextResources::plan(&selected, &one).is_some());
    assert!(crate::context::ContextResources::plan(&selected, &two).is_none());
    drop(root);
    drop(selected_work);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    assert!(one_weak.upgrade().is_some());
    assert!(
        two_weak.upgrade().is_none(),
        "core summary must not keep unrelated resource plan body alive"
    );
    drop(selected);
    assert!(one_weak.upgrade().is_none());
    assert!(summary.observation.body.guidance.agent_source.is_none());
    Ok(())
}

#[tokio::test]
async fn temporary_execution_resources_are_prepared_once_and_render_does_not_read_files(
) -> Result<()> {
    let home = tempdir()?;
    let workspace = tempdir()?;
    std::fs::write(
        home.path().join("AGENTS.md"),
        "active original home guidance",
    )?;
    std::fs::write(
        workspace.path().join("AGENTS.md"),
        "temporary execution guidance",
    )?;
    let runtime = fixture(home.path())?;
    runtime.refresh_context_resources().await?;
    let active = runtime.capture_current_context_resources()?;
    let view = crate::system::WorkspaceView::new(
        Some("resource-temporary".into()),
        workspace.path().to_path_buf(),
        workspace.path().to_path_buf(),
        workspace.path().to_path_buf(),
        Some("resource-temporary:execution".into()),
        Some(crate::system::WorkspaceAccessMode::SharedRead),
        WorkspaceProjectionKind::CanonicalRoot,
        None,
    )?;
    let execution = runtime
        .effective_execution_for_workspace(ExecutionScopeKind::SubagentTask, view)
        .await?;
    let (resources, skills) = runtime
        .prepare_subagent_resource_observation(&execution)
        .await?;
    std::fs::write(
        workspace.path().join("AGENTS.md"),
        "unobserved changed execution guidance",
    )?;
    let prompt =
        crate::diagnostics::resource_io::deny_readers(runtime.build_subagent_prompt_for_workspace(
            "resource-preview",
            "pure resource preview",
            &AuthorityClass::OperatorInstruction,
            &execution,
            resources,
            &skills,
        ))
        .await?;
    assert_eq!(
        prompt
            .loaded_agents_md
            .workspace_source
            .as_ref()
            .unwrap()
            .content,
        "temporary execution guidance"
    );
    assert!(prompt.resource_pins.is_some());
    let current = runtime.capture_current_context_resources()?;
    assert_eq!(
        active.observation.source.workspace_id, current.observation.source.workspace_id,
        "transient source must not overwrite active workspace registry"
    );
    let actual = runtime.agent_state().await?;
    assert_eq!(
        current.observation.source.workspace_id.as_deref(),
        actual
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.workspace_id.as_str())
    );
    assert_ne!(
        current.observation.source.workspace_id.as_deref(),
        Some("resource-temporary")
    );
    assert_eq!(
        current.observation.source.scope,
        active.observation.source.scope
    );
    runtime.stop_context_resource_observer().await?;
    assert_eq!(
        prompt
            .loaded_agents_md
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "active original home guidance"
    );
    Ok(())
}

#[tokio::test]
async fn cancelled_caller_cannot_end_a_detached_physical_resource_write() -> Result<()> {
    let home = tempdir()?;
    std::fs::write(
        home.path().join("AGENTS.md"),
        "before original physical writer",
    )?;
    let runtime = fixture(home.path())?;
    runtime.refresh_context_resources().await?;
    let old = runtime.capture_current_context_resources()?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let write = runtime
        .begin_context_resource_write()?
        .context("original writable resource observation absent")?;
    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let (physical_tx, physical_rx) = tokio::sync::oneshot::channel();
    let file = home.path().join("AGENTS.md");
    let caller = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || -> Result<()> {
            let original_write = write;
            std::fs::write(&file, "partial original physical writer")?;
            reached_tx
                .send(())
                .map_err(|_| anyhow!("writer checkpoint caller gone"))?;
            resume_rx.recv_timeout(std::time::Duration::from_secs(5))?;
            std::fs::write(&file, "after original physical writer")?;
            drop(original_write);
            let _ = physical_tx.send(());
            Ok(())
        })
        .await??;
        Ok::<_, anyhow::Error>(())
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), reached_rx).await??;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(runtime.capture_current_context_resources().is_err());
    drop(pause);
    let rejected = runtime.refresh_context_resources().await.unwrap_err();
    assert!(matches!(
        rejected.downcast_ref::<ResourceObservationError>(),
        Some(ResourceObservationError::NotReady(
            ResourceFreshness::Dirty(_)
        ))
    ));
    assert_eq!(
        old.observation
            .body
            .guidance
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "before original physical writer"
    );
    resume_tx.send(())?;
    tokio::time::timeout(std::time::Duration::from_secs(5), physical_rx).await??;
    runtime.refresh_context_resources().await?;
    let current = runtime.capture_current_context_resources()?;
    assert_eq!(
        current
            .observation
            .body
            .guidance
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "after original physical writer"
    );
    assert_ne!(current.observation.digest, old.observation.digest);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[cfg(unix)]
fn resource_command(cmd: String, yield_time_ms: u64) -> CommandTaskSpec {
    CommandTaskSpec {
        cmd,
        workdir: None,
        shell: Some("sh".into()),
        login: false,
        tty: false,
        yield_time_ms,
        max_output_tokens: Some(1024),
        accepts_input: false,
        terminal_reentry: false,
    }
}
#[cfg(unix)]
#[tokio::test]
async fn foreground_actual_process_exit_refreshes_next_context_guidance() -> Result<()> {
    let home = tempdir()?;
    std::fs::write(home.path().join("AGENTS.md"), "foreground before")?;
    let runtime = fixture(home.path())?;
    runtime.refresh_context_resources().await?;
    for code in [0, 7] {
        let command = resource_command(
            format!("printf 'foreground after {code}' > AGENTS.md; exit {code}"),
            5000,
        );
        let result = runtime
            .execute_exec_command(
                command,
                ExecCommandDuplicatePolicy::StartNew,
                &AuthorityClass::OperatorInstruction,
                None,
            )
            .await?;
        assert!(
            matches!(result.outcome,ExecCommandOutcome::Completed {exit_status:Some(actual),..} if actual==code)
        );
        // A successful wait/status observation is physical exit even at exit7;
        // it is not the narrower status.success()/exit-zero meaning.
        let prompt = runtime
            .build_loaded_context_prompt(&message(), None, None, false)
            .await?;
        assert_eq!(
            prompt
                .loaded_agents_md
                .agent_source
                .as_ref()
                .unwrap()
                .content,
            format!("foreground after {code}")
        );
    }
    assert_eq!(
        runtime
            .inner
            .context_resources
            .actual_command_exits_for_test()?,
        2
    );
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn background_command_caller_cancel_keeps_context_available_until_actual_exit_hook(
) -> Result<()> {
    let home = tempdir()?;
    std::fs::write(home.path().join("AGENTS.md"), "background before")?;
    let runtime = fixture(home.path())?;
    runtime.refresh_context_resources().await?;
    let (send, receive) = tokio::sync::oneshot::channel();
    let caller_runtime = runtime.clone();
    let caller = tokio::spawn(async move {
        let command=resource_command("while [ ! -f release-resource-command ]; do sleep 0.01; done; printf 'background after' > AGENTS.md".into(),0);
        let result = caller_runtime
            .execute_exec_command(
                command,
                ExecCommandDuplicatePolicy::StartNew,
                &AuthorityClass::OperatorInstruction,
                None,
            )
            .await;
        let _ = send.send(result);
        std::future::pending::<()>().await;
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), receive).await???;
    assert!(matches!(
        result.outcome,
        ExecCommandOutcome::PromotedToTask { .. }
    ));
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    runtime.refresh_context_resources().await?;
    let prompt = crate::diagnostics::resource_io::deny_readers(
        runtime.build_loaded_context_prompt(&message(), None, None, false),
    )
    .await?;
    assert_eq!(
        prompt
            .loaded_agents_md
            .agent_source
            .as_ref()
            .unwrap()
            .content,
        "background before"
    );
    assert_eq!(
        runtime
            .inner
            .context_resources
            .actual_command_exits_for_test()?,
        0,
        "caller cancel / background registration is not an OS-exit observation"
    );
    std::fs::write(home.path().join("release-resource-command"), "")?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let notified = runtime.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if runtime
                .inner
                .context_resources
                .actual_command_exits_for_test()?
                == 1
            {
                return Ok::<_, anyhow::Error>(());
            }
            notified.await;
        }
    })
    .await??;
    let next = runtime
        .build_loaded_context_prompt(&message(), None, None, false)
        .await?;
    assert_eq!(
        next.loaded_agents_md.agent_source.as_ref().unwrap().content,
        "background after"
    );
    assert_eq!(
        runtime
            .inner
            .context_resources
            .actual_command_exits_for_test()?,
        1
    );
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn actual_host_write_between_resource_read_and_install_rejects_late_generation() -> Result<()>
{
    let home = tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    config.user_home_dir = Some(home.path().to_path_buf());
    let host = crate::host::RuntimeHost::new_with_provider(
        config,
        Arc::new(crate::provider::StubProvider::new("done")),
    )?;
    host.ensure_default_agent_identity()?;
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    let path = runtime.agent_home().join("AGENTS.md");
    {
        let write = host.begin_loaded_context_resource_write().await?;
        std::fs::write(&path, "# before the actual Host write")?;
        drop(write);
    }
    runtime.refresh_context_resources().await?;
    let old = runtime.capture_current_context_resources()?;
    let original_generation = old.observation.source.host_write_generation;
    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let late_runtime = runtime.clone();
    let late = tokio::spawn(async move {
        late_runtime
            .refresh_resources_with_checkpoint_for_test(reached_tx, resume_rx)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), reached_rx).await??;
    let write = host.begin_loaded_context_resource_write().await?;
    assert!(runtime.capture_current_context_resources().is_err());
    std::fs::write(&path, "# after the actual Host write")?;
    drop(write);
    resume_tx.send(())?;
    let error = late.await?.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ResourceObservationError>(),
        Some(ResourceObservationError::ObsoleteRefresh)
    ));
    assert_eq!(
        old.guidance().agent_source.as_ref().unwrap().content,
        "# before the actual Host write"
    );
    runtime.refresh_context_resources().await?;
    let observed = runtime.capture_current_context_resources()?;
    assert_ne!(
        observed.observation.source.host_write_generation,
        original_generation
    );
    assert_eq!(
        observed.guidance().agent_source.as_ref().unwrap().content,
        "# after the actual Host write"
    );
    host.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_refresh_owner_contention_preserves_ready_or_known_dirty_until_actual_retry(
) -> Result<()> {
    let home = tempdir()?;
    std::fs::write(
        home.path().join("AGENTS.md"),
        "original contention guidance",
    )?;
    let runtime = fixture(home.path())?;
    runtime.refresh_context_resources().await?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    for changed in [false, true] {
        if changed {
            std::fs::write(
                home.path().join("AGENTS.md"),
                "changed during actual refresh",
            )?;
        }
        let original = runtime.capture_current_context_resources()?;
        let (reached, seen) = tokio::sync::oneshot::channel();
        let (resume, wait) = std::sync::mpsc::channel();
        let (busy, blocked) = tokio::sync::oneshot::channel();
        let worker_runtime = runtime.clone();
        let worker = tokio::spawn(async move {
            worker_runtime
                .refresh_context_resources_with_contention_barrier_for_test(reached, wait, busy)
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), seen).await??;
        let held = runtime.inner.mutation_owner.reserve().await?;
        resume.send(())?;
        tokio::time::timeout(std::time::Duration::from_secs(5), blocked).await??;
        assert!(
            !worker.is_finished(),
            "refresh must await actual owner release"
        );
        if changed {
            assert!(matches!(
                runtime.inner.context_resources.freshness()?,
                ResourceFreshness::Dirty(ResourceInvalidation::ExternalEdit)
            ));
            assert!(runtime.capture_current_context_resources().is_err());
        } else {
            assert_eq!(
                runtime.inner.context_resources.freshness()?,
                ResourceFreshness::Ready
            );
            let kept = runtime.capture_current_context_resources()?;
            assert!(Arc::ptr_eq(&original.observation, &kept.observation));
        }
        assert_eq!(
            original
                .observation
                .body
                .guidance
                .agent_source
                .as_ref()
                .unwrap()
                .content,
            "original contention guidance"
        );
        drop(held);
        tokio::time::timeout(std::time::Duration::from_secs(5), worker).await???;
        let ready = runtime.capture_current_context_resources()?;
        assert_eq!(
            runtime.inner.context_resources.freshness()?,
            ResourceFreshness::Ready
        );
        assert_eq!(
            ready
                .observation
                .body
                .guidance
                .agent_source
                .as_ref()
                .unwrap()
                .content,
            if changed {
                "changed during actual refresh"
            } else {
                "original contention guidance"
            }
        );
    }
    runtime.stop_context_resource_observer().await?;
    Ok(())
}
