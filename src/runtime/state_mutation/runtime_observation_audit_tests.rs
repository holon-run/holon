use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadError, CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
};
use crate::types::{CommandTaskSpec, WorkspaceEntry};
use std::time::Duration;

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "original-command-work-skill-audits",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    Ok((home, runtime))
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM audit_events),
         (SELECT COUNT(*) FROM current_read_members WHERE family='event'),
         (SELECT COUNT(*) FROM current_event_native_births),
         (SELECT COUNT(*) FROM input_mutation_receipts)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?)
}
fn selected(runtime: &RuntimeHandle, kind: &str) -> Result<Vec<AuditEvent>> {
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("original Root absent")?;
    let current = root
        .current_reads
        .as_ref()
        .context("actual Current absent")?
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    Ok(current
        .events
        .iter()
        .filter_map(|r| match &r.fact {
            CurrentReadFact::Event(event) if event.kind == kind => Some(event.clone()),
            _ => None,
        })
        .collect())
}

#[tokio::test]
async fn original_work_warning_public_ack_keeps_exact_fields_and_pure_current_after_sql_denial(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let baseline = counts(&runtime)?;
    runtime
        .record_work_item_completion_warning(
            "arbitrary-observed-work\0id".into(),
            "missing_completion_report",
            "original warning text",
            Some(7),
            Some(3),
        )
        .await?;
    let events = selected(&runtime, "work_item_completion_warning")?;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].data,
        serde_json::json!({
            "agent_id":runtime.instance_agent_id(), "work_item_id":"arbitrary-observed-work\0id",
            "kind":"missing_completion_report", "message":"original warning text",
            "source_turn_index":7,"source_round":3,
        })
    );
    assert_eq!(
        counts(&runtime)?,
        (
            baseline.0 + 1,
            baseline.1 + 1,
            baseline.2 + 1,
            baseline.3 + 1
        )
    );
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(root.state, before.state);
    assert_eq!(
        root.current_reads.as_ref().unwrap().coverage().revision,
        before.current_reads.as_ref().unwrap().coverage().revision + 1
    );
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE audit_events RENAME TO original_observation_body_forbidden")?;
    let pure = selected(&runtime, "work_item_completion_warning");
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE original_observation_body_forbidden RENAME TO audit_events")?;
    assert_eq!(pure?, events);
    Ok(())
}

#[tokio::test]
async fn registered_work_warning_survives_caller_abort_and_closing_without_new_inner_admission(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let owner = runtime.begin_mutation().await?;
    let baseline = counts(&runtime)?;
    let mut caller = Box::pin(runtime.record_work_item_completion_warning(
        "original-work".into(),
        "original-kind",
        "accepted-before-closing",
        None,
        None,
    ));
    assert!(futures_util::poll!(&mut caller).is_pending());
    drop(caller);
    runtime.close_mutation_admission();
    assert!(runtime
        .record_work_item_completion_warning(
            "later-work".into(),
            "later-kind",
            "must reject",
            None,
            None
        )
        .await
        .is_err());
    drop(owner);
    runtime.join_owned_mutations().await?;
    let events = selected(&runtime, "work_item_completion_warning")?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data["message"], "accepted-before-closing");
    assert_eq!(
        counts(&runtime)?,
        (
            baseline.0 + 1,
            baseline.1 + 1,
            baseline.2 + 1,
            baseline.3 + 1
        )
    );
    // The existing turn continuation already owns its later warning, even now.
    runtime
        .record_owned_work_item_completion_warning(
            "original-turn-work".into(),
            "turn-kind",
            "owned-turn-continues",
            Some(8),
            None,
        )
        .await?;
    assert_eq!(selected(&runtime, "work_item_completion_warning")?.len(), 2);
    Ok(())
}

#[tokio::test]
async fn work_warning_rollback_known_ack_and_source_binding_preserve_the_original_event_once(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let baseline = counts(&runtime)?;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    runtime.runtime_db().connection()?.execute_batch(
        "CREATE TRIGGER reject_original_work_warning BEFORE INSERT ON audit_events
         WHEN NEW.kind='work_item_completion_warning'
         BEGIN SELECT RAISE(ABORT,'original Work warning rollback'); END;",
    )?;
    let error = runtime
        .record_work_item_completion_warning("observed-work".into(), "kind", "rollback", None, None)
        .await
        .err()
        .context("Audit unexpectedly committed")?;
    assert!(format!("{error:#}").contains("original Work warning rollback"));
    assert_eq!(counts(&runtime)?, baseline);
    assert!(Arc::ptr_eq(
        &before,
        &runtime.inner.mutation_owner.capture()?.unwrap()
    ));
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("DROP TRIGGER reject_original_work_warning")?;
    let audit = runtime.prepare_original_work_completion_warning(
        "observed-work",
        "kind",
        "known",
        None,
        None,
    )?;
    let id = audit.event.id.clone();
    let frozen = audit.event.clone();
    let weak = Arc::downgrade(&audit.source._material);
    let mut owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    let prepared =
        crate::runtime_db::transitions::PreparedInputTransition::runtime_observation_audit(
            owner.input_evidence_scope(),
            audit.event,
            root.state.clone(),
            audit.source,
            Some(TransitionFaultPoint::BeforeCacheUpdate),
        )?;
    drop(root);
    let error = owner
        .commit_prepared_input_transition(prepared)
        .await
        .err()
        .context("actual Known fault absent")?;
    drop(owner);
    assert!(
        matches!(
            runtime.inner.mutation_owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ),
        "{error:#}"
    );
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let saved = reservation.recovery()?.unwrap().known.unwrap();
    let crate::runtime::mutation_owner::KnownCommit::State {
        current_reads: Some(actual),
        state,
        ..
    } = &saved
    else {
        panic!("actual observation ACK lacks Current source")
    };
    actual.validate_state(state)?;
    let actual = actual.clone();
    drop(reservation);
    let committed = counts(&runtime)?;
    assert_eq!(
        committed,
        (
            baseline.0 + 1,
            baseline.1 + 1,
            baseline.2 + 1,
            baseline.3 + 1
        )
    );
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE audit_events RENAME TO observation_known_body_forbidden")?;
    let recovered = runtime.begin_mutation().await;
    let read = selected(&runtime, "work_item_completion_warning");
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE observation_known_body_forbidden RENAME TO audit_events")?;
    drop(recovered?);
    assert!(Arc::ptr_eq(
        runtime
            .inner
            .mutation_owner
            .capture()?
            .unwrap()
            .current_reads
            .as_ref()
            .unwrap(),
        &actual
    ));
    let event = read?
        .into_iter()
        .find(|e| e.id == id)
        .context("recovered event absent")?;
    assert_eq!(event.id, frozen.id);
    assert_eq!(event.created_at, frozen.created_at);
    assert_eq!(event.data, frozen.data);
    assert_eq!(counts(&runtime)?, committed);
    assert!(weak.upgrade().is_none());
    let huge = "x".repeat(9 * 1024 * 1024);
    let budget = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .prepare_original_work_completion_warning("id", "kind", &huge, None, None)
        .err()
        .context("first borrowed gate absent")?;
    assert!(matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::ResourceExhausted)
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        budget
    );
    assert_eq!(counts(&runtime)?, committed);
    let (_other_home, other) = fixture().await?;
    let audit =
        runtime.prepare_original_work_completion_warning("id", "kind", "foreign", None, None)?;
    let weak = Arc::downgrade(&audit.source._material);
    let other_before = counts(&other)?;
    assert!(other
        .finish_owned_runtime_observation_audit(audit)
        .await
        .is_err());
    assert_eq!(counts(&other)?, other_before);
    assert!(weak.upgrade().is_none());
    Ok(())
}

async fn workspace(
    runtime: &RuntimeHandle,
    home: &tempfile::TempDir,
) -> Result<std::path::PathBuf> {
    let path = home.path().join("observed-workspace");
    std::fs::create_dir(&path)?;
    let entry = WorkspaceEntry::new("original-observation-workspace", path.clone(), None);
    runtime.attach_workspace(&entry).await?;
    runtime
        .enter_workspace(
            &entry,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::ExclusiveWrite,
            None,
            None,
        )
        .await?;
    runtime.refresh_context_resources().await?;
    Ok(path)
}
#[tokio::test]
async fn actual_command_request_audit_precedes_task_created_and_keeps_original_process_payload(
) -> Result<()> {
    let (home, runtime) = fixture().await?;
    let path = workspace(&runtime, &home).await?;
    let cmd = "printf requested; while :; do sleep 1; done";
    let task = runtime
        .schedule_command_task(
            "original command request".into(),
            CommandTaskSpec {
                cmd: cmd.into(),
                workdir: Some(path.to_string_lossy().into()),
                shell: None,
                login: false,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: Some(128),
                accepts_input: false,
                terminal_reentry: false,
            },
            AuthorityClass::OperatorInstruction,
        )
        .await?;
    let lease = runtime
        .inner
        .task_handles
        .lock()
        .await
        .get(&task.id)
        .and_then(|h| h.control_lease())
        .context("actual Command monitor absent")?;
    let result:Result<()>=async {
        let events=selected(&runtime,"process_execution_requested")?;
        let event=events.iter().find(|e|e.data["task_id"]==task.id).context("requested Event not qualified")?;
        assert_eq!(event.data["surface"],"command_task");assert_eq!(event.data["cmd_display"],cmd);
        assert_eq!(event.data["authority_class"],serde_json::to_value(AuthorityClass::OperatorInstruction)?);
        assert_eq!(event.data["workdir"],serde_json::to_value(&path)?);
        assert_eq!(event.data["promoted_from_exec_command"],false);
        assert!(event.data["execution"].is_object());assert!(event.data["boundary"].is_object());
        let created: u64=runtime.runtime_db().connection()?.query_row(
            "SELECT MIN(event_seq) FROM audit_events WHERE kind='task_created' AND json_extract(data_json,'$.data.task_id')=?1",
            [&task.id],|r|r.get(0))?;
        assert!(event.event_seq < created);
        assert_eq!(runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM current_event_native_births WHERE event_id=?1",[&event.id],|r|r.get::<_,usize>(0))?,1);
        Ok(())
    }.await;
    lease.cancel();
    let cleanup = tokio::time::timeout(Duration::from_secs(5), lease.join()).await?;
    result?;
    cleanup
}

#[tokio::test]
async fn actual_skill_activation_keeps_state_ack_before_audit_and_does_not_rollback_state_on_audit_error(
) -> Result<()> {
    for rollback in [false, true] {
        let (home, runtime) = fixture().await?;
        let path = workspace(&runtime, &home).await?;
        let skill_dir = path.join(".agents/skills/original-demo");
        std::fs::create_dir_all(&skill_dir)?;
        let skill = skill_dir.join("SKILL.md");
        std::fs::write(
            &skill,
            "---\nname: original-demo\ndescription: original skill\n---\nOriginal instructions.\n",
        )?;
        runtime.refresh_context_resources().await?;
        if rollback {
            runtime.runtime_db().connection()?.execute_batch(
            "CREATE TRIGGER reject_original_skill_activation BEFORE INSERT ON audit_events
             WHEN NEW.kind='skill_activated' BEGIN SELECT RAISE(ABORT,'original Skill Audit rollback'); END;")?;
        }
        let result = runtime
            .record_skill_read_activation(skill.to_str().unwrap(), SkillLoadReason::ReadSkillMd)
            .await;
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(root.state.active_skills.len(), 1);
        let active = &root.state.active_skills[0];
        assert_eq!(active.name, "original-demo");
        assert_eq!(active.activation_state, SkillActivationState::TurnActive);
        assert_eq!(
            runtime
                .runtime_db()
                .agent_states()
                .latest(runtime.instance_agent_id())?
                .as_ref(),
            Some(root.state.as_ref())
        );
        if rollback {
            assert!(format!(
                "{:#}",
                result.err().context("Skill Audit failure was swallowed")?
            )
            .contains("original Skill Audit rollback"));
            assert!(selected(&runtime, "skill_activated")?.is_empty());
        } else {
            result?;
            let events = selected(&runtime, "skill_activated")?;
            assert_eq!(events.len(), 1);
            let event = &events[0];
            assert_eq!(
                event.data,
                serde_json::json!({
                    "agent_id":runtime.instance_agent_id(),"skill_id":active.skill_id,"skill_name":"original-demo",
                    "path":skill,"entrypoint_path":active.path,"scope":active.scope,
                    "activation_source":SkillActivationSource::ImplicitFromCatalog,
                    "activation_state":SkillActivationState::TurnActive,"load_reason":SkillLoadReason::ReadSkillMd,
                    "turn_index":root.state.turn_index,"run_id":root.state.current_run_id,"repeated":false,
                })
            );
            let state = selected(&runtime, "agent_state_mutated")?;
            assert!(state
                .iter()
                .any(|state| state.event_seq + 1 == event.event_seq));
            drop(root);
            runtime
                .record_skill_read_activation(skill.to_str().unwrap(), SkillLoadReason::ReadSkillMd)
                .await?;
            let repeated = selected(&runtime, "skill_activated")?;
            assert_eq!(repeated.len(), 2);
            assert!(repeated.iter().any(|e| e.data["repeated"] == true));
        }
    }
    Ok(())
}

#[tokio::test]
async fn original_runtime_observations_cannot_borrow_lifecycle_deleting_or_no_state_authority(
) -> Result<()> {
    for mode in ["deleting", "no_state", "successor"] {
        let (_home, runtime) = fixture().await?;
        let audit = runtime
            .prepare_original_work_completion_warning("work", "kind", "original", None, None)?;
        let baseline = counts(&runtime)?;
        match mode {
            "no_state" => {
                runtime.runtime_db().connection()?.execute(
                    "DELETE FROM agent_states WHERE agent_id=?1",
                    [runtime.instance_agent_id()],
                )?;
            }
            _ => {
                let mut identity = runtime
                    .runtime_db()
                    .agent_identities()
                    .latest(runtime.instance_agent_id())?
                    .unwrap();
                if mode == "deleting" {
                    identity.status = crate::types::AgentRegistryStatus::Deleting;
                } else {
                    identity.incarnation += 1;
                }
                runtime.runtime_db().agent_identities().upsert(&identity)?;
            }
        }
        assert!(
            runtime
                .finish_owned_runtime_observation_audit(audit)
                .await
                .is_err(),
            "{mode}"
        );
        assert_eq!(counts(&runtime)?, baseline, "{mode}");
    }
    Ok(())
}
