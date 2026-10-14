use super::*;
use crate::runtime_db::task_result_settlement::TaskResultSettlementDisposition;

fn result_source(id: &str, task_id: &str) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "task-agent",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "original task result".into(),
        },
    );
    message.id = id.into();
    message.task_id = Some(task_id.into());
    message.created_at = at();
    message.metadata = Some(serde_json::json!({
        "task_id": task_id, "task_kind":"agent_message_wait", "task_status":"completed",
        "task_detail":{"message_id":"original-reply","reply_content_source":"original_message"}
    }));
    message
}
fn result_task(id: &str) -> TaskRecord {
    let mut value = task(id, TaskStatus::Completed);
    value.kind = TaskKind::AgentMessageWait;
    value.detail = Some(serde_json::json!({
        "rejoin_obligation_id": id, "rejoin_generation":1, "parent_turn_id":"original-parent-turn"
    }));
    value
}
// The original pending writer accepts frozen Result facts; it does not require
// a Task row. This is also how pre-cutover malformed dependencies can exist.
fn qualify_result(
    tx: &Transaction<'_>,
    source: &MessageEnvelope,
    value: &TaskRecord,
) -> Result<TaskResultSettlementRecord> {
    let (source, _) = append_message_tx(tx, source)?;
    let result = TaskResultSettlementRecord::pending(value, &source, at())?.context("pending")?;
    let keys = BTreeSet::from([TaskKey::new(
        TaskFamily::ResultSettlement,
        &result.result_identity,
    )]);
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    upsert_pending_tx(tx, &result)?;
    let mut state = AgentState::new("task-agent");
    state.total_message_count = tx.query_row(
        "SELECT COUNT(*) FROM messages WHERE agent_id='task-agent'",
        [],
        |r| r.get(0),
    )?;
    upsert_agent_state_tx(tx, &state)?;
    register_task_writes_tx(tx, &proof, &keys, &state, &request())?;
    Ok(result)
}

#[test]
fn no_state_existing_maintenance_updates_only_original_members_and_invalidates() -> Result<()> {
    for deleting in [false, true] {
        let (_dir, db) = fresh()?;
        let mut original = task("qualified-old", TaskStatus::Running);
        let old = db
            .transaction(|tx| {
                write_task(tx, &original)?;
                prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
            })?
            .resident;
        let old_capture = old.capture(&request().capture)?;
        db.transaction(|tx| {
            let mut raw = task("raw-neighbor", TaskStatus::Running);
            repositories::upsert_task_tx(tx, &raw)?;
            tx.execute(
                "DELETE FROM agent_states WHERE agent_id=?1",
                [&scope().agent_id],
            )?;
            if deleting {
                let mut identity = identity();
                identity.status = AgentRegistryStatus::Deleting;
                upsert_agent_identity_tx(tx, &identity)?;
            }
            let keys = BTreeSet::from([
                TaskKey::new(TaskFamily::Task, &original.id),
                TaskKey::new(TaskFamily::Task, &raw.id),
            ]);
            assert!(prepare_task_write_tx(tx, &scope(), &keys).is_err());
            let mut wrong_inc = scope();
            wrong_inc.identity_incarnation += 1;
            assert!(prepare_existing_task_maintenance_tx(tx, &wrong_inc, &keys).is_err());
            let proof = prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?;
            original.status = TaskStatus::Interrupted;
            original.updated_at = at() + Duration::seconds(1);
            raw.status = TaskStatus::Interrupted;
            raw.updated_at = original.updated_at;
            repositories::upsert_task_tx(tx, &original)?;
            repositories::upsert_task_tx(tx, &raw)?;
            let ack = register_existing_task_maintenance_tx(tx, proof, &keys)?;
            assert_eq!(
                ack.qualified_actual_keys,
                BTreeSet::from([TaskKey::new(TaskFamily::Task, &original.id)])
            );
            assert!(alias(tx, &scope(), &TaskKey::new(TaskFamily::Task, &raw.id))?.is_none());
            let coverage = current_coverage(tx, &scope())?.unwrap();
            assert!(!coverage.active_complete && !coverage.unsettled_complete);
            assert_eq!(
                ack.invalidation.as_ref().unwrap().revision,
                coverage.revision
            );
            assert!(coverage.revision > old.coverage().revision);
            assert!(matches!(
                prepare_task_domain_snapshot_tx(tx, &scope(), &request())?,
                TaskBootstrapSnapshot::Retired { .. }
            ));
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM agent_states WHERE agent_id=?1",
                [&scope().agent_id],
                |r| r.get(0),
            )?;
            assert_eq!(count, 0);
            Ok(())
        })?;
        let captured = old_capture
            .lookup(&TaskKey::new(TaskFamily::Task, &original.id))?
            .unwrap();
        let CanonicalTaskFact::Task(value) = &captured.fact else {
            unreachable!()
        };
        assert_eq!(value.status, TaskStatus::Running);
    }
    Ok(())
}

#[test]
fn existing_maintenance_alias_failure_rolls_back_business_and_coverage() -> Result<()> {
    let (_dir, db) = fresh()?;
    let original = task("rollback-task", TaskStatus::Running);
    db.transaction(|tx| {
        write_task(tx, &original)?;
        Ok(())
    })?;
    let before = db.transaction(|tx| current_coverage(tx, &scope()))?;
    db.transaction(|tx| {
        tx.execute_batch("CREATE TRIGGER fail_maintenance_alias BEFORE INSERT ON task_domain_records BEGIN SELECT RAISE(ABORT,'maintenance alias fault'); END;")?;
        Ok(())
    })?;
    let error = db
        .transaction(|tx| {
            let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &original.id)]);
            let proof = prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?;
            let mut final_task = original.clone();
            final_task.status = TaskStatus::Interrupted;
            final_task.updated_at = at() + Duration::seconds(1);
            repositories::upsert_task_tx(tx, &final_task)?;
            register_existing_task_maintenance_tx(tx, proof, &keys)
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("maintenance alias fault"));
    db.transaction(|tx| {
        let stored = stored_on(
            tx,
            &scope().agent_id,
            &TaskKey::new(TaskFamily::Task, &original.id),
        )?
        .unwrap();
        assert_eq!(stored.fact, CanonicalTaskFact::Task(original.clone()));
        assert_eq!(current_coverage(tx, &scope())?, before);
        Ok(())
    })
}

#[test]
fn qualified_result_missing_foreign_and_invalid_task_relations_preserve_original_skip() -> Result<()>
{
    for relation in ["missing", "foreign", "invalid_fence"] {
        let (_dir, db) = fresh()?;
        let value = result_task("result-task");
        let result = db.transaction(|tx| {
            if relation != "missing" {
                let mut canonical = value.clone();
                if relation == "foreign" {
                    canonical.agent_id = "foreign-agent".into();
                }
                if relation == "invalid_fence" {
                    canonical.detail = None;
                }
                repositories::upsert_task_tx(tx, &canonical)?;
            }
            qualify_result(
                tx,
                &result_source("original-result-message", &value.id),
                &value,
            )
        })?;
        db.transaction(|tx| {
            let snapshot =
                prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
            assert!(snapshot.resident.coverage().unsettled_complete);
            let captured = snapshot.resident.capture(&request().capture)?;
            assert!(matches!(
                captured
                    .result_dependency(&result.result_identity)?
                    .relation,
                TaskResultDependencyRelation::Invalid { .. }
            ));
            assert!(!captured
                .records
                .iter()
                .any(|r| r.locator.key.family == TaskFamily::Task));
            assert!(!snapshot.input_roots.contains(&InputEvidenceKey::new(
                InputEvidenceFamily::Turn,
                &result.parent_turn_id
            )));
            let mut explicit = request();
            explicit
                .capture
                .required_keys
                .insert(TaskKey::new(TaskFamily::Task, &value.id));
            if relation == "missing" {
                let absent =
                    prepare_task_domain_snapshot_tx(tx, &scope(), &explicit)?.require_current()?;
                assert!(absent
                    .resident
                    .capture(&explicit.capture)?
                    .lookup(&TaskKey::new(TaskFamily::Task, &value.id))?
                    .is_none());
            } else {
                assert!(prepare_task_domain_snapshot_tx(tx, &scope(), &explicit).is_err());
            }
            Ok(())
        })?;
        // Original invalid Reply admission is a skip, leaving the deadline intact.
        db.task_result_settlements().admit_reply_message(
            "task-agent",
            None,
            "original-reply",
            "reply-activation",
            at() + Duration::seconds(1),
        )?;
        assert_eq!(
            db.task_result_settlements()
                .latest_for_message(&result.message_id)?
                .unwrap(),
            result
        );
        // Message admission actually settles invalid rows despite returning None.
        assert!(db
            .task_result_settlements()
            .admit_message(
                "task-agent",
                &result.message_id,
                "message-activation",
                at() + Duration::seconds(2)
            )?
            .is_none());
        let settled = db
            .task_result_settlements()
            .latest_for_message(&result.message_id)?
            .unwrap();
        assert_eq!(settled.state, TaskResultSettlementState::Settled);
        assert_eq!(
            settled.disposition,
            Some(TaskResultSettlementDisposition::InvalidOrStale)
        );
        assert!(settled.next_recheck_at.is_none());
    }
    Ok(())
}

#[test]
fn matching_same_agent_unqualified_task_is_not_adopted_by_result_relation() -> Result<()> {
    let (_dir, db) = fresh()?;
    let error = db
        .transaction(|tx| {
            let value = result_task("raw-matching-task");
            repositories::upsert_task_tx(tx, &value)?;
            let (source, _) =
                append_message_tx(tx, &result_source("raw-matching-message", &value.id))?;
            let result = TaskResultSettlementRecord::pending(&value, &source, at())?.unwrap();
            let keys = BTreeSet::from([TaskKey::new(
                TaskFamily::ResultSettlement,
                &result.result_identity,
            )]);
            let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
            upsert_pending_tx(tx, &result)?;
            let mut state = AgentState::new("task-agent");
            state.total_message_count = 1;
            upsert_agent_state_tx(tx, &state)?;
            register_task_writes_tx(tx, &proof, &keys, &state, &request())
        })
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::NeedsPreparation(key)) if key.id=="raw-matching-task")
    );
    db.transaction(|tx| {
        assert!(alias(
            tx,
            &scope(),
            &TaskKey::new(TaskFamily::Task, "raw-matching-task")
        )?
        .is_none());
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM task_result_settlements", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            tx.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
}

#[test]
fn task_only_writer_refreshes_result_predicate_and_rejects_old_revision() -> Result<()> {
    let (_dir, db) = fresh()?;
    let terminal = result_task("fence-task");
    let mut value = terminal.clone();
    value.status = TaskStatus::Running;
    let result = db.transaction(|tx| {
        write_task(tx, &value)?;
        qualify_result(
            tx,
            &result_source("fence-result-message", &value.id),
            &terminal,
        )
    })?;
    let old = db
        .transaction(|tx| {
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()
        })?
        .resident;
    let capture = old.capture(&request().capture)?;
    assert!(matches!(
        capture.result_dependency(&result.result_identity)?.relation,
        TaskResultDependencyRelation::Invalid {
            reason: TaskDependencyInvalidReason::PredicateMismatch,
            ..
        }
    ));
    let updated=db.transaction(|tx| {
        let task_key=TaskKey::new(TaskFamily::Task,&value.id);
        let keys=BTreeSet::from([task_key]);
        let proof=prepare_task_write_tx(tx,&scope(),&keys)?;
        value.status=TaskStatus::Completed;
        value.updated_at=at()+Duration::seconds(1);
        repositories::upsert_task_tx(tx,&value)?;
        let mut state=AgentState::new("task-agent");state.total_message_count=1;
        let facts=register_task_writes_tx(tx,&proof,&keys,&state,&request())?;
        // There was no actual Result write. The original admitted generation
        // now has its legal Running -> Completed Task, so the dependency becomes valid.
        // Terminal Task fences are immutable; do not simulate a terminal rewrite.
        assert_eq!(facts.delta.coverage.result_dependencies[&result.result_identity].result,
            capture.result_dependency(&result.result_identity)?.result);
        assert_eq!(keys.len(), 1);
        assert!(keys.iter().all(|key| key.family == TaskFamily::Task));
        let key = TaskKey::new(TaskFamily::Task, &value.id);
        assert!(facts.delta.records.iter().find(|record|record.locator.key==key).unwrap().locator.revision
            > capture.lookup(&key)?.unwrap().locator.revision);
        let error=validate_task_domain_observation_tx(tx,&scope(),old.coverage().revision).unwrap_err();
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::RevisionMismatch{expected,actual})
            if *expected==old.coverage().revision && *actual==Some(facts.delta.revision)));
        Ok(facts)
    })?;
    let next = old.with_committed(&updated.delta)?;
    assert!(matches!(
        next.capture(&request().capture)?
            .result_dependency(&result.result_identity)?
            .relation,
        TaskResultDependencyRelation::QualifiedCurrent { .. }
    ));
    assert!(matches!(
        capture.result_dependency(&result.result_identity)?.relation,
        TaskResultDependencyRelation::Invalid {
            reason: TaskDependencyInvalidReason::PredicateMismatch,
            ..
        }
    ));
    db.transaction(|tx| {
        // Explicit corruption of a qualified body cannot be downgraded into a
        // harmless invalid predicate observation.
        tx.execute("UPDATE tasks SET payload_json=json_set(payload_json,'$.detail.rejoin_generation',3) WHERE task_id=?1",[&value.id])?;
        let error=prepare_task_domain_snapshot_tx(tx,&scope(),&request()).unwrap_err();
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::CanonicalChanged(key)) if key.id==value.id));
        Ok(())
    })
}

#[test]
fn pruned_and_fixed_unavailable_result_dependencies_are_distinct_and_never_revive() -> Result<()> {
    let (_dir, db) = fresh()?;
    let value = result_task("pruned-task");
    let result = db.transaction(|tx| {
        write_task(tx, &value)?;
        qualify_result(
            tx,
            &result_source("pruned-result-message", &value.id),
            &value,
        )
    })?;
    db.transaction(|tx| {
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &value.id)]);
        let proof = prepare_existing_task_maintenance_tx(tx, &scope(), &keys)?;
        tx.execute("DELETE FROM tasks WHERE task_id=?1", [&value.id])?;
        register_existing_task_maintenance_tx(tx, proof, &keys)?;
        let current =
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert!(matches!(
            current
                .resident
                .capture(&request().capture)?
                .result_dependency(&result.result_identity)?
                .relation,
            TaskResultDependencyRelation::Invalid {
                target: TaskDependencyTarget::Pruned { .. },
                ..
            }
        ));
        // Simulate a restore behind the original writer gate; it cannot replace
        // the tombstone or grant this body via a Result dependency.
        repositories::upsert_task_tx(tx, &value)?;
        assert!(prepare_task_domain_snapshot_tx(tx, &scope(), &request()).is_err());
        assert_eq!(
            alias(tx, &scope(), &TaskKey::new(TaskFamily::Task, &value.id))?
                .unwrap()
                .availability,
            TaskAvailability::Pruned
        );
        Ok(())
    })?;
    let mut fixed_id = String::new();
    let (_dir, legacy_db) = legacy(true, |tx| {
        let value = result_task("missing-at-original-cutover");
        let (source, _) = append_message_tx(
            tx,
            &result_source("fixed-missing-result-message", &value.id),
        )?;
        let result = TaskResultSettlementRecord::pending(&value, &source, at())?.unwrap();
        upsert_pending_tx(tx, &result)?;
        // The fixture is a real v81 source/result, with no actual Task row.
        let mut state = AgentState::new("task-agent");
        state.total_message_count = 1;
        upsert_agent_state_tx(tx, &state)?;
        fixed_id = result.result_identity;
        Ok(())
    })?;
    legacy_db.transaction(|tx| {
        let current =
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert!(matches!(
            current
                .resident
                .capture(&request().capture)?
                .result_dependency(&fixed_id)?
                .relation,
            TaskResultDependencyRelation::Invalid {
                target: TaskDependencyTarget::FixedUnavailable { .. },
                ..
            }
        ));
        Ok(())
    })
}

#[test]
fn real_v81_foreign_invalid_result_upgrades_without_foreign_task_qualification() -> Result<()> {
    let mut result_id = String::new();
    let (_dir, db) = legacy(true, |tx| {
        let expected = result_task("foreign-invalid-task");
        let mut foreign = expected.clone();
        foreign.agent_id = "foreign-owner".into();
        repositories::upsert_task_tx(tx, &foreign)?;
        let (source, _) = append_message_tx(
            tx,
            &result_source("foreign-invalid-result-message", &expected.id),
        )?;
        let result = TaskResultSettlementRecord::pending(&expected, &source, at())?.unwrap();
        upsert_pending_tx(tx, &result)?;
        let mut state = AgentState::new("task-agent");
        state.total_message_count = 1;
        upsert_agent_state_tx(tx, &state)?;
        result_id = result.result_identity;
        Ok(())
    })?;
    db.transaction(|tx| {
        let current=prepare_task_domain_snapshot_tx(tx,&scope(),&request())?.require_current()?;
        assert!(current.resident.coverage().unsettled_complete);
        assert!(matches!(current.resident.capture(&request().capture)?.result_dependency(&result_id)?.relation,
            TaskResultDependencyRelation::Invalid{reason:TaskDependencyInvalidReason::ForeignOwner,..}));
        assert!(alias(tx,&scope(),&TaskKey::new(TaskFamily::Task,"foreign-invalid-task"))?.is_none());
        let original_seed = binding(tx, &scope())?.context("original fixed seed")?;
        assert_eq!(tx.query_row("SELECT COUNT(*) FROM task_domain_eligibility WHERE seed_id=?1 AND family='task' AND record_id='foreign-invalid-task'",[original_seed],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    })?;
    assert!(db
        .task_result_settlements()
        .admit_message(
            "task-agent",
            "foreign-invalid-result-message",
            "original-activation",
            at() + Duration::seconds(1)
        )?
        .is_none());
    let actual = db
        .task_result_settlements()
        .latest_for_message("foreign-invalid-result-message")?
        .unwrap();
    assert_eq!(
        actual.disposition,
        Some(TaskResultSettlementDisposition::InvalidOrStale)
    );
    Ok(())
}

#[test]
fn pending_relation_guard_is_selected_and_released_without_task_body_authority() -> Result<()> {
    let (_dir, db) = fresh()?;
    let value = result_task("missing-pending-task");
    let result = db.transaction(|tx| {
        qualify_result(
            tx,
            &result_source("pending-pair-message", &value.id),
            &value,
        )
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let before = ledger.report()?.observed_bytes;
        let selected = prepare_pending_task_result_dependency_tx(
            tx,
            &scope(),
            &result.message_id,
            &result.task_id,
            &request(),
            &ledger,
        )?
        .unwrap();
        assert!(matches!(
            &selected.observation.relation,
            TaskResultDependencyRelation::Invalid {
                target: TaskDependencyTarget::GlobalAbsent { .. },
                ..
            }
        ));
        assert!(ledger.report()?.observed_bytes > before);
        assert!(prepare_pending_task_result_dependency_tx(
            tx,
            &scope(),
            &result.message_id,
            "wrong-task",
            &request(),
            &ledger
        )
        .is_err());
        let retained = ledger.report()?.observed_bytes;
        assert!(retained > before);
        assert!(prepare_pending_task_result_dependency_tx(
            tx,
            &scope(),
            "no-settlement",
            &result.task_id,
            &request(),
            &ledger
        )?
        .is_none());
        drop(selected);
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(())
    })
}

#[test]
fn result_predicate_keeps_original_signed_task_timestamp_revision() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut value = result_task("old-clock-task");
    value.created_at = "1969-12-31T23:59:59.999Z".parse()?;
    value.updated_at = value.created_at;
    db.transaction(|tx| {
        write_task(tx, &value)?;
        let result = qualify_result(tx, &result_source("old-clock-message", &value.id), &value)?;
        let current =
            prepare_task_domain_snapshot_tx(tx, &scope(), &request())?.require_current()?;
        assert!(matches!(
            current
                .resident
                .capture(&request().capture)?
                .result_dependency(&result.result_identity)?
                .relation,
            TaskResultDependencyRelation::QualifiedCurrent { .. }
        ));
        Ok(())
    })
}

#[test]
fn pending_normalized_address_is_budgeted_before_id_decode_or_projection_error() -> Result<()> {
    let (_dir, db) = fresh()?;
    let value = result_task("address-budget-task");
    let result = db.transaction(|tx| {
        qualify_result(
            tx,
            &result_source("address-budget-message", &value.id),
            &value,
        )
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let huge = "x".repeat(64 * 1024);
        // Deliberately corrupt only the normalized ID. The small JSON still
        // names its original ID, so material admission must precede that error.
        tx.execute(
            "UPDATE task_result_settlements SET result_identity=?1 WHERE message_id=?2",
            params![huge, result.message_id],
        )?;
        let before = ledger.report()?.observed_bytes;
        let mut small = request();
        small.byte_limit = 8 * 1024;
        let error = prepare_pending_task_result_dependency_tx(
            tx,
            &scope(),
            &result.message_id,
            &result.task_id,
            &small,
            &ledger,
        )
        .err()
        .context("oversized address must fail")?;
        assert!(matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted("bytes"))
        ));
        assert_eq!(ledger.report()?.observed_bytes, before);
        // With room reserved, the malformed identity/projection remains strict,
        // and all newly registered temporary owned guards are released on Err.
        assert!(prepare_pending_task_result_dependency_tx(
            tx,
            &scope(),
            &result.message_id,
            &result.task_id,
            &request(),
            &ledger
        )
        .is_err());
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(())
    })
}
#[test]
fn pending_empty_address_is_rejected_after_reserved_material_is_released() -> Result<()> {
    let (_dir, db) = fresh()?;
    let value = result_task("empty-address-task");
    let result = db.transaction(|tx| {
        qualify_result(
            tx,
            &result_source("empty-address-message", &value.id),
            &value,
        )
    })?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE task_result_settlements SET result_identity='' WHERE message_id=?1",
            [&result.message_id],
        )?;
        let before = ledger.report()?.observed_bytes;
        let error = prepare_pending_task_result_dependency_tx(
            tx,
            &scope(),
            &result.message_id,
            &result.task_id,
            &request(),
            &ledger,
        )
        .err()
        .context("empty address must fail")?;
        assert!(format!("{error:#}").contains("address is invalid"));
        assert_eq!(ledger.report()?.observed_bytes, before);
        Ok(())
    })
}

#[path = "original_result_predicate_tests.rs"]
mod original_result_predicate_tests;

#[path = "original_result_whole_tests.rs"]
mod original_result_whole_tests;
