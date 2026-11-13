//! Task metadata read bounds and an explicit synthetic writer profile.
use super::*;
use crate::runtime_db::transitions::{
    PreparedInputTransition, PreparedTaskResultTransition, TaskResultOperation,
};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::{cell::RefCell, time::Instant};

fn defer(db: &RuntimeDb, operation_id: &str, ledger: &InputEvidenceBudgetLedger) -> Result<()> {
    let prepared = PreparedInputTransition::task_result(
        scope(),
        operation_id.into(),
        at(),
        PreparedTaskResultTransition::new(
            scope().agent_id,
            TaskResultOperation::Defer {
                message_id: "absent-result-source".into(),
                reason: "synthetic-observation".into(),
                at: at(),
                deadline: at() + Duration::seconds(30),
            },
        )?,
    )?;
    drop(
        db.transitions_in_scope(&scope())
            .commit_prepared_transition_with_work_request_and_memory_budget(
                &prepared,
                &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
                    capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                    record_limit: 4096,
                    byte_limit: 4 * 1024 * 1024,
                },
                ledger,
                &request(),
            )?,
    );
    Ok(())
}

thread_local! {
    static SQL_PROFILE: RefCell<BTreeMap<&'static str, (usize, u128)>> = const { RefCell::new(BTreeMap::new()) };
}

fn profile(sql: &str, duration: std::time::Duration) {
    let kind = if sql.contains("json_each(coverage_json") {
        "coverage_node_count"
    } else if sql.contains("SELECT coverage_json") {
        "coverage_read"
    } else if sql.contains("coverage_json") {
        "coverage_other"
    } else if sql.contains("roots_json") {
        "checkpoint_other"
    } else {
        "other"
    };
    SQL_PROFILE.with(|profile| {
        let mut profile = profile.borrow_mut();
        let item = profile.entry(kind).or_default();
        item.0 += 1;
        item.1 += duration.as_micros();
    });
}

#[test]
fn revision_only_task_proof_does_not_read_coverage_payload() -> Result<()> {
    let (_dir, db) = fresh()?;
    let mut connection = db.connection()?;
    connection.execute(
        "INSERT INTO task_domain_current(agent_id,identity_incarnation,revision,coverage_json) VALUES(?1,2,9,'not-json')",
        [&scope().agent_id],
    )?;
    connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Read {
            table_name: "task_domain_current",
            column_name: "coverage_json",
            ..
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }));
    let tx = connection.transaction()?;
    assert_eq!(loader::current_revision(&tx, &scope())?, Some(1));
    assert_eq!(
        loader::current_revision(
            &tx,
            &InputEvidenceScope {
                identity_incarnation: 2,
                ..scope()
            }
        )?,
        Some(9)
    );
    assert_eq!(
        loader::current_revision(
            &tx,
            &InputEvidenceScope {
                agent_id: "other-agent".into(),
                ..scope()
            }
        )?,
        None
    );
    let proof = prepare_task_write_tx(&tx, &scope(), &BTreeSet::new())?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    // Actual snapshot preflight must still acquire and validate the payload.
    assert!(
        preflight_task_write_snapshot_with_budget_tx(&tx, &proof, &request(), &ledger).is_err()
    );
    Ok(())
}

#[test]
fn task_result_defer_bounds_full_coverage_reads() -> Result<()> {
    let (_dir, db) = fresh()?;
    db.transaction(|tx| write_task(tx, &task("running-command", TaskStatus::Running)).map(|_| ()))?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.writer
        .state
        .connection
        .lock()
        .unwrap()
        .profile(Some(profile));
    SQL_PROFILE.with(|profile| profile.borrow_mut().clear());
    defer(&db, "metadata-read-bound", &ledger)?;
    db.writer.state.connection.lock().unwrap().profile(None);
    let reads = SQL_PROFILE.with(|profile| {
        profile
            .borrow()
            .get("coverage_read")
            .map_or(0, |entry| entry.0)
    });
    // Real snapshots read coverage; revision-only checks must not add two more.
    assert!(
        reads > 0 && reads <= 3,
        "TaskResult decoded coverage {reads} times"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    db.transaction(|tx| {
        let current = stored_on(
            tx,
            "task-agent",
            &TaskKey::new(TaskFamily::Task, "running-command"),
        )?
        .unwrap();
        let CanonicalTaskFact::Task(current) = current.fact else {
            unreachable!()
        };
        assert_eq!(current.status, TaskStatus::Running);
        Ok(())
    })
}

#[test]
fn metadata_node_accounting_preserves_nested_collections_and_releases_ownership() -> Result<()> {
    let (_dir, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    db.transaction(|tx| {
        let mut coverage = current_coverage(tx, &scope())?.unwrap();
        coverage.query.born_task_seqs.insert("task-a".into(), 1);
        coverage.query.reply_task_ids.insert(
            "message-a".into(),
            BTreeSet::from(["task-a".into(), "task-b".into()]),
        );
        coverage
            .task_result_ids
            .insert("task-a".into(), BTreeSet::from(["result-a".into()]));
        coverage.activation_result_ids.insert(
            "activation-a".into(),
            BTreeSet::from(["result-a".into(), "result-b".into()]),
        );
        coverage
            .reply_result_ids
            .insert("reply-a".into(), BTreeSet::from(["result-b".into()]));
        coverage.stop.pending_effect_seqs.extend([1, 2]);
        coverage.observations.insert(
            TaskKey::new(TaskFamily::Task, "absent"),
            TaskAvailability::Absent,
        );
        loader::persist_coverage(tx, &scope(), &coverage)?;
        let scope = scope();
        let request = request();
        let mut material =
            crate::runtime_db::task_snapshot::read_preparation::TaskReadMaterial::new(
                &scope,
                &request,
                Some(&ledger),
            );
        assert_eq!(material.read_current_metadata(tx)?, Some(coverage.clone()));
        assert_eq!(
            material.material_estimate().records,
            (1 + coverage.owned_metadata_record_count()?) * 4
        );
        assert!(material.material_estimate().bytes > 0);
        assert!(ledger.report()?.observed_bytes > 0);
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn metadata_count_keeps_duplicate_and_corruption_checks() -> Result<()> {
    let (_dir, db) = fresh()?;
    let original = db.transaction(|tx| {
        Ok(serde_json::to_string(
            &current_coverage(tx, &scope())?.unwrap(),
        )?)
    })?;
    let duplicate_map = original.replace(
        "\"task_result_ids\":{}",
        "\"task_result_ids\":{\"task\":[\"one\"],\"task\":[\"one\"]}",
    );
    let duplicate_set = original.replace(
        "\"reply_result_ids\":{}",
        "\"reply_result_ids\":{\"reply\":[\"one\",\"one\"]}",
    );
    assert_ne!(duplicate_map, original);
    assert_ne!(duplicate_set, original);
    let mismatch = original.replace("\"revision\":1", "\"revision\":2");
    assert_ne!(mismatch, original);
    for (raw, message) in [
        (duplicate_map, Some("metadata count differs")),
        (duplicate_set, Some("metadata count differs")),
        (mismatch, Some("revision projection mismatch")),
        ("{".into(), None),
    ] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let error = db.transaction(|tx| {
            tx.execute("UPDATE task_domain_current SET coverage_json=?1 WHERE agent_id=?2 AND identity_incarnation=1", params![raw, scope().agent_id])?;
            let scope = scope();
            let request = request();
            let mut material = crate::runtime_db::task_snapshot::read_preparation::TaskReadMaterial::new(&scope, &request, Some(&ledger));
            material.read_current_metadata(tx)
        }).unwrap_err();
        if let Some(message) = message {
            assert!(error.to_string().contains(message), "{error:#}");
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
#[ignore = "explicit synthetic writer profiling"]
fn task_result_metadata_cost_profile() -> Result<()> {
    for records in [1_000, 10_000, 50_000] {
        let (_dir, db) = fresh()?;
        let mut coverage = db.transaction(|tx| {
            current_coverage(tx, &scope())?.context("fixture coverage missing")
        })?;
        for index in 0..records {
            let key = TaskKey::new(
                TaskFamily::Task,
                format!("observed-empty-task-{index:08}-{}", "x".repeat(96)),
            );
            coverage.observations.insert(key, TaskAvailability::Absent);
        }
        let bytes = serde_json::to_vec(&coverage)?.len();
        db.writer
            .state
            .connection
            .lock()
            .unwrap()
            .profile(Some(profile));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        for mode in ["metadata", "result_defer"] {
            db.transaction(|tx| loader::persist_coverage(tx, &scope(), &coverage))?;
            SQL_PROFILE.with(|profile| profile.borrow_mut().clear());
            let attribution_before = crate::diagnostics::attribution::snapshot();
            let started = Instant::now();
            if mode == "metadata" {
                db.transaction(|tx| {
                    let scope = scope();
                    let request = request();
                    let mut material =
                        crate::runtime_db::task_snapshot::read_preparation::TaskReadMaterial::new(
                            &scope,
                            &request,
                            Some(&ledger),
                        );
                    let actual = material.read_current_metadata(tx)?.unwrap();
                    assert_eq!(actual, coverage);
                    Ok(())
                })?;
            } else {
                defer(&db, &format!("profile-defer-{records}"), &ledger)?;
            }
            let micros = started.elapsed().as_micros();
            let defer_stages_us = attribution_before
                .into_iter()
                .zip(crate::diagnostics::attribution::snapshot())
                .filter(|(before, after)| {
                    after.name.starts_with("db.task_result.defer.") && after.count > before.count
                })
                .map(|(before, after)| (after.name, (after.total_ns - before.total_ns) / 1_000))
                .collect::<BTreeMap<_, _>>();
            SQL_PROFILE.with(|profile| {
                eprintln!(
                    "SYNTHETIC_PROFILE {}",
                    serde_json::json!({
                        "records": records, "coverage_bytes": bytes, "mode": mode,
                        "total_us": micros, "sql": *profile.borrow(),
                        "defer_stages_us": defer_stages_us,
                    })
                );
            });
        }
        db.writer.state.connection.lock().unwrap().profile(None);
    }
    Ok(())
}
