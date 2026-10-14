use super::*;
use crate::runtime::{
    input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate},
    resident_input_evidence::InputEvidenceScope,
    resident_task::{QualifiedTaskFact, TaskCaptureRequest, TaskCoverage, TaskFamily, TaskKey},
};
use crate::runtime_db::{
    input_mutation_receipts::{InputMutationResult, OriginalInputMutationAttempt},
    task_snapshot as tasks,
    task_status_metadata::{
        FrozenControlWorktreeCompletion, TaskStatusMetadataInstallMode, TaskStatusMetadataSelection,
    },
    transitions::{InputMutationExecution, PreparedInputTransition, TransitionFaultPoint},
};

struct Original {
    f: Fixture,
    scope: InputEvidenceScope,
    target: ControlCancellationTarget,
    record: Arc<QualifiedTaskFact>,
    coverage: TaskCoverage,
    frozen: Arc<FrozenControlWorktreeCompletion>,
    ledger: InputEvidenceBudgetLedger,
    records: Vec<Arc<QualifiedTaskFact>>,
}
fn request() -> tasks::TaskDomainLoadRequest {
    tasks::TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 8 * 1024 * 1024,
    }
}
fn original() -> Result<Original> {
    original_with_artifact(None)
}
fn original_with_artifact(artifact: Option<serde_json::Value>) -> Result<Original> {
    let f = Fixture::new();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let scope = InputEvidenceScope {
        agent_id: f.key.agent_id.clone(),
        identity_incarnation: f.key.identity_incarnation,
    };
    let mut task = TaskRecord {
        id: "actual-control-worktree".into(),
        agent_id: f.key.agent_id.clone(),
        kind: TaskKind::ChildAgentTask,
        status: TaskStatus::Running,
        created_at: f.time,
        updated_at: f.time,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("actual original child".into()),
        recovery: None,
        detail: Some(
            json!({"child_agent_id":"original-child","workspace_mode":"worktree","parent_turn_id":"original-parent","rejoin_obligation_id":"actual-control-worktree","rejoin_generation":1,"worktree":{"worktree_path":"/original/exact/path","worktree_branch":"original-branch"}}),
        ),
    };
    if let Some(artifact) = artifact {
        task.detail.as_mut().unwrap()["worktree"] = artifact;
    }
    f.db.transaction(|tx| {
        tasks::prepare_task_domain_snapshot_with_budget_tx(tx, &scope, &request(), &ledger)?
            .require_current()?;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &task.id)]);
        let proof = tasks::prepare_task_write_tx(tx, &scope, &keys)?;
        upsert_task_tx(tx, &task)?;
        tasks::register_task_writes_with_budget_tx(
            tx,
            &proof,
            &keys,
            &f.db_state(tx)?,
            &request(),
            &ledger,
        )?;
        Ok(())
    })?;
    let mut input = f.input(ControlAction::Stop);
    let mut target = input.cancellation_targets[0].clone();
    target.obligation_id = "original-worktree-obligation".into();
    target.target_kind = ControlCancellationKind::Worktree;
    target.target_generation["artifact"] = task.detail.as_ref().unwrap()["worktree"].clone();
    target.target = json!({"reason":"agent_lifecycle_stop"});
    input.cancellation_targets.push(target.clone());
    let command = prepare_control(input)?;
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 8 * 1024 * 1024,
    };
    let ControlCommit::Applied { delta, .. } =
        f.db.commit_control_with_domain_requests(&command, &work, &request(), &ledger)?
    else {
        anyhow::bail!("initial Control replayed")
    };
    let task_delta = delta
        .transition
        .effects
        .task_delta
        .as_ref()
        .context("actual Task ACK missing")?;
    let record = task_delta
        .records
        .iter()
        .find(|r| r.locator.key.family == TaskFamily::Task && r.locator.key.id == task.id)
        .context("actual terminal Task missing")?
        .clone();
    let records = task_delta.records.clone();
    let mut coverage = task_delta.coverage.clone();
    let mut metadata_guards = Vec::new();
    coverage.status = f.db.transaction(|tx| {
        tasks::observe_task_status_headers_tx(
            tx,
            &scope,
            std::slice::from_ref(&record),
            |bytes, records| {
                metadata_guards.push(crate::runtime_db::command_task_terminal::charge_material(
                    &scope,
                    bytes * 8,
                    records * 8,
                    &request(),
                    &ledger,
                )?);
                Ok(())
            },
        )
    })?;
    // Freeze the explicit already-observed Status head. This setup does not
    // create a Task alias or alter the actual initial Control/Task receipts.
    f.db.transaction(|tx|{tx.execute("UPDATE task_domain_current SET coverage_json=?3 WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation,serde_json::to_string(&coverage)?])?;Ok(())})?;
    let material = Arc::new(ledger.reserve_owned_completion(
        &scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate::default(),
    )?);
    let frozen = Arc::new(FrozenControlWorktreeCompletion::freeze(
        &f.key,
        &target,
        &json!({"detail":{"worktree":{"cleanup_status":"cleaned","auto_cleaned_up":true}}}),
        f.time + chrono::Duration::seconds(20),
        &request(),
        material,
    )?);
    drop((metadata_guards, delta));
    Ok(Original {
        f,
        scope,
        target,
        record,
        coverage,
        frozen,
        ledger,
        records,
    })
}
fn phase(
    o: &Original,
    nonce: &str,
    selected: bool,
    fault: Option<TransitionFaultPoint>,
) -> Result<PreparedInputTransition> {
    let selected = selected
        .then(|| {
            TaskStatusMetadataSelection::single(
                &o.scope,
                o.coverage.revision,
                &o.record,
                &request(),
                &o.ledger,
            )
        })
        .transpose()?;
    PreparedInputTransition::control_worktree_metadata(
        o.frozen.clone(),
        nonce.into(),
        selected,
        fault,
    )
}
fn execute(
    o: &Original,
    prepared: &PreparedInputTransition,
    request: &tasks::TaskDomainLoadRequest,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 8 * 1024 * 1024,
    };
    o.f.db
        .transitions_in_scope(&o.scope)
        .commit_prepared_transition_with_work_request_and_memory_budget(
            prepared, &work, &o.ledger, request,
        )
}
#[test]
fn original_control_completion_joins_status_revision_and_whole_receipt_exactly_once() -> Result<()>
{
    let o = original()?;
    let canonical = o.f.db.tasks().latest(&o.record.locator.key.id)?.unwrap();
    let before = o.coverage.clone();
    let prepared = phase(&o, "status-once", true, None)?;
    let InputMutationExecution::Committed { value, receipt } = execute(&o, &prepared, &request())?
    else {
        anyhow::bail!("first phase replayed")
    };
    let delta = value
        .effects
        .task_delta
        .as_ref()
        .context("metadata Task delta missing")?;
    assert_eq!(delta.base_revision, before.revision);
    assert_eq!(delta.revision, before.revision + 1);
    assert!(delta.records.is_empty() && delta.stop_records.is_empty() && delta.removals.is_empty());
    assert_eq!(o.f.db.tasks().latest(&canonical.id)?, Some(canonical));
    assert_eq!(
        o.f.db
            .control_obligation(&o.f.key, &o.target)?
            .unwrap()
            .state,
        ControlObligationState::Completed
    );
    assert!(
        matches!(execute(&o,&prepared,&request())?,InputMutationExecution::Replayed{receipt:original,..}if original==receipt)
    );
    let mut expected = before;
    expected.status = delta.coverage.status.clone();
    expected.revision = delta.revision;
    assert_eq!(expected, delta.coverage);
    let attempt = OriginalInputMutationAttempt::new(prepared.descriptor().clone());
    attempt.finish_original_attempt();
    let observed =
        o.f.db
            .with_task_status_metadata_recovery(
                &attempt,
                &request(),
                &o.ledger,
                |tx, view, remaining| {
                    tasks::read_task_status_metadata_recovery_tx(
                        tx,
                        view,
                        &o.scope,
                        &o.coverage,
                        remaining,
                        &o.ledger,
                        false,
                    )
                },
            )?
            .unwrap();
    assert_eq!(observed.1 .0.coverage, delta.coverage);
    assert!(observed.0.matches_original(&attempt));
    Ok(())
}
#[test]
fn original_control_metadata_receipt_failure_rolls_back_source_and_task_then_retries_same_nonce(
) -> Result<()> {
    let o = original()?;
    let broken = phase(
        &o,
        "status-rollback",
        true,
        Some(TransitionFaultPoint::BeforeCommit),
    )?;
    assert!(execute(&o, &broken, &request()).is_err());
    assert_eq!(
        o.f.db
            .control_obligation(&o.f.key, &o.target)?
            .unwrap()
            .state,
        ControlObligationState::Pending
    );
    let count: usize = o.f.db.transaction(|tx| {
        Ok(tx.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='status-rollback'",
            [],
            |r| r.get(0),
        )?)
    })?;
    assert_eq!(count, 0);
    let retry = phase(&o, "status-rollback", true, None)?;
    assert_eq!(broken.descriptor(), retry.descriptor());
    assert!(matches!(
        execute(&o, &retry, &request())?,
        InputMutationExecution::Committed { .. }
    ));
    Ok(())
}
#[test]
fn original_control_source_only_no_state_and_retired_keep_task_none_without_successor_read(
) -> Result<()> {
    for mode in ["current-source-only", "no-state", "retired"] {
        let o = original()?;
        if mode == "no-state" {
            o.f.db.transaction(|tx| {
                tx.execute(
                    "DELETE FROM agent_states WHERE agent_id=?1",
                    [&o.scope.agent_id],
                )?;
                Ok(())
            })?;
        }
        if mode == "retired" {
            let mut identity =
                o.f.db
                    .agent_identities()
                    .latest(&o.scope.agent_id)?
                    .unwrap();
            identity.incarnation += 1;
            o.f.db.agent_identities().upsert(&identity)?;
            o.f.db.transaction(|tx|{tx.execute("UPDATE agent_states SET payload_json='poison-successor-state' WHERE agent_id=?1",[&o.scope.agent_id])?;Ok(())})?;
        }
        let prepared = phase(&o, &format!("status-{mode}"), false, None)?;
        let InputMutationExecution::Committed { value, receipt } =
            execute(&o, &prepared, &request())?
        else {
            anyhow::bail!("first source-only replayed")
        };
        assert!(
            value.effects.task_delta.is_none()
                && value.effects.agent_state.is_none()
                && value.effects.messages.is_empty()
        );
        let InputMutationResult::TaskStatusMetadata { ack } = &receipt.outcome.result else {
            anyhow::bail!("wrong phase result")
        };
        assert_eq!(
            ack.install_mode,
            match mode {
                "no-state" => TaskStatusMetadataInstallMode::NoState,
                "retired" => TaskStatusMetadataInstallMode::Retired,
                _ => TaskStatusMetadataInstallMode::Current,
            }
        );
        assert!(ack.task_revision.is_none());
        let attempt = OriginalInputMutationAttempt::new(prepared.descriptor().clone());
        attempt.finish_original_attempt();
        assert!(o
            .f
            .db
            .with_task_status_metadata_recovery(&attempt, &request(), &o.ledger, |_, view, _| Ok(
                view.mode()
            ))?
            .is_some());
    }
    Ok(())
}
#[test]
fn malformed_large_control_pool_is_admitted_before_decode_and_releases_failed_guards() -> Result<()>
{
    let o = original()?;
    let prepared = phase(&o, "status-budget", false, None)?;
    o.f.db.transaction(|tx|{tx.execute("UPDATE control_cancellation_obligations SET target_json=?4 WHERE agent_id=?1 AND command_id=?2 AND obligation_id=?3",params![o.scope.agent_id,o.f.key.command_id,o.target.obligation_id,"x".repeat(65*1024)])?;Ok(())})?;
    let before = o.ledger.report()?.observed_bytes;
    let mut tiny = request();
    tiny.byte_limit = 64 * 1024;
    let error = execute(&o, &prepared, &tiny).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<tasks::TaskSnapshotError>(),
            Some(tasks::TaskSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(o.ledger.report()?.observed_bytes, before);
    let count: usize = o.f.db.transaction(|tx| {
        Ok(tx.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='status-budget'",
            [],
            |r| r.get(0),
        )?)
    })?;
    assert_eq!(count, 0);
    Ok(())
}

#[tokio::test]
async fn status_lost_publication_recovers_only_original_metadata_and_preserves_task_none(
) -> Result<()> {
    use crate::runtime::{
        mutation_owner::{MutationOwner, MutationReservation},
        resident_task::ResidentTaskDomain,
    };
    for prepared_task in [false, true] {
        let o = original()?;
        let owner = MutationOwner::standalone(&o.scope.agent_id, o.scope.identity_incarnation);
        let state = Arc::new(o.f.db.transaction(|tx| o.f.db_state(tx))?);
        let tasks = prepared_task
            .then(|| {
                ResidentTaskDomain::bootstrap(
                    o.scope.clone(),
                    o.coverage.clone(),
                    o.records.clone(),
                    65536,
                    8 * 1024 * 1024,
                )?
                .with_budget(o.ledger.clone())
                .map(Arc::new)
            })
            .transpose()?;
        let mut initial = owner.reserve().await?;
        initial.begin("status-original-ready".into())?;
        initial.committed(state.clone(), tasks.clone())?;
        let before = initial.publish(state.clone())?;
        drop(initial);
        let ticket = match o
            .ledger
            .reserve_pending(&o.scope, "accepted-before-status", 10)?
        {
            crate::runtime::input_evidence_budget::PendingAdmission::Reserved(t) => t,
            _ => anyhow::bail!("new accepted fixture already exists"),
        };
        o.ledger.mark_attempt_started(&ticket)?;
        o.ledger.mark_accepted(&ticket)?;
        let pending = o.ledger.report()?.pending;
        let prepared = phase(
            &o,
            if prepared_task {
                "status-lost-task"
            } else {
                "status-lost-none"
            },
            prepared_task,
            Some(TransitionFaultPoint::BeforeCacheUpdate),
        )?;
        let mut reservations = vec![owner.reserve().await?];
        let error = crate::runtime::execute_original_control_metadata(
            &o.f.db,
            &mut reservations,
            &prepared,
            &request(),
            &o.ledger,
            &crate::context::ContextConfig::default(),
            &crate::config::RuntimeModelCatalog::default(),
            vec![],
        )
        .err()
        .context("phase should lose publication")?;
        assert!(error.to_string().contains("publication recovery"));
        assert!(owner.capture().is_err());
        drop(reservations);
        let mut reservations = vec![owner.reserve().await?];
        let known = reservations[0]
            .recovery()?
            .unwrap()
            .known
            .context("original known ACK missing")?;
        let after = MutationReservation::reconcile_all(&mut reservations, vec![Some(known)])?
            .remove(0)
            .unwrap();
        assert!(Arc::ptr_eq(&before.state, &after.state));
        assert!(after.episodes.is_none() && after.input_evidence.is_none() && after.work.is_none());
        assert_eq!(after.tasks.is_some(), prepared_task);
        if let Some(tasks) = &after.tasks {
            assert_eq!(tasks.coverage().revision, o.coverage.revision + 1);
        } else {
            assert!(before.tasks.is_none());
        }
        assert_eq!(o.ledger.report()?.pending, pending);
        drop(reservations);
    }
    Ok(())
}

#[tokio::test]
async fn actual_git_completion_survives_audit_sql_failure_and_retries_original_body_once(
) -> Result<()> {
    use crate::system::{
        EffectiveExecution, ExecutionProfile, ExecutionScopeKind, WorkspaceAccessMode,
        WorkspaceProjectionKind, WorkspaceView,
    };
    fn git(root: &std::path::Path, args: &[&str]) -> Result<()> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()?;
        anyhow::ensure!(
            out.status.success(),
            "git fixture failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    }
    let repo = tempfile::tempdir()?;
    let root = repo.path().join("repo");
    let path = repo.path().join("worktree");
    std::fs::create_dir(&root)?;
    git(&root, &["init", "--quiet"])?;
    git(&root, &["config", "user.email", "fixture@localhost"])?;
    git(&root, &["config", "user.name", "Fixture"])?;
    std::fs::write(root.join("tracked"), "original\n")?;
    git(&root, &["add", "tracked"])?;
    git(&root, &["commit", "--quiet", "-m", "original"])?;
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "original-audit-stop",
            path.to_str().unwrap(),
        ],
    )?;
    let artifact = json!({"worktree_path":path,"worktree_branch":"original-audit-stop"});
    let o = original_with_artifact(Some(artifact.clone()))?;
    let completion = Arc::new(o.ledger.reserve_owned_completion(
        &o.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate::default(),
    )?);
    let kernel = crate::runtime::FrozenWorktreeCleanup::new(
        EffectiveExecution {
            profile: ExecutionProfile::default(),
            workspace: WorkspaceView::new(
                None,
                root.clone(),
                root.clone(),
                root.clone(),
                None,
                Some(WorkspaceAccessMode::ExclusiveWrite),
                WorkspaceProjectionKind::CanonicalRoot,
                None,
            )?,
            scope: ExecutionScopeKind::AgentTurn,
            attached_workspaces: Vec::new(),
        },
        root.clone(),
        o.scope.clone(),
        o.ledger.clone(),
        &request(),
    )?;
    let mut detail = artifact;
    let (status, audits) = crate::runtime::cleanup_completion_worktree_for_test(
        kernel,
        completion.clone(),
        &o.record.locator.key.id,
        &mut detail,
    )
    .await?;
    assert_eq!(
        status,
        crate::runtime::TaskOwnedWorktreeCleanupStatus::Cleaned
    );
    assert!(!path.exists());
    assert!(!audits.is_empty());
    let frozen = Arc::new(FrozenControlWorktreeCompletion::freeze_with_audits(
        &o.f.key,
        &o.target,
        &json!({"detail":{"worktree":&detail}}),
        &audits,
        true,
        o.f.time + chrono::Duration::seconds(30),
        &request(),
        completion.clone(),
    )?);
    let selected = TaskStatusMetadataSelection::single(
        &o.scope,
        o.coverage.revision,
        &o.record,
        &request(),
        &o.ledger,
    )?;
    let prepared = PreparedInputTransition::control_worktree_metadata(
        frozen.clone(),
        "physical-audit-retry".into(),
        Some(selected),
        None,
    )?;
    o.f.db.connection()?.execute_batch("CREATE TRIGGER reject_original_cleanup_audit BEFORE INSERT ON audit_events WHEN NEW.kind='worktree_auto_cleaned_up' BEGIN SELECT RAISE(ABORT,'original audit failure'); END;")?;
    let error = execute(&o, &prepared, &request()).unwrap_err();
    assert!(format!("{error:#}").contains("original audit failure"));
    assert_eq!(
        o.f.db
            .control_obligation(&o.f.key, &o.target)?
            .unwrap()
            .state,
        ControlObligationState::Pending
    );
    let receipt_count: usize = o.f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce='physical-audit-retry'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(receipt_count, 0);
    // A replaced path proves retry cannot invoke the cleanup kernel again. The
    // frozen original cleaned evidence must remain exactly that first outcome.
    std::fs::create_dir(&path)?;
    std::fs::write(path.join("successor-sentinel"), "keep")?;
    o.f.db
        .connection()?
        .execute_batch("DROP TRIGGER reject_original_cleanup_audit")?;
    let InputMutationExecution::Committed { receipt, .. } = execute(&o, &prepared, &request())?
    else {
        anyhow::bail!("retry replayed missing receipt")
    };
    assert!(path.join("successor-sentinel").exists());
    assert!(matches!(
        execute(&o, &prepared, &request())?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(
        o.f.db
            .control_obligation(&o.f.key, &o.target)?
            .unwrap()
            .state,
        ControlObligationState::Completed
    );
    for audit in &audits {
        let count: usize = o.f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE audit_event_id=?1",
            [&audit.id],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1);
    }
    assert!(
        receipt.outcome.scopes[0].audit_events.is_empty(),
        "original durable-only audit must not acquire new wire effects"
    );
    assert_eq!(detail["cleanup_status"], "cleaned");
    drop((prepared, frozen, audits, completion));
    Ok(())
}
#[test]
fn original_control_pool_nodes_are_admitted_before_decoding_within_one_allowance() -> Result<()> {
    let o = original()?;
    // This is valid compact JSON with many Value nodes, not an oversized string.
    let many = serde_json::to_string(&vec![0; 2048])?;
    o.f.db.connection()?.execute("UPDATE control_cancellation_obligations SET completion_evidence_json=?4 WHERE agent_id=?1 AND identity_incarnation=?2 AND command_id=?3",params![o.scope.agent_id,o.scope.identity_incarnation,o.f.key.command_id,many])?;
    let mut small = request();
    small.record_limit = 128;
    let before = o.ledger.report()?.observed_bytes;
    let error =
        o.f.db
            .transaction(|tx| {
                crate::runtime_db::task_status_metadata::control_completion_material_tx(
                    tx, &o.scope, &o.f.key, &small, &o.ledger,
                )
            })
            .unwrap_err();
    assert!(error
        .downcast_ref::<tasks::TaskSnapshotError>()
        .is_some_and(|e| matches!(e, tasks::TaskSnapshotError::ResourceExhausted(_))));
    assert_eq!(o.ledger.report()?.observed_bytes, before);
    let material = o.f.db.transaction(|tx| {
        crate::runtime_db::task_status_metadata::control_completion_material_tx(
            tx,
            &o.scope,
            &o.f.key,
            &request(),
            &o.ledger,
        )
    })?;
    assert!(material.2 > 128);
    assert!(o.ledger.report()?.observed_bytes > before);
    drop(material);
    assert_eq!(o.ledger.report()?.observed_bytes, before);
    Ok(())
}

#[test]
fn failed_physical_cleanup_persists_its_original_audit_without_completing_obligation() -> Result<()>
{
    let o = original()?;
    let event = AuditEvent::legacy(
        "worktree_auto_cleanup_failed",
        json!({"task_id":o.record.locator.key.id,"error":"original Git refused remove"}),
    );
    let completion = Arc::new(o.ledger.reserve_owned_completion(
        &o.scope,
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate::default(),
    )?);
    let frozen = Arc::new(FrozenControlWorktreeCompletion::freeze_with_audits(
        &o.f.key,
        &o.target,
        &json!({"detail":{"worktree":{"cleanup_status":"failed"}}}),
        std::slice::from_ref(&event),
        false,
        o.f.time + chrono::Duration::seconds(30),
        &request(),
        completion,
    )?);
    let selected = TaskStatusMetadataSelection::single(
        &o.scope,
        o.coverage.revision,
        &o.record,
        &request(),
        &o.ledger,
    )?;
    let prepared = PreparedInputTransition::control_worktree_metadata(
        frozen,
        "failed-physical-audit".into(),
        Some(selected),
        None,
    )?;
    let InputMutationExecution::Committed { value, receipt } = execute(&o, &prepared, &request())?
    else {
        anyhow::bail!("failed audit unexpectedly replayed")
    };
    assert!(value.applied);
    assert!(value.effects.task_delta.is_none());
    assert!(
        matches!(&receipt.outcome.result,InputMutationResult::TaskStatusMetadata{ack} if !ack.source_changed && ack.task_revision.is_none())
    );
    assert_eq!(
        o.f.db
            .control_obligation(&o.f.key, &o.target)?
            .unwrap()
            .state,
        ControlObligationState::Pending
    );
    assert!(matches!(
        execute(&o, &prepared, &request())?,
        InputMutationExecution::Replayed { .. }
    ));
    let count: usize = o.f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE audit_event_id=?1",
        [&event.id],
        |r| r.get(0),
    )?;
    assert_eq!(count, 1);
    Ok(())
}
