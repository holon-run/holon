//! Reader/qualification split for named Task preparation: the preparation
//! reader pool builds the candidate; the original writer re-verifies the
//! recorded dependency view and commits. Reader observations never become
//! write authority on their own.

use super::candidate::{task_preparation_initialization_required_tx, TaskPreparationCandidate};
use super::tests::{at, fresh, request, scope, task};
use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{evidence::upsert_agent_identity_tx, RuntimeDb},
    types::{AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility},
};

fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}

fn owner_request(keys: &[TaskKey]) -> TaskDomainLoadRequest {
    let mut value = request();
    value.capture.required_keys.extend(keys.iter().cloned());
    value
}

fn seed_owner_task(db: &RuntimeDb, id: &str) -> Result<()> {
    let value = task(id, crate::types::TaskStatus::Running);
    let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, id)]);
    db.transaction(|tx| {
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &value)?;
        register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &request(),
        )
    })?;
    Ok(())
}

fn candidate_for(
    db: &RuntimeDb,
    budget: &InputEvidenceBudgetLedger,
    keys: &[TaskKey],
) -> Result<TaskPreparationCandidate> {
    db.read_for_preparation(|tx| {
        prepare_task_candidate_tx(
            tx,
            &scope(),
            &AgentState::new("task-agent"),
            &owner_request(keys),
            budget,
        )
    })?
    .context("prepared domain produced no candidate")
}

fn seed_owner_child_task(db: &RuntimeDb) -> Result<TaskKey> {
    let mut value = task("candidate-child", crate::types::TaskStatus::Running);
    value.kind = crate::types::TaskKind::ChildAgentTask;
    value.detail = Some(serde_json::json!({"child_agent_id": "shared-child"}));
    let key = TaskKey::new(TaskFamily::Task, &value.id);
    let keys = BTreeSet::from([key.clone()]);
    db.transaction(|tx| {
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        repositories::upsert_task_tx(tx, &value)?;
        register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &request(),
        )
    })?;
    Ok(key)
}

fn cross_parent_delegation(id: &str, child: &str) -> crate::types::WorkItemDelegationRecord {
    crate::types::WorkItemDelegationRecord {
        delegation_id: id.into(),
        parent_agent_id: "another-parent".into(),
        parent_work_item_id: "original-parent-work".into(),
        child_agent_id: child.into(),
        child_work_item_id: "original-child-work".into(),
        state: crate::types::WorkItemDelegationState::Open,
        result_summary: None,
        created_at: at(),
        updated_at: at(),
    }
}

fn task_lane_metadata(db: &RuntimeDb) -> Result<(u64, String)> {
    Ok(db.connection()?.query_row(
        "SELECT revision,coverage_json FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope().agent_id, scope().identity_incarnation],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

#[test]
fn cross_parent_delegation_head_changes_invalidate_candidate() -> Result<()> {
    for change in ["appears", "replaced", "same_head_metadata", "disappears"] {
        let (_dir, db) = fresh()?;
        let key = seed_owner_child_task(&db)?;
        let original = cross_parent_delegation("original-delegation", "shared-child");
        if change != "appears" {
            db.work_item_delegations().upsert(&original)?;
        }
        let budget = ledger()?;
        let initial = candidate_for(&db, &budget, std::slice::from_ref(&key))?;
        let resident = db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), initial))?;
        let expected = (change != "appears").then(|| TaskDelegationMetadata {
            child_agent_id: original.child_agent_id.clone(),
            parent_work_item_id: original.parent_work_item_id.clone(),
            child_work_item_id: original.child_work_item_id.clone(),
            delegation_id: original.delegation_id.clone(),
        });
        assert_eq!(
            resident.coverage().status.heads[&key.id].delegation,
            TaskDelegationObservation::Observed { latest: expected },
            "{change}"
        );
        let candidate = candidate_for(&db, &budget, std::slice::from_ref(&key))?;
        let before = task_lane_metadata(&db)?;
        let mut next = original.clone();
        match change {
            "appears" => {}
            "replaced" => {
                next.delegation_id = "newer-delegation".into();
                next.updated_at += chrono::Duration::seconds(1);
            }
            // Equal timestamps are accepted by the actual writer. Comparing
            // only a delegation ID or its timestamp would miss these fields.
            "same_head_metadata" => {
                next.parent_work_item_id = "changed-parent-work".into();
                next.child_work_item_id = "changed-child-work".into();
            }
            "disappears" => next.child_agent_id = "other-child".into(),
            _ => unreachable!(),
        }
        db.work_item_delegations().upsert(&next)?;
        assert_eq!(task_lane_metadata(&db)?, before, "{change}");
        let error = db
            .transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<TaskPreparationCandidateStale>()
                .map(|stale| stale.0),
            Some("TaskStatus delegation head"),
            "{change}: {error:#}"
        );
        assert_eq!(task_lane_metadata(&db)?, before, "{change}");
        let retry = candidate_for(&db, &budget, std::slice::from_ref(&key))?;
        let resident = db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), retry))?;
        let latest = (change != "disappears").then(|| TaskDelegationMetadata {
            child_agent_id: next.child_agent_id.clone(),
            parent_work_item_id: next.parent_work_item_id.clone(),
            child_work_item_id: next.child_work_item_id.clone(),
            delegation_id: next.delegation_id.clone(),
        });
        assert_eq!(
            resident.coverage().status.heads[&key.id].delegation,
            TaskDelegationObservation::Observed { latest },
            "{change}"
        );
    }
    Ok(())
}

#[test]
fn unrelated_child_delegation_does_not_invalidate_candidate() -> Result<()> {
    for has_head in [false, true] {
        let (_dir, db) = fresh()?;
        let key = seed_owner_child_task(&db)?;
        if has_head {
            db.work_item_delegations().upsert(&cross_parent_delegation(
                "original-delegation",
                "shared-child",
            ))?;
        }
        let budget = ledger()?;
        let candidate = candidate_for(&db, &budget, &[key])?;
        let before = task_lane_metadata(&db)?;
        db.work_item_delegations().upsert(&cross_parent_delegation(
            "unrelated-delegation",
            "other-child",
        ))?;
        assert_eq!(task_lane_metadata(&db)?, before);
        db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))?;
    }
    Ok(())
}

fn seed_missing_result_target(db: &RuntimeDb, id: &str) -> Result<String> {
    use crate::{
        runtime_db::{
            evidence::append_message_tx,
            task_result_settlement::{upsert_pending_tx, TaskResultSettlementRecord},
        },
        types::{
            AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
            TaskKind, TaskStatus,
        },
    };
    let mut value = task(id, TaskStatus::Completed);
    value.kind = TaskKind::AgentMessageWait;
    value.detail = Some(serde_json::json!({
        "rejoin_obligation_id": id, "rejoin_generation": 1, "parent_turn_id": "original-parent"
    }));
    let mut source = MessageEnvelope::new(
        "task-agent",
        MessageKind::TaskResult,
        MessageOrigin::Task { task_id: id.into() },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "reply".into(),
        },
    );
    source.task_id = Some(id.into());
    source.created_at = at();
    db.transaction(|tx| {
        let (source, _) = append_message_tx(tx, &source)?;
        // Frozen Result facts are legitimate even when their Task is absent.
        let result =
            TaskResultSettlementRecord::pending(&value, &source, at())?.context("pending")?;
        let keys = BTreeSet::from([TaskKey::new(
            TaskFamily::ResultSettlement,
            &result.result_identity,
        )]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        upsert_pending_tx(tx, &result)?;
        register_task_writes_tx(
            tx,
            &proof,
            &keys,
            &AgentState::new("task-agent"),
            &request(),
        )?;
        Ok(result.result_identity)
    })
}

#[test]
fn result_candidate_commits_without_a_canonical_revision_column() -> Result<()> {
    let (_dir, db) = fresh()?;
    let result_id = seed_missing_result_target(&db, "positive-result-candidate")?;
    let key = TaskKey::new(TaskFamily::ResultSettlement, &result_id);
    let budget = ledger()?;
    let candidate = candidate_for(&db, &budget, &[key.clone()])?;
    let resident = db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))?;
    assert_eq!(
        resident.coverage().observations.get(&key),
        Some(&TaskAvailability::Present)
    );
    db.transaction(|tx| {
        let saved = alias(tx, &scope(), &key)?.context("Result alias missing")?;
        assert_eq!(saved.availability, TaskAvailability::Present);
        Ok(())
    })
}

#[test]
fn result_candidate_lifecycle_drift_is_rechecked_even_at_the_same_timestamp() -> Result<()> {
    use crate::runtime_db::task_result_settlement::TaskResultSettlementState;

    for settle in [false, true] {
        for advance_time in [false, true] {
            let (_dir, db) = fresh()?;
            let result_id = seed_missing_result_target(&db, "result-candidate-drift")?;
            let key = TaskKey::new(TaskFamily::ResultSettlement, &result_id);
            let (message_id, original_timestamp): (String, String) = db.transaction(|tx| {
                Ok(tx.query_row(
                    "SELECT message_id,updated_at FROM task_result_settlements WHERE result_identity=?1",
                    [&result_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })?;
            let budget = ledger()?;
            let candidate = candidate_for(&db, &budget, &[key])?;
            let now = at() + chrono::Duration::seconds(if advance_time { 1 } else { 0 });
            // Both changes use the real Result repository. Deferral need not
            // change state; rejecting its drift must not rely on updated_at.
            let changed = if settle {
                // The missing source Task legitimately settles invalid/stale.
                assert!(db
                    .task_result_settlements()
                    .admit_message("task-agent", &message_id, "candidate-admission", now)?
                    .is_none());
                db.task_result_settlements()
                    .latest_for_message(&message_id)?
                    .context("settled Result missing")?
            } else {
                db.task_result_settlements()
                    .mark_deferred(
                        &message_id,
                        "candidate-owner-blocked",
                        now,
                        now + chrono::Duration::seconds(30),
                    )?
                    .context("deferred Result missing")?
            };
            assert_eq!(changed.updated_at, now);
            assert_eq!(
                changed.state,
                if settle {
                    TaskResultSettlementState::Settled
                } else {
                    TaskResultSettlementState::PersistedPending
                }
            );
            if !advance_time {
                assert_eq!(
                    repositories::timestamp(changed.updated_at),
                    original_timestamp
                );
            }
            let error = db
                .transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))
                .unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<TaskPreparationCandidateStale>()
                    .map(|stale| stale.0),
                Some("selected canonical row"),
                "Result metadata drift was not rejected by canonical revalidation: {error:#}"
            );
        }
    }
    Ok(())
}

#[test]
fn invalid_result_target_absence_is_rechecked_without_selecting_the_task() -> Result<()> {
    for target_appears in [false, true] {
        let (_dir, db) = fresh()?;
        let result_id = seed_missing_result_target(&db, "missing-result-target")?;
        let budget = ledger()?;
        let candidate = candidate_for(&db, &budget, &[])?;
        db.transaction(|tx| {
            let mut foreign = task(
                if target_appears {
                    "missing-result-target"
                } else {
                    "unrelated-target"
                },
                crate::types::TaskStatus::Running,
            );
            foreign.agent_id = "neighbor-agent".into();
            repositories::upsert_task_tx(tx, &foreign)?;
            Ok(())
        })?;
        let committed = db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate));
        if target_appears {
            let error = committed.unwrap_err();
            assert!(
                error
                    .downcast_ref::<TaskPreparationCandidateStale>()
                    .is_some(),
                "unexpected target drift error: {error:#}"
            );
            // Reprepare observes the foreign predicate without claiming its body.
            let candidate = candidate_for(&db, &budget, &[])?;
            let resident =
                db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))?;
            assert!(matches!(
                &resident.coverage().result_dependencies[&result_id].relation,
                TaskResultDependencyRelation::Invalid {
                    target: TaskDependencyTarget::CanonicalObserved { owner, .. }, ..
                } if owner == "neighbor-agent"
            ));
        } else {
            let resident = committed?;
            assert!(matches!(
                &resident.coverage().result_dependencies[&result_id].relation,
                TaskResultDependencyRelation::Invalid {
                    target: TaskDependencyTarget::GlobalAbsent { .. },
                    ..
                }
            ));
        }
    }
    Ok(())
}

#[test]
fn invalid_foreign_result_target_revision_is_rechecked() -> Result<()> {
    let (_dir, db) = fresh()?;
    seed_missing_result_target(&db, "foreign-result-target")?;
    db.transaction(|tx| {
        let mut foreign = task("foreign-result-target", crate::types::TaskStatus::Running);
        foreign.agent_id = "neighbor-agent".into();
        repositories::upsert_task_tx(tx, &foreign)?;
        Ok(())
    })?;
    let budget = ledger()?;
    let candidate = candidate_for(&db, &budget, &[])?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE tasks SET revision=revision+1 WHERE task_id='foreign-result-target'",
            [],
        )?;
        Ok(())
    })?;
    let error = db
        .transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<TaskPreparationCandidateStale>()
            .is_some(),
        "unexpected target drift error: {error:#}"
    );
    Ok(())
}

#[test]
fn invalid_foreign_result_target_same_revision_predicate_is_rechecked() -> Result<()> {
    for update_target in [false, true] {
        let (_dir, db) = fresh()?;
        let target_id = "same-revision-result-target";
        let result_id = seed_missing_result_target(&db, target_id)?;
        let mut target = task(target_id, crate::types::TaskStatus::Running);
        target.agent_id = "neighbor-agent".into();
        let mut unrelated = task("unrelated-foreign-task", crate::types::TaskStatus::Running);
        unrelated.agent_id = "neighbor-agent".into();
        db.transaction(|tx| {
            assert!(repositories::upsert_task_tx(tx, &target)?);
            assert!(repositories::upsert_task_tx(tx, &unrelated)?);
            Ok(())
        })?;
        let budget = ledger()?;
        let baseline = candidate_for(&db, &budget, &[])?;
        let baseline =
            db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), baseline))?;
        let old_relation = baseline.coverage().result_dependencies[&result_id]
            .relation
            .clone();
        let candidate = candidate_for(&db, &budget, &[])?;
        let mut updated = if update_target { target } else { unrelated };
        updated.status = crate::types::TaskStatus::Completed;
        updated.updated_at += chrono::Duration::nanoseconds(1);
        db.transaction(|tx| {
            let revision: i64 = tx.query_row(
                "SELECT revision FROM tasks WHERE task_id=?1",
                [&updated.id],
                |row| row.get(0),
            )?;
            // A legal Running -> terminal write may advance time but retain
            // the same millisecond revision.
            assert!(repositories::upsert_task_tx(tx, &updated)?);
            let new_revision: i64 = tx.query_row(
                "SELECT revision FROM tasks WHERE task_id=?1",
                [&updated.id],
                |row| row.get(0),
            )?;
            assert_eq!(revision, new_revision);
            Ok(())
        })?;
        let committed = db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate));
        if update_target {
            let error = committed.unwrap_err();
            assert!(
                error
                    .downcast_ref::<TaskPreparationCandidateStale>()
                    .is_some(),
                "unexpected same-revision predicate drift error: {error:#}"
            );
            let candidate = candidate_for(&db, &budget, &[])?;
            let resident =
                db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))?;
            assert_ne!(
                resident.coverage().result_dependencies[&result_id].relation,
                old_relation,
                "reprepare must not retain the old predicate digest"
            );
        } else {
            let resident = committed?;
            assert_eq!(
                resident.coverage().result_dependencies[&result_id].relation,
                old_relation,
                "unrelated foreign writes must not invalidate the target observation"
            );
        }
    }
    Ok(())
}

#[test]
fn preparation_reader_never_holds_the_original_writer_barrier() -> Result<()> {
    let (_dir, db) = fresh()?;
    seed_owner_task(&db, "candidate-owner")?;
    let budget = ledger()?;
    let key = TaskKey::new(TaskFamily::Task, "candidate-owner");
    let mut concurrent_write = None;
    let candidate = db
        .read_for_preparation(|tx| {
            // An original writer commits unrelated work while the preparation
            // reader holds its snapshot: preparation readers never take the
            // writer barrier.
            concurrent_write = Some(
                db.transaction(|writer| {
                    writer.execute(
                        "INSERT INTO task_domain_current(agent_id,identity_incarnation,revision,coverage_json) VALUES('neighbor-agent',1,1,'{}')",
                        [],
                    )?;
                    Ok(())
                })
                .is_ok(),
            );
            prepare_task_candidate_tx(
                tx,
                &scope(),
                &AgentState::new("task-agent"),
                &owner_request(&[key.clone()]),
                &budget,
            )
        })?
        .context("prepared domain produced no candidate")?;
    assert_eq!(
        concurrent_write,
        Some(true),
        "preparation reader blocked an original writer"
    );
    let resident = db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))?;
    assert_eq!(
        resident.coverage().observations.get(&key),
        Some(&TaskAvailability::Present)
    );
    Ok(())
}

#[test]
fn lane_revision_drift_invalidates_the_candidate() -> Result<()> {
    let (_dir, db) = fresh()?;
    seed_owner_task(&db, "candidate-owner")?;
    let budget = ledger()?;
    let candidate = candidate_for(
        &db,
        &budget,
        &[TaskKey::new(TaskFamily::Task, "candidate-owner")],
    )?;
    // A Task lane write invalidates the reader even when all selected rows
    // retain their original canonical revisions.
    db.transaction(|tx| {
        tx.execute(
            "UPDATE task_domain_current SET revision=revision+1 WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope().agent_id, scope().identity_incarnation],
        )?;
        Ok(())
    })?;
    let error = db
        .transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<TaskPreparationCandidateStale>()
            .is_some(),
        "unexpected lane drift error: {error:#}"
    );
    Ok(())
}

#[test]
fn same_revision_coverage_drift_invalidates_the_candidate() -> Result<()> {
    let (_dir, db) = fresh()?;
    seed_owner_task(&db, "candidate-owner")?;
    let budget = ledger()?;
    let candidate = candidate_for(
        &db,
        &budget,
        &[TaskKey::new(TaskFamily::Task, "candidate-owner")],
    )?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE task_domain_current SET coverage_json=coverage_json||' ' WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope().agent_id, scope().identity_incarnation],
        )?;
        Ok(())
    })?;
    let error = db
        .transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<TaskPreparationCandidateStale>()
            .map(|error| error.0),
        Some("base coverage"),
        "unexpected coverage drift error: {error:#}"
    );
    Ok(())
}

#[test]
fn selected_canonical_row_drift_invalidates_the_candidate() -> Result<()> {
    let (_dir, db) = fresh()?;
    seed_owner_task(&db, "candidate-owner")?;
    let budget = ledger()?;
    let candidate = candidate_for(
        &db,
        &budget,
        &[TaskKey::new(TaskFamily::Task, "candidate-owner")],
    )?;
    // The recorded canonical revision is re-observed by exact key; the writer
    // never decodes the Task body.
    db.transaction(|tx| {
        tx.execute(
            "UPDATE tasks SET revision=revision+1 WHERE task_id='candidate-owner'",
            [],
        )?;
        Ok(())
    })?;
    let error = db
        .transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<TaskPreparationCandidateStale>()
            .is_some(),
        "unexpected canonical row drift error: {error:#}"
    );
    Ok(())
}

#[test]
fn canonical_drift_between_reader_and_writer_fails_closed() -> Result<()> {
    let (_dir, db) = fresh()?;
    seed_owner_task(&db, "candidate-owner")?;
    let budget = ledger()?;
    let candidate = candidate_for(
        &db,
        &budget,
        &[TaskKey::new(TaskFamily::Task, "candidate-owner")],
    )?;
    // The reader recorded the canonical identity read; changing it between
    // reader and writer must invalidate the candidate.
    db.transaction(|tx| {
        let raw: String = tx.query_row(
            "SELECT payload_json FROM agent_identities WHERE agent_id='task-agent'",
            [],
            |row| row.get(0),
        )?;
        let mut successor: AgentIdentityRecord = serde_json::from_str(&raw)?;
        successor.updated_at += chrono::Duration::days(1);
        upsert_agent_identity_tx(tx, &successor)?;
        Ok(())
    })?;
    let error = db
        .transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))
        .unwrap_err();
    assert!(
        error.to_string().contains("dependency changed"),
        "unexpected canonical drift error: {error:#}"
    );
    Ok(())
}

#[test]
fn negative_observations_commit_explicit_absence_aliases() -> Result<()> {
    let (_dir, db) = fresh()?;
    let budget = ledger()?;
    let absent = TaskKey::new(TaskFamily::Task, "absent-negative");
    let candidate = candidate_for(&db, &budget, &[absent.clone()])?;
    let resident = db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))?;
    let availability = resident
        .coverage()
        .observations
        .get(&absent)
        .context("absent key missing from candidate coverage")?;
    assert_ne!(availability, &TaskAvailability::Present);
    db.transaction(|tx| {
        let saved = alias(tx, &scope(), &absent)?.context("explicit absence alias missing")?;
        assert_ne!(saved.availability, TaskAvailability::Present);
        assert!(saved.digest.is_none());
        Ok(())
    })
}

#[test]
fn unrelated_agent_writes_do_not_invalidate_a_candidate() -> Result<()> {
    let (_dir, db) = fresh()?;
    seed_owner_task(&db, "candidate-owner")?;
    let budget = ledger()?;
    let key = TaskKey::new(TaskFamily::Task, "candidate-owner");
    let candidate = candidate_for(&db, &budget, &[key.clone()])?;
    db.transaction(|tx| {
        upsert_agent_identity_tx(
            tx,
            &AgentIdentityRecord::new(
                "neighbor-agent",
                AgentKind::Named,
                AgentVisibility::Public,
                AgentOwnership::SelfOwned,
                AgentProfilePreset::PublicNamed,
                None,
                None,
            ),
        )?;
        tx.execute(
            "INSERT INTO task_domain_current(agent_id,identity_incarnation,revision,coverage_json) VALUES('neighbor-agent',3,7,'{\"revision\":7}')",
            [],
        )?;
        Ok(())
    })?;
    let resident = db.transaction_once(|tx| commit_task_candidate_tx(tx, &scope(), candidate))?;
    assert_eq!(
        resident.coverage().observations.get(&key),
        Some(&TaskAvailability::Present)
    );
    db.transaction(|tx| {
        let saved = alias(tx, &scope(), &key)?.context("positive alias missing")?;
        assert_eq!(saved.availability, TaskAvailability::Present);
        Ok(())
    })
}

#[test]
fn cold_domain_requires_bootstrap_instead_of_named_writer_fallback() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    let budget = ledger()?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_candidate_tx(
            tx,
            &scope(),
            &AgentState::new("task-agent"),
            &request(),
            &budget,
        )
    })?;
    assert!(
        candidate.is_none(),
        "cold domain must not produce a preparation candidate"
    );
    Ok(())
}

#[test]
fn cold_initialization_gate_requires_both_coverage_and_source_inventory() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| {
        assert!(!task_preparation_initialization_required_tx(tx, &scope())?);
        tx.execute(
            "DELETE FROM task_message_source_inventory WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope().agent_id, scope().identity_incarnation],
        )?;
        assert!(task_preparation_initialization_required_tx(tx, &scope())?);
        tx.execute(
            "INSERT INTO task_message_source_inventory(agent_id,identity_incarnation,origin,seed_id) VALUES(?1,?2,'fresh_empty',NULL)",
            params![scope().agent_id, scope().identity_incarnation],
        )?;
        assert!(!task_preparation_initialization_required_tx(tx, &scope())?);
        tx.execute(
            "DELETE FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            params![scope().agent_id, scope().identity_incarnation],
        )?;
        assert!(task_preparation_initialization_required_tx(tx, &scope())?);
        Ok(())
    })
}
