use super::*;
use crate::{
    runtime_db::{
        evidence::{upsert_agent_identity_tx, upsert_agent_state_tx},
        task_recovery::{
            commit_task_recovery_candidate_tx, prepare_bootstrap_task_domains_tx,
            prepare_task_recovery_candidate_tx,
        },
        task_snapshot::tests as fixtures,
        RuntimeDb,
    },
    types::TaskStatus,
};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::StatementStatus;

#[test]
fn fresh_and_legacy_bootstrap_initialize_before_receipt_reader_admission() -> Result<()> {
    for legacy in [false, true] {
        let (home, db) = if legacy {
            fixtures::legacy(true, |tx| {
                repositories::upsert_task_tx(
                    tx,
                    &fixtures::task("cold-legacy", TaskStatus::Running),
                )?;
                Ok(())
            })?
        } else {
            let home = tempfile::tempdir()?;
            let db = RuntimeDb::open_and_migrate(
                home.path().join("runtime.sqlite"),
                home.path().join("runtime.lock"),
            )?;
            db.transaction(|tx| {
                upsert_agent_identity_tx(tx, &fixtures::identity())?;
                upsert_agent_state_tx(tx, &AgentState::new("task-agent"))?;
                Ok(())
            })?;
            (home, db)
        };
        let _home = home;
        let scope = fixtures::scope();
        let state = AgentState::new("task-agent");
        let request = fixtures::request();
        let budget = InputEvidenceBudgetLedger::new(Default::default())?;
        let cold = db.read_for_preparation(|tx| {
            prepare_task_candidate_tx(tx, &scope, &state, &request, &budget)
                .map(|candidate| candidate.is_none())
        })?;
        assert!(
            cold,
            "uninitialized readers must not invent current membership"
        );
        db.transaction(|tx| {
            prepare_bootstrap_task_domains_tx(
                tx,
                &scope,
                &state,
                &request.capture,
                &[],
                request.record_limit,
                request.byte_limit,
                &budget,
            )
            .map(|_| ())
        })?;
        let candidate = db.read_for_preparation(|tx| {
            prepare_task_recovery_candidate_tx(
                tx,
                &scope,
                &state,
                &request.capture,
                &[],
                request.record_limit,
                request.byte_limit,
                &budget,
                None,
            )
        })?;
        let prepared = db.recovery_transaction_once(|tx| {
            tx.authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Read {
                    table_name: "tasks",
                    column_name: "payload_json",
                }
                | AuthAction::Read {
                    table_name: "episode_continuity_seeds" | "episode_continuity_imports",
                    column_name: "roots_json",
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }));
            let result = commit_task_recovery_candidate_tx(
                tx,
                &scope,
                &state,
                &request.capture,
                &[],
                request.record_limit,
                request.byte_limit,
                &budget,
                None,
                candidate,
            );
            tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            result
        })?;
        assert_eq!(prepared.resident.scope(), &scope);
        if legacy {
            assert!(prepared
                .resident
                .capture(&request.capture)?
                .lookup(&TaskKey::new(TaskFamily::Task, "cold-legacy"))?
                .is_some());
        }
        drop(prepared);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

fn legacy_reader_fixture(
    missing_messages: bool,
    terminal_count: usize,
) -> Result<(tempfile::TempDir, RuntimeDb, InputEvidenceBudgetLedger)> {
    let mut message_schema: Option<String> = None;
    let (home, db) = fixtures::legacy(true, |tx| {
        for index in 0..terminal_count {
            let mut task = fixtures::task(&format!("legacy-{index:03}"), TaskStatus::Completed);
            task.updated_at -= chrono::Duration::seconds(index as i64);
            repositories::upsert_task_tx(tx, &task)?;
        }
        if missing_messages {
            message_schema = Some(tx.query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='messages'",
                [],
                |row| row.get(0),
            )?);
            tx.execute_batch("DROP TABLE messages")?;
        }
        Ok(())
    })?;
    let budget = InputEvidenceBudgetLedger::new(Default::default())?;
    let request = fixtures::request();
    db.transaction(|tx| {
        // Current schema availability cannot retroactively turn the original
        // unavailable historical table into an observed empty namespace.
        if let Some(sql) = &message_schema {
            tx.execute_batch(sql)?;
        }
        prepare_bootstrap_task_domains_tx(
            tx,
            &fixtures::scope(),
            &AgentState::new("task-agent"),
            &request.capture,
            &[],
            request.record_limit,
            request.byte_limit,
            &budget,
        )
        .map(|_| ())
    })?;
    Ok((home, db, budget))
}

fn commit_without_history(
    db: &RuntimeDb,
    capture: &TaskCaptureRequest,
    candidate: crate::runtime_db::task_recovery::TaskRecoveryCandidate,
    budget: &InputEvidenceBudgetLedger,
) -> Result<crate::runtime_db::task_recovery::PreparedTaskDomains> {
    let request = fixtures::request();
    db.recovery_transaction_once(|tx| {
        tx.authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Read {
                table_name: "tasks" | "task_result_settlements",
                column_name: "payload_json",
            }
            | AuthAction::Read {
                table_name: "episode_continuity_seeds" | "episode_continuity_imports",
                column_name: "roots_json",
            }
            | AuthAction::Read {
                table_name: "task_stop_effects",
                column_name: "prepared_json" | "intent_json" | "ack_task_json" | "progress_json",
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }));
        let result = commit_task_recovery_candidate_tx(
            tx,
            &fixtures::scope(),
            &AgentState::new("task-agent"),
            capture,
            &[],
            request.record_limit,
            request.byte_limit,
            budget,
            None,
            candidate,
        );
        tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        result
    })
}

#[test]
fn receipt_reader_candidate_legacy_inventory_only_member_uses_exact_metadata() -> Result<()> {
    let (_home, db, budget) = legacy_reader_fixture(false, 130)?;
    let key = TaskKey::new(TaskFamily::Task, "legacy-129");
    db.transaction(|tx| {
        assert!(alias(tx, &fixtures::scope(), &key)?.is_none());
        let seed = binding(tx, &fixtures::scope())?.context("legacy seed missing")?;
        assert!(qualification::fixed_eligible_digest(tx, &seed, &key)?.is_none());
        Ok(())
    })?;
    let request = fixtures::request();
    let mut capture = request.capture;
    capture.required_keys.insert(key.clone());
    // An unknown original selector exercises the negative fallback as well.
    capture
        .required_keys
        .insert(TaskKey::new(TaskFamily::Task, "never-existed"));
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &fixtures::scope(),
            &AgentState::new("task-agent"),
            &capture,
            &[],
            request.record_limit,
            request.byte_limit,
            &budget,
            None,
        )
    })?;
    let prepared = commit_without_history(&db, &capture, candidate, &budget)?;
    capture
        .required_keys
        .remove(&TaskKey::new(TaskFamily::Task, "never-existed"));
    assert!(prepared.resident.capture(&capture)?.lookup(&key)?.is_some());
    drop(prepared);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn receipt_reader_candidate_rejects_fixed_capture_marker_drift() -> Result<()> {
    let (_home, db, budget) = legacy_reader_fixture(false, 0)?;
    let request = fixtures::request();
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &fixtures::scope(),
            &AgentState::new("task-agent"),
            &request.capture,
            &[],
            request.record_limit,
            request.byte_limit,
            &budget,
            None,
        )
    })?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE task_domain_captures SET metadata_digest=?1",
            ["0".repeat(64)],
        )?;
        Ok(())
    })?;
    let error = commit_without_history(&db, &request.capture, candidate, &budget)
        .err()
        .context("changed fixed capture marker accepted")?;
    assert!(
        error
            .downcast_ref::<TaskPreparationCandidateStale>()
            .is_some(),
        "{error:#}"
    );
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn receipt_reader_candidate_sealed_unavailable_output_inventory_is_not_cold() -> Result<()> {
    let (_home, db, budget) = legacy_reader_fixture(true, 1)?;
    let request = fixtures::request();
    db.transaction(|tx| {
        assert!(query_sources::source_inventory_initialized_tx(
            tx,
            &fixtures::scope()
        )?);
        assert!(!query_sources::source_inventory_complete_tx(
            tx,
            &fixtures::scope()
        )?);
        assert!(!task_preparation_initialization_required_tx(
            tx,
            &fixtures::scope()
        )?);
        Ok(())
    })?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &fixtures::scope(),
            &AgentState::new("task-agent"),
            &request.capture,
            &[],
            request.record_limit,
            request.byte_limit,
            &budget,
            None,
        )
    })?;
    let prepared = commit_without_history(&db, &request.capture, candidate, &budget)?;
    assert!(
        prepared
            .resident
            .capture(&request.capture)?
            .output_source("legacy-000")
            .is_err(),
        "unavailable historical Output must not become an observed empty result"
    );
    drop(prepared);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn receipt_reader_candidate_pending_stop_frontier_uses_partial_index() -> Result<()> {
    let (_home, db) = fixtures::fresh()?;
    db.transaction(|tx| {
        for upgrade in [false, true] {
            if upgrade {
                // Exercise the exact DDL used by migration 112 on the pre-112
                // index shape, as well as normal fresh database initialization.
                tx.execute_batch("DROP INDEX idx_task_stop_pending_frontier")?;
                tx.execute_batch(holon_storage::schema::TASK_STOP_PENDING_FRONTIER_SCHEMA)?;
            }
            for sql in [
                format!("EXPLAIN QUERY PLAN SELECT COUNT(*) {PENDING_FRONTIER}"),
                format!(
                    "EXPLAIN QUERY PLAN SELECT effect_seq {PENDING_FRONTIER} ORDER BY effect_seq"
                ),
            ] {
                let mut query = tx.prepare(&sql)?;
                let plan = query
                    .query_map(params![fixtures::scope().agent_id, 1], |row| {
                        row.get::<_, String>(3)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert!(
                    plan.iter()
                        .any(|step| step.contains("idx_task_stop_pending_frontier")),
                    "pending Stop frontier must not scan completed history: {plan:?}"
                );
                assert!(
                    !plan.iter().any(|step| step.contains("TEMP B-TREE")),
                    "{plan:?}"
                );
            }
        }
        Ok(())
    })
}

fn insert_stop_frontier_row(
    tx: &Transaction<'_>,
    nonce: &str,
    agent: &str,
    incarnation: i64,
    states: [&str; 4],
) -> Result<i64> {
    tx.execute(
        "INSERT INTO task_stop_effects
         (command_nonce,agent_id,identity_incarnation,task_id,task_created_at,
          generation_digest,intent_digest,prepared_json,intent_json,ack_task_json,
          ack_task_digest,revision,physical_state,child_state,worktree_state,result_state,
          progress_json,created_at,updated_at)
         VALUES(?1,?2,?3,'frontier-task','2026-11-16T00:00:00Z',
          ?8,?8,'{}','{}','{}',?8,1,?4,?5,?6,?7,'{}',
          '2026-11-16T00:00:00Z','2026-11-16T00:00:00Z')",
        params![
            nonce,
            agent,
            incarnation,
            states[0],
            states[1],
            states[2],
            states[3],
            "0".repeat(64)
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

#[test]
fn receipt_reader_candidate_pending_stop_empty_frontier_vm_steps_ignore_history() -> Result<()> {
    let (_home, db) = fixtures::fresh()?;
    db.transaction(|tx| {
        // These metadata-only rows deliberately omit their deferred receipts.
        // Roll the fixture back before the outer transaction can commit.
        tx.execute_batch("SAVEPOINT stop_frontier_history")?;
        let scope = fixtures::scope();
        let complete = ["monitor_joined", "not_required", "retained", "completed"];
        let pending = ["pending", "pending", "pending", "pending"];
        let mut inserted = 0;
        let mut costs = Vec::new();
        for size in [100, 10_000] {
            for index in inserted..size {
                insert_stop_frontier_row(
                    tx,
                    &format!("complete-{index}"),
                    &scope.agent_id,
                    1,
                    complete,
                )?;
                insert_stop_frontier_row(
                    tx,
                    &format!("other-owner-{index}"),
                    "other-agent",
                    1,
                    pending,
                )?;
                insert_stop_frontier_row(
                    tx,
                    &format!("other-incarnation-{index}"),
                    &scope.agent_id,
                    2,
                    pending,
                )?;
            }
            inserted = size;
            let mut steps = Vec::new();
            for (sql, expected) in [
                (format!("SELECT COUNT(*) {PENDING_FRONTIER}"), vec![0_i64]),
                (
                    format!("SELECT effect_seq {PENDING_FRONTIER} ORDER BY effect_seq"),
                    vec![],
                ),
            ] {
                let mut query = tx.prepare(&sql)?;
                let rows = query
                    .query_map(params![scope.agent_id, 1], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(rows, expected);
                steps.push(query.get_status(StatementStatus::VmStep));
            }
            costs.push(steps);
        }
        assert_eq!(
            costs[0], costs[1],
            "empty pending Stop frontier revisited unrelated history"
        );
        assert!(costs[0].iter().all(|steps| *steps < 40), "{costs:?}");
        tx.execute_batch("ROLLBACK TO stop_frontier_history; RELEASE stop_frontier_history")?;
        Ok(())
    })
}

#[test]
fn receipt_reader_candidate_pending_stop_frontier_preserves_each_pending_state() -> Result<()> {
    let (_home, db) = fixtures::fresh()?;
    db.transaction(|tx| {
        tx.execute_batch("SAVEPOINT stop_frontier_states")?;
        let scope = fixtures::scope();
        let complete = ["monitor_joined", "not_required", "retained", "completed"];
        let mut expected = Vec::new();
        for field in 0..4 {
            let mut states = complete;
            states[field] = "pending";
            expected.push(insert_stop_frontier_row(
                tx,
                &format!("pending-{field}"),
                &scope.agent_id,
                1,
                states,
            )?);
        }
        let pending = ["pending"; 4];
        expected.push(insert_stop_frontier_row(
            tx,
            "pending-all",
            &scope.agent_id,
            1,
            pending,
        )?);
        insert_stop_frontier_row(tx, "complete", &scope.agent_id, 1, complete)?;
        insert_stop_frontier_row(
            tx,
            "attention",
            &scope.agent_id,
            1,
            [
                "needs_attention",
                "needs_attention",
                "failed",
                "needs_attention",
            ],
        )?;
        insert_stop_frontier_row(tx, "other-owner", "other-agent", 1, pending)?;
        insert_stop_frontier_row(tx, "other-incarnation", &scope.agent_id, 2, pending)?;
        let count: usize = tx.query_row(
            &format!("SELECT COUNT(*) {PENDING_FRONTIER}"),
            params![scope.agent_id, 1],
            |row| row.get(0),
        )?;
        let actual = tx
            .prepare(&format!(
                "SELECT effect_seq {PENDING_FRONTIER} ORDER BY effect_seq"
            ))?
            .query_map(params![scope.agent_id, 1], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(count, expected.len());
        assert_eq!(actual, expected);
        tx.execute_batch("ROLLBACK TO stop_frontier_states; RELEASE stop_frontier_states")?;
        Ok(())
    })
}
