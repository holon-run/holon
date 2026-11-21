use super::*;
#[path = "write_delta_stop_tests.rs"]
mod stop;
use crate::{
    config::StateMemoryLimits,
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        evidence::{append_message_tx, upsert_agent_identity_tx},
        task_result_settlement::{
            upsert_pending_tx, TaskResultSettlementDisposition, TaskResultSettlementRecord,
        },
        task_snapshot::tests::{at, fresh, identity, request, scope, task},
    },
    types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        TaskRecord, TaskStatus,
    },
};

fn seed_tasks(tx: &Transaction<'_>, values: &[TaskRecord]) -> Result<()> {
    let keys = values
        .iter()
        .map(|value| TaskKey::new(TaskFamily::Task, &value.id))
        .collect();
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    for value in values {
        repositories::upsert_task_tx(tx, value)?;
    }
    register_task_writes_tx(
        tx,
        &proof,
        &keys,
        &AgentState::new("task-agent"),
        &request(),
    )?;
    Ok(())
}

fn assert_whole_ack(actual: &CommittedTaskFacts, expected: &CommittedTaskFacts) {
    assert_eq!(actual.delta.scope, expected.delta.scope);
    assert_eq!(actual.delta.base_revision, expected.delta.base_revision);
    assert_eq!(actual.delta.revision, expected.delta.revision);
    assert_eq!(actual.delta.records, expected.delta.records);
    assert_eq!(actual.delta.stop_records, expected.delta.stop_records);
    assert_eq!(actual.delta.removals, expected.delta.removals);
    assert_eq!(actual.delta.coverage, expected.delta.coverage);
    assert_eq!(actual._input_roots, expected._input_roots);
    assert_eq!(actual._work_roots, expected._work_roots);
}

#[test]
fn incremental_ack_reobserves_invalid_raw_dependency_cleanup() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let mut completed = task("raw-target", TaskStatus::Completed);
        completed.detail = Some(serde_json::json!({
            "rejoin_obligation_id": completed.id, "rejoin_generation": 1,
            "parent_turn_id": "raw-parent",
        }));
        let mut raw = completed.clone();
        raw.status = TaskStatus::Running;
        repositories::upsert_task_tx(tx, &raw)?;
        let mut source = MessageEnvelope::new(
            "task-agent",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: completed.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "old result".into(),
            },
        );
        source.id = "raw-target-result".into();
        source.task_id = Some(completed.id.clone());
        source.created_at = at();
        let (source, _) = append_message_tx(tx, &source)?;
        let result = TaskResultSettlementRecord::pending(&completed, &source, at())?.unwrap();
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
        let actual = oracle(tx, &request(), &BTreeSet::new(), |tx| {
            // Raw cleanup is business SQL, not a qualified Task-domain write.
            tx.execute("DELETE FROM tasks WHERE task_id='raw-target'", [])?;
            Ok(())
        })?;
        assert!(matches!(
            actual.delta.coverage.result_dependencies[&result.result_identity].relation,
            TaskResultDependencyRelation::Invalid { .. }
        ));
        Ok(())
    })
}

/// Replay the same business write under a savepoint, once through the delta
/// finalizer and once through the pre-existing full-loader oracle.
fn oracle(
    tx: &Transaction<'_>,
    load: &TaskDomainLoadRequest,
    keys: &BTreeSet<TaskKey>,
    write: impl FnMut(&Transaction<'_>) -> Result<()>,
) -> Result<CommittedTaskFacts> {
    oracle_with_material(tx, load, keys, write).map(|(facts, _)| facts)
}

fn oracle_with_material(
    tx: &Transaction<'_>,
    load: &TaskDomainLoadRequest,
    keys: &BTreeSet<TaskKey>,
    mut write: impl FnMut(&Transaction<'_>) -> Result<()>,
) -> Result<(CommittedTaskFacts, usize)> {
    let scope = scope();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let proof = prepare_task_write_tx(tx, &scope, keys)?;
    let baseline = preflight_task_write_snapshot_with_budget_tx(tx, &proof, load, &ledger)?
        .require_current()?;
    let reused = baseline
        .resident
        .selected_install_records()
        .into_iter()
        .find(|record| !keys.contains(&record.locator.key));
    tx.execute_batch("SAVEPOINT task_delta_oracle")?;
    write(tx)?;
    let mut material = read_preparation::TaskReadMaterial::new(&scope, load, Some(&ledger));
    let actual = proof.register_incremental_tx(
        tx,
        baseline,
        keys,
        &AgentState::new("task-agent"),
        load,
        &ledger,
        &mut material,
        None,
    )?;
    if let Some(reused) = reused {
        if let Some(record) = actual
            .delta
            .records
            .iter()
            .find(|r| r.locator.key == reused.locator.key)
        {
            assert!(
                Arc::ptr_eq(record, &reused),
                "unchanged selected Task body was reloaded"
            );
        }
    }
    let material_bytes = material.material_estimate().bytes;
    drop(material);
    tx.execute_batch("ROLLBACK TO task_delta_oracle")?;
    write(tx)?;
    let expected = register_task_writes_with_budget_tx(
        tx,
        &proof,
        keys,
        &AgentState::new("task-agent"),
        load,
        &ledger,
    )?;
    assert_whole_ack(&actual, &expected);
    tx.execute_batch("RELEASE task_delta_oracle")?;
    Ok((actual, material_bytes))
}

#[test]
fn incremental_changed_result_reuses_prepared_target_body() -> Result<()> {
    fn run(body_bytes: usize) -> Result<(usize, usize)> {
        let (_home, db) = fresh()?;
        db.transaction(|tx| {
            let mut value = task("prepared-target", TaskStatus::Completed);
            value.summary = Some("x".repeat(body_bytes));
            value.detail = Some(serde_json::json!({
                "rejoin_obligation_id": value.id, "rejoin_generation": 1,
                "parent_turn_id": "prepared-parent",
            }));
            seed_tasks(tx, &[value.clone()])?;
            let mut source = MessageEnvelope::new(
                "task-agent",
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: value.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "completed".into(),
                },
            );
            source.id = "prepared-target-result".into();
            source.task_id = Some(value.id.clone());
            source.created_at = at();
            let (source, _) = append_message_tx(tx, &source)?;
            let result = TaskResultSettlementRecord::pending(&value, &source, at())?.unwrap();
            let owner = scope();
            let task_key = TaskKey::new(TaskFamily::Task, &value.id);
            let mut stale = stored_on(tx, &owner.agent_id, &task_key)?
                .unwrap()
                .qualified(&owner, alias(tx, &owner, &task_key)?.unwrap().revision)?
                .locator
                .clone();
            stale.revision += 1;
            let load = request();
            let mut material = read_preparation::TaskReadMaterial::new(&owner, &load, None);
            let error = result_relation::observe_task_predicate_tx(
                tx,
                &owner,
                binding(tx, &owner)?.as_deref(),
                &result,
                Some(&stale),
                &load,
                &mut material,
            )
            .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<TaskSnapshotError>(),
                Some(TaskSnapshotError::CanonicalChanged(_))
            ));
            let keys = BTreeSet::from([TaskKey::new(
                TaskFamily::ResultSettlement,
                &result.result_identity,
            )]);
            let (_, inserted_bytes) = oracle_with_material(tx, &load, &keys, |tx| {
                upsert_pending_tx(tx, &result).map(|_| ())
            })?;
            let mut deferred = result.clone();
            deferred.next_recheck_at = Some(at() + chrono::Duration::seconds(3));
            let (_, changed_bytes) = oracle_with_material(tx, &load, &keys, |tx| {
                upsert_pending_tx(tx, &deferred).map(|_| ())
            })?;
            Ok((inserted_bytes, changed_bytes))
        })
    }
    // Both ACKs are compared with the full-loader oracle. Neither insertion
    // nor a changed dependency may acquire the already selected target again.
    assert_eq!(run(16)?, run(256 * 1024)?);
    Ok(())
}

#[test]
fn incremental_task_ack_matches_oracle_with_terminal_lookahead_and_trim() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let values = [
            task("active-unchanged", TaskStatus::Running),
            task("terminal-a", TaskStatus::Completed),
            task("terminal-b", TaskStatus::Completed),
            task("terminal-c", TaskStatus::Completed),
        ];
        seed_tasks(tx, &values)?;
        let mut load = request();
        load.capture.terminal_limit = 2;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &values[1].id)]);
        let actual = oracle(tx, &load, &keys, |tx| {
            tx.execute("DELETE FROM tasks WHERE task_id=?1", [&values[1].id])?;
            Ok(())
        })?;
        assert_eq!(
            actual.delta.coverage.terminal.keys,
            vec![
                TaskKey::new(TaskFamily::Task, "terminal-b"),
                TaskKey::new(TaskFamily::Task, "terminal-c"),
            ]
        );
        assert!(actual.delta.coverage.terminal.is_exhaustive);
        let mut active = values[0].clone();
        assert!(actual.delta.coverage.active_task_ids.contains(&active.id));
        active.status = TaskStatus::Completed;
        active.updated_at += chrono::Duration::seconds(1);
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &active.id)]);
        let actual = oracle(tx, &load, &keys, |tx| {
            repositories::upsert_task_tx(tx, &active)?;
            Ok(())
        })?;
        assert!(!actual.delta.coverage.terminal.is_exhaustive);
        assert_eq!(
            actual.delta.coverage.terminal.keys,
            vec![
                TaskKey::new(TaskFamily::Task, &active.id),
                TaskKey::new(TaskFamily::Task, "terminal-b"),
            ]
        );
        assert!(!actual.delta.coverage.active_task_ids.contains(&active.id));
        Ok(())
    })
}

#[test]
fn incremental_terminal_backfill_completes_partial_invalid_result_reverse_index() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let mut older = task("reverse-older", TaskStatus::Completed);
        older.detail = Some(serde_json::json!({
            "rejoin_obligation_id": older.id, "rejoin_generation": 2,
            "parent_turn_id": "reverse-parent",
        }));
        let mut latest = task("reverse-latest", TaskStatus::Completed);
        latest.updated_at += chrono::Duration::seconds(1);
        seed_tasks(tx, &[older.clone(), latest.clone()])?;
        let mut results = Vec::new();
        for generation in [1, 2] {
            let mut source_task = older.clone();
            source_task.detail = Some(serde_json::json!({
                "rejoin_obligation_id": older.id, "rejoin_generation": generation,
                "parent_turn_id": "reverse-parent",
            }));
            let mut source = MessageEnvelope::new(
                "task-agent",
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: older.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "completed".into(),
                },
            );
            source.id = format!("reverse-result-{generation}");
            source.task_id = Some(older.id.clone());
            source.created_at = at();
            let (source, _) = append_message_tx(tx, &source)?;
            let mut result =
                TaskResultSettlementRecord::pending(&source_task, &source, at())?.unwrap();
            if generation == 2 {
                result.state = TaskResultSettlementState::Settled;
                result.disposition = Some(TaskResultSettlementDisposition::OwnerClosed);
                result.settled_at = Some(at());
            }
            results.push(result);
        }
        let result_keys = results
            .iter()
            .map(|result| TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity))
            .collect();
        let proof = prepare_task_write_tx(tx, &scope(), &result_keys)?;
        for result in &results {
            upsert_pending_tx(tx, result)?;
        }
        register_task_writes_tx(
            tx,
            &proof,
            &result_keys,
            &AgentState::new("task-agent"),
            &request(),
        )?;

        let mut load = request();
        load.capture.terminal_limit = 1;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &latest.id)]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let baseline = preflight_task_write_snapshot_with_budget_tx(tx, &proof, &load, &ledger)?
            .require_current()?;
        let coverage = baseline.resident.coverage();
        assert_eq!(
            coverage
                .observations
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                TaskKey::new(TaskFamily::Task, &latest.id),
                TaskKey::new(TaskFamily::ResultSettlement, &results[0].result_identity),
            ])
        );
        assert_eq!(
            coverage.terminal.keys,
            vec![TaskKey::new(TaskFamily::Task, &latest.id)]
        );
        assert!(!coverage.task_metadata.contains_key(&older.id));
        assert_eq!(
            coverage.task_result_ids[&older.id],
            BTreeSet::from([results[0].result_identity.clone()])
        );
        assert!(matches!(
            coverage.result_dependencies[&results[0].result_identity].relation,
            TaskResultDependencyRelation::Invalid {
                reason: TaskDependencyInvalidReason::PredicateMismatch,
                ..
            }
        ));
        assert!(!coverage
            .result_metadata
            .contains_key(&results[1].result_identity));

        let actual = oracle(tx, &load, &keys, |tx| {
            tx.execute("DELETE FROM tasks WHERE task_id=?1", [&latest.id])?;
            Ok(())
        })?;
        assert_eq!(
            actual.delta.coverage.terminal.keys,
            vec![TaskKey::new(TaskFamily::Task, &older.id)]
        );
        assert!(actual.delta.coverage.terminal.is_exhaustive);
        assert_eq!(
            actual.delta.coverage.task_result_ids[&older.id],
            results
                .iter()
                .map(|result| result.result_identity.clone())
                .collect()
        );
        assert!(actual.delta.records.iter().any(|record| {
            record.locator.key
                == TaskKey::new(TaskFamily::ResultSettlement, &results[1].result_identity)
        }));
        Ok(())
    })
}

#[test]
fn incremental_task_ack_matches_oracle_for_new_active_query_and_noop() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        seed_tasks(tx, &[task("unchanged", TaskStatus::Running)])?;
        let mut load = request();
        load.capture.terminal_limit = 0;
        load.query.birth_after = Some(0);
        let value = task("new-active", TaskStatus::Queued);
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &value.id)]);
        let actual = oracle(tx, &load, &keys, |tx| {
            repositories::upsert_task_tx(tx, &value)?;
            Ok(())
        })?;
        assert!(actual.delta.coverage.active_task_ids.contains(&value.id));
        assert!(actual
            .delta
            .coverage
            .query
            .all_selected_task_ids()
            .any(|id| id == value.id));
        let noop = oracle(tx, &load, &keys, |_| Ok(()))?;
        assert_eq!(noop.delta.base_revision, noop.delta.revision);
        assert!(noop.delta.records.is_empty());
        Ok(())
    })
}

#[test]
fn incremental_result_ack_matches_oracle_and_preserves_reverse_dependencies() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let mut value = task("result-task", TaskStatus::Completed);
        value.detail = Some(serde_json::json!({
            "rejoin_obligation_id": value.id, "rejoin_generation": 1,
            "parent_turn_id": "original-parent",
        }));
        seed_tasks(tx, &[value.clone(), task("unchanged", TaskStatus::Running)])?;
        let mut source = MessageEnvelope::new(
            "task-agent", MessageKind::TaskResult,
            MessageOrigin::Task { task_id: value.id.clone() },
            AuthorityClass::RuntimeInstruction, Priority::Normal,
            MessageBody::Text { text: "completed".into() },
        );
        source.id = "delta-result-message".into();
        source.task_id = Some(value.id.clone());
        source.created_at = at();
        let (source, _) = append_message_tx(tx, &source)?;
        let result = TaskResultSettlementRecord::pending(&value, &source, at())?.unwrap();
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::ResultSettlement, &result.result_identity)]);
        let load = request();
        let actual = oracle(tx, &load, &keys, |tx| upsert_pending_tx(tx, &result).map(|_| ()))?;
        assert!(actual.delta.coverage.unsettled_result_ids.contains(&result.result_identity));
        assert!(actual.delta.coverage.task_result_ids[&value.id].contains(&result.result_identity));
        assert!(actual.delta.coverage.result_dependencies.contains_key(&result.result_identity));
        assert!(actual._input_roots.contains(&InputEvidenceKey::new(InputEvidenceFamily::Turn, "original-parent")));
        let mut deferred = result.clone();
        deferred.next_recheck_at = Some(at() + chrono::Duration::seconds(3));
        let actual = oracle(tx, &load, &keys, |tx| {
            tx.execute(
                "UPDATE task_result_settlements SET next_recheck_at=?2,payload_json=?3 WHERE result_identity=?1",
                params![deferred.result_identity, deferred.next_recheck_at.map(repositories::timestamp),
                    serde_json::to_string(&deferred)?],
            )?;
            Ok(())
        })?;
        assert!(actual.delta.coverage.result_metadata[&result.result_identity].next_recheck_at.is_some());
        Ok(())
    })
}

#[test]
fn incremental_finalizer_rejects_outside_w1_and_retired_scope() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let scope = scope();
        let load = request();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let keys = BTreeSet::new();
        let proof = prepare_task_write_tx(tx, &scope, &keys)?;
        let baseline = preflight_task_write_snapshot_with_budget_tx(tx, &proof, &load, &ledger)?
            .require_current()?;
        let mut material = read_preparation::TaskReadMaterial::new(&scope, &load, Some(&ledger));
        let error = proof
            .register_incremental_tx(
                tx,
                baseline,
                &BTreeSet::from([TaskKey::new(TaskFamily::Task, "outside")]),
                &AgentState::new("task-agent"),
                &load,
                &ledger,
                &mut material,
                None,
            )
            .unwrap_err();
        assert!(error.to_string().contains("exceed pre-SQL proof"));
        let baseline = preflight_task_write_snapshot_with_budget_tx(tx, &proof, &load, &ledger)?
            .require_current()?;
        let mut retired = identity();
        retired.status = AgentRegistryStatus::Deleting;
        upsert_agent_identity_tx(tx, &retired)?;
        assert!(proof
            .register_incremental_tx(
                tx,
                baseline,
                &keys,
                &AgentState::new("task-agent"),
                &load,
                &ledger,
                &mut material,
                None,
            )
            .is_err());
        Ok(())
    })
}

#[test]
fn incremental_ack_keeps_required_pruned_observation_and_terminal_lookahead() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        seed_tasks(
            tx,
            &[
                task("terminal-a", TaskStatus::Completed),
                task("terminal-b", TaskStatus::Completed),
                task("terminal-c", TaskStatus::Completed),
            ],
        )?;
        let key = TaskKey::new(TaskFamily::Task, "terminal-a");
        let keys = BTreeSet::from([key.clone()]);
        let mut load = request();
        load.capture.terminal_limit = 2;
        load.capture.required_keys = keys.clone();
        let actual = oracle(tx, &load, &keys, |tx| {
            tx.execute("DELETE FROM tasks WHERE task_id='terminal-a'", [])?;
            Ok(())
        })?;
        assert_eq!(
            actual.delta.removals.get(&key),
            Some(&TaskAvailability::Pruned)
        );
        assert_eq!(
            actual.delta.coverage.observations.get(&key),
            Some(&TaskAvailability::Pruned)
        );
        assert_eq!(actual.delta.coverage.terminal.keys.len(), 2);
        assert!(actual.delta.coverage.terminal.is_exhaustive);
        Ok(())
    })
}

#[test]
fn incremental_ack_reobserves_recovery_metadata_without_task_body_writes() -> Result<()> {
    use crate::{
        runtime_db::input_evidence_snapshot::{
            register_committed_input_evidence_tx, CanonicalInputEvidence,
        },
        types::{AdmissionContext, MessageDeliverySurface, QueueEntryRecord, QueueEntryStatus},
    };
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        seed_tasks(tx, &[task("unchanged", TaskStatus::Running)])?;
        let mut source = MessageEnvelope::new(
            "task-agent",
            MessageKind::InternalFollowup,
            MessageOrigin::System {
                subsystem: "task_result_recovery".into(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "recover task results".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        source.id = "delta-recovery".into();
        source.created_at = at();
        let (source, _) = append_message_tx(tx, &source)?;
        register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[CanonicalInputEvidence::Message(source.clone())],
        )?;
        let mut queue = QueueEntryRecord {
            message_id: source.id.clone(),
            agent_id: scope().agent_id,
            priority: Priority::Next,
            status: QueueEntryStatus::Interrupted,
            created_at: at(),
            updated_at: at(),
        };
        repositories::upsert_queue_entry_tx(tx, &queue)?;
        let mut load = request();
        load.capture.recovery_source_ids.insert(source.id.clone());
        load.recovery_at = Some(at());
        let actual = oracle(tx, &load, &BTreeSet::new(), |tx| {
            queue.status = QueueEntryStatus::Processed;
            repositories::upsert_queue_entry_tx(tx, &queue)?;
            Ok(())
        })?;
        assert!(!actual.delta.coverage.recovery_sources[&source.id].queue_interrupted);
        assert_eq!(actual.delta.coverage.recovery_at, Some(at()));
        Ok(())
    })
}

#[test]
fn incremental_finalizer_charges_new_selected_material_before_decode() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let scope = scope();
        let mut load = request();
        load.byte_limit = 1024;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "large")]);
        let proof = prepare_task_write_tx(tx, &scope, &keys)?;
        let baseline = preflight_task_write_snapshot_with_budget_tx(tx, &proof, &load, &ledger)?
            .require_current()?;
        let mut large = task("large", TaskStatus::Running);
        large.summary = Some("x".repeat(16 * 1024));
        repositories::upsert_task_tx(tx, &large)?;
        let size = checkpoint_material_size_on(tx, keys.first().unwrap())?.unwrap();
        assert!(size > 1024);
        let mut material = read_preparation::TaskReadMaterial::new(&scope, &load, Some(&ledger));
        let before = ledger.report()?.owned.payload_bytes;
        // Original write completion observes owned material; legacy byte_limit
        // does not reject data already produced by the business write.
        let actual = proof.register_incremental_tx(
            tx,
            baseline,
            &keys,
            &AgentState::new("task-agent"),
            &load,
            &ledger,
            &mut material,
            None,
        )?;
        assert!(actual
            .delta
            .records
            .iter()
            .any(|record| record.locator.key == TaskKey::new(TaskFamily::Task, &large.id)));
        assert!(ledger.report()?.owned.payload_bytes >= before + u64::try_from(size * 8)?);
        assert!(material.material_estimate().bytes >= size * 8);
        Ok(())
    })
}

#[test]
fn incremental_finalizer_accounts_actual_writes_before_decode_failure() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let scope = scope();
        let load = request();
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "malformed")]);
        let proof = prepare_task_write_tx(tx, &scope, &keys)?;
        let baseline = preflight_task_write_snapshot_with_budget_tx(tx, &proof, &load, &ledger)?
            .require_current()?;
        let mut large = task("malformed", TaskStatus::Running);
        large.summary = Some("x".repeat(16 * 1024));
        repositories::upsert_task_tx(tx, &large)?;
        tx.execute(
            "UPDATE tasks SET payload_json='{}' WHERE task_id=?1",
            [&large.id],
        )?;
        let size = checkpoint_material_size_on(tx, keys.first().unwrap())?.unwrap();
        let mut material = read_preparation::TaskReadMaterial::new(&scope, &load, Some(&ledger));
        let before = ledger.report()?.owned.payload_bytes;
        let error = proof
            .register_incremental_tx(
                tx,
                baseline,
                &keys,
                &AgentState::new("task-agent"),
                &load,
                &ledger,
                &mut material,
                None,
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("missing field"));
        assert!(ledger.report()?.owned.payload_bytes >= before + u64::try_from(size * 8)?);
        assert!(material.material_estimate().bytes >= size * 8);
        Ok(())
    })
}

#[test]
fn adopted_baseline_replays_full_preflight_ack() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        seed_tasks(tx, &[task("adopt-live", TaskStatus::Queued)])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let mut running = task("adopt-live", TaskStatus::Running);
        running.updated_at = at();
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "adopt-live")]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        let full = preflight_task_write_snapshot_with_budget_tx(tx, &proof, &request(), &ledger)?
            .require_current()?;
        let adopted = adopt_task_write_baseline_tx(
            tx,
            &proof,
            &request(),
            &full.resident,
            &serde_json::to_string(full.resident.coverage())?,
        )?
        .expect("current complete resident is eligible");
        assert_eq!(adopted.resident.coverage(), full.resident.coverage());
        tx.execute_batch("SAVEPOINT adopted_ack")?;
        repositories::upsert_task_tx(tx, &running)?;
        let scope = scope();
        let load = request();
        let mut material = read_preparation::TaskReadMaterial::new(&scope, &load, Some(&ledger));
        let actual = proof.register_incremental_tx(
            tx,
            adopted,
            &keys,
            &AgentState::new("task-agent"),
            &load,
            &ledger,
            &mut material,
            None,
        )?;
        tx.execute_batch("ROLLBACK TO adopted_ack; RELEASE adopted_ack")?;
        let expected = oracle(tx, &request(), &keys, |tx| {
            repositories::upsert_task_tx(tx, &running)?;
            Ok(())
        })?;
        assert_whole_ack(&actual, &expected);
        Ok(())
    })
}

#[test]
fn adoption_declines_after_concurrent_write_moves_coverage() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        seed_tasks(tx, &[task("adopt-stale", TaskStatus::Queued)])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "adopt-stale")]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        let full = preflight_task_write_snapshot_with_budget_tx(tx, &proof, &request(), &ledger)?
            .require_current()?;
        // A concurrent original writer commits one revision past the proof.
        let other = BTreeSet::from([TaskKey::new(TaskFamily::Task, "adopt-rival")]);
        let rival_proof = prepare_task_write_tx(tx, &scope(), &other)?;
        repositories::upsert_task_tx(tx, &task("adopt-rival", TaskStatus::Queued))?;
        register_task_writes_tx(
            tx,
            &rival_proof,
            &other,
            &AgentState::new("task-agent"),
            &request(),
        )?;
        assert!(adopt_task_write_baseline_tx(
            tx,
            &proof,
            &request(),
            &full.resident,
            &serde_json::to_string(full.resident.coverage())?,
        )?
        .is_none());
        Ok(())
    })
}

#[test]
fn deleting_baseline_adoption_requires_original_cleanup_and_matches_full_ack() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        let live = task("deleting-adopt", TaskStatus::Running);
        seed_tasks(tx, &[live.clone()])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &live.id)]);
        let ordinary = prepare_task_write_tx(tx, &scope(), &keys)?;
        let baseline =
            preflight_task_write_snapshot_with_budget_tx(tx, &ordinary, &request(), &ledger)?
                .require_current()?;
        let coverage = serde_json::to_string(baseline.resident.coverage())?;
        let mut deleting = identity();
        deleting.status = AgentRegistryStatus::Deleting;
        upsert_agent_identity_tx(tx, &deleting)?;

        assert!(prepare_task_write_tx(tx, &scope(), &keys).is_err());
        assert!(adopt_task_write_baseline_tx(
            tx,
            &ordinary,
            &request(),
            &baseline.resident,
            &coverage,
        )?
        .is_none());
        let outside = BTreeSet::from([TaskKey::new(TaskFamily::Task, "not-original")]);
        assert!(prepare_original_task_cleanup_tx(tx, &scope(), &outside).is_err());
        let cleanup = prepare_original_task_cleanup_tx(tx, &scope(), &keys)?;
        let adopted =
            adopt_task_write_baseline_tx(tx, &cleanup, &request(), &baseline.resident, &coverage)?
                .context("original same-incarnation cleanup must adopt its baseline")?;
        let mut completed = live;
        completed.status = TaskStatus::Failed;
        tx.execute_batch("SAVEPOINT deleting_adopt_ack")?;
        repositories::upsert_task_tx(tx, &completed)?;
        let load = request();
        let owner = scope();
        let mut material = read_preparation::TaskReadMaterial::new(&owner, &load, Some(&ledger));
        let actual = cleanup.register_incremental_tx(
            tx,
            adopted,
            &keys,
            &AgentState::new("task-agent"),
            &load,
            &ledger,
            &mut material,
            None,
        )?;
        tx.execute_batch("ROLLBACK TO deleting_adopt_ack; RELEASE deleting_adopt_ack")?;
        repositories::upsert_task_tx(tx, &completed)?;
        let expected =
            register_task_writes_tx(tx, &cleanup, &keys, &AgentState::new("task-agent"), &load)?;
        assert_whole_ack(&actual, &expected);
        Ok(())
    })
}

#[test]
fn adoption_declines_recovery_request_without_reader_baseline() -> Result<()> {
    let (_home, db) = fresh()?;
    db.transaction(|tx| {
        seed_tasks(tx, &[task("adopt-covered", TaskStatus::Queued)])?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, "adopt-covered")]);
        let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
        let full = preflight_task_write_snapshot_with_budget_tx(tx, &proof, &request(), &ledger)?
            .require_current()?;
        let mut recovery = request();
        recovery.recovery_at = Some(at());
        assert!(
            adopt_task_write_baseline_tx(
                tx,
                &proof,
                &recovery,
                &full.resident,
                &serde_json::to_string(full.resident.coverage())?,
            )?
            .is_none(),
            "a current resident never substitutes a recovery-time read"
        );
        Ok(())
    })
}
