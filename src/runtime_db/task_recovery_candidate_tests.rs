use super::*;

#[test]
fn receipt_reader_candidate_cold_domain_is_a_gate_not_writer_initialization() -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    db.transaction(|tx| {
        tx.execute(
            "DELETE FROM task_domain_current WHERE agent_id=?1",
            [&scope().agent_id],
        )?;
        Ok(())
    })?;
    let error = db
        .read_for_preparation(|tx| {
            prepare_task_recovery_candidate_tx(
                tx,
                &scope(),
                &state,
                &TaskCaptureRequest::current(),
                &[],
                4096,
                4 * 1024 * 1024,
                &budget,
                None,
            )
            .map(|_| ())
        })
        .unwrap_err();
    assert!(error.to_string().contains("reader gate"), "{error:#}");
    let count: usize = db.read_for_preparation(|tx| {
        Ok(tx.query_row(
            "SELECT COUNT(*) FROM task_domain_current WHERE agent_id=?1",
            [&scope().agent_id],
            |row| row.get(0),
        )?)
    })?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn receipt_reader_candidate_closes_current_domains_without_writer_task_body_reads() -> Result<()> {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let (_dir, db, state, budget) = fixture()?;
    db.transaction(|tx| write_task(tx, &state, &task("receipt-reader", TaskStatus::Running)))?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
        )
    })?;
    db.recovery_transaction_once(|tx| {
        // A full Task loader is forbidden even for metadata preflight lengths.
        tx.authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Read { table_name: "tasks", column_name: "payload_json" } =>
                Authorization::Deny,
            _ => Authorization::Allow,
        }));
        let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            query: None,
            selected_completion_brief_ids: Default::default(),
            interrupted_sources: Default::default(),
            windows: Vec::new(),
            mandatory_roots: Default::default(),
            record_limit: 4096,
            byte_limit: 4 * 1024 * 1024,
        };
        let result = crate::runtime_db::input_preparation::prepare_original_input_recovery_with_task_candidate_tx(
            tx, &scope(), &state, &request, 16, &budget,
            &crate::runtime::resident_work::WorkCaptureRequest::current(),
            &TaskCaptureRequest::current(), None, candidate,
        );
        tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        let prepared = result?;
        let canonical = ContextDurableRead::new(tx, &scope().agent_id).scope_facts()?;
        assert_eq!(prepared.facts.identity, canonical.identity);
        assert_eq!(prepared.facts.state, canonical.state);
        assert_eq!(prepared.facts.control_revision, canonical.control_revision);
        assert!(prepared.inputs.belongs_to(&scope()));
        assert!(prepared.episodes.belongs_to(&scope().agent_id, 1));
        assert_eq!(prepared.work.as_ref().unwrap().scope(), &scope());
        let tasks = prepared.tasks.as_ref().unwrap();
        assert_eq!(tasks.scope(), &scope());
        assert!(tasks.capture(&TaskCaptureRequest::current())?
            .lookup(&TaskKey::new(TaskFamily::Task, "receipt-reader"))?.is_some());
        assert_eq!(prepared.pending.len(), state.pending);
        Ok(())
    })
}

#[test]
fn receipt_reader_candidate_rejects_message_head_drift_without_task_revision_change() -> Result<()>
{
    let (_dir, db, state, budget) = fixture()?;
    db.transaction(|tx| write_task(tx, &state, &task("receipt-head", TaskStatus::Running)))?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
        )
    })?;
    let before: (u64, String) = db.read_for_preparation(|tx| Ok(tx.query_row(
        "SELECT revision,coverage_json FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=1",
        [&scope().agent_id], |row| Ok((row.get(0)?, row.get(1)?)),
    )?))?;
    db.transaction(|tx| {
        let mut message = result_message("receipt-head", "receipt-new-output");
        message.metadata =
            Some(serde_json::json!({"task_id":"receipt-head", "task_status":"running"}));
        let (actual, _) = append_message_tx(tx, &message)?;
        // Canonical append alone is not an accepted Output source.
        crate::runtime_db::input_evidence_snapshot::register_committed_input_evidence_tx(
            tx,
            &scope(),
            &[crate::runtime_db::input_evidence_snapshot::CanonicalInputEvidence::Message(actual)],
        )?;
        Ok(())
    })?;
    let after: u64 = db.read_for_preparation(|tx| {
        Ok(tx.query_row(
            "SELECT revision FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=1",
            [&scope().agent_id],
            |row| row.get(0),
        )?)
    })?;
    assert_eq!(before.0, after);
    let error = db
        .recovery_transaction_once(|tx| {
            commit_task_recovery_candidate_tx(
                tx,
                &scope(),
                &state,
                &TaskCaptureRequest::current(),
                &[],
                4096,
                4 * 1024 * 1024,
                &budget,
                None,
                candidate,
            )
            .map(|_| ())
        })
        .unwrap_err();
    assert!(
        error.is::<task_snapshot::TaskPreparationCandidateStale>(),
        "{error:#}"
    );
    let current: String = db.read_for_preparation(|tx| Ok(tx.query_row(
        "SELECT coverage_json FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=1",
        [&scope().agent_id], |row| row.get(0),
    )?))?;
    assert_eq!(current, before.1, "stale qualification must roll back");
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
        )
    })?;
    let prepared = db.recovery_transaction_once(|tx| {
        commit_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
            candidate,
        )
    })?;
    assert_eq!(
        prepared.resident.coverage().output.heads["receipt-head"]
            .source
            .as_ref()
            .context("accepted Output head missing after reader retry")?
            .message
            .key
            .id,
        "receipt-new-output"
    );
    drop(prepared);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn receipt_reader_candidate_does_not_adopt_raw_appended_output() -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    db.transaction(|tx| write_task(tx, &state, &task("receipt-raw-head", TaskStatus::Running)))?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
        )
    })?;
    db.transaction(|tx| {
        let mut message = result_message("receipt-raw-head", "receipt-raw-output");
        message.metadata =
            Some(serde_json::json!({"task_id":"receipt-raw-head", "task_status":"running"}));
        append_message_tx(tx, &message)?;
        Ok(())
    })?;
    let prepared = db.recovery_transaction_once(|tx| {
        commit_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
            candidate,
        )
    })?;
    assert!(
        prepared.resident.coverage().output.heads["receipt-raw-head"]
            .source
            .is_none()
    );
    assert!(!prepared
        .resident
        .coverage()
        .output
        .window
        .as_ref()
        .context("Output window missing")?
        .message_ids
        .iter()
        .any(|id| id == "receipt-raw-output"));
    drop(prepared);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn receipt_reader_candidate_rejects_state_drift_and_can_reprepare_only_qualification() -> Result<()>
{
    let (_dir, db, mut state, budget) = fixture()?;
    db.transaction(|tx| write_task(tx, &state, &task("receipt-state", TaskStatus::Running)))?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
        )
    })?;
    state.status = crate::types::AgentStatus::Stopped;
    db.transaction(|tx| upsert_agent_state_tx(tx, &state))?;
    let error = db
        .recovery_transaction_once(|tx| {
            commit_task_recovery_candidate_tx(
                tx,
                &scope(),
                &state,
                &TaskCaptureRequest::current(),
                &[],
                4096,
                4 * 1024 * 1024,
                &budget,
                None,
                candidate,
            )
            .map(|_| ())
        })
        .unwrap_err();
    assert!(
        error.is::<task_snapshot::TaskPreparationCandidateStale>(),
        "{error:#}"
    );
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
        )
    })?;
    db.recovery_transaction_once(|tx| {
        commit_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
            candidate,
        )
        .map(|_| ())
    })?;
    Ok(())
}

#[test]
fn receipt_reader_candidate_closes_pending_result_without_writer_body_reload() -> Result<()> {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let (_dir, db, mut state, budget) = fixture()?;
    let task = result_task_for_joint("receipt-qualified-result", "receipt-result-source");
    let (source, result) = db.transaction(|tx| {
        write_task(tx, &state, &task)?;
        qualified_pending_result(tx, &mut state, &task)
    })?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            std::slice::from_ref(&source),
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
        )
    })?;
    let prepared = db.recovery_transaction_once(|tx| {
        tx.authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Read {
                table_name: "tasks" | "task_result_settlements",
                column_name: "payload_json",
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }));
        let prepared = commit_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            std::slice::from_ref(&source),
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
            candidate,
        );
        tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        prepared
    })?;
    assert!(matches!(
        &prepared.resident.coverage().result_dependencies[&result.result_identity].relation,
        TaskResultDependencyRelation::QualifiedCurrent { .. }
    ));
    assert!(prepared.work_roots.contains("asserted-result-work"));
    assert!(prepared.input_roots.contains(&InputEvidenceKey::new(
        InputEvidenceFamily::Turn,
        "asserted-result-parent-turn",
    )));
    drop(prepared);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn receipt_reader_candidate_closes_output_reply_parent_and_birth_selectors() -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    let mut task = task("receipt-query", TaskStatus::Completed);
    task.kind = TaskKind::AgentMessageWait;
    task.detail = Some(serde_json::json!({
        "message_id":"receipt-query-reply",
        "parent_turn_id":"receipt-query-turn",
    }));
    db.transaction(|tx| write_task(tx, &state, &task))?;
    let mut query = crate::runtime::task_queries::TaskQuerySelection::default();
    query.output_task_ids.insert(task.id.clone());
    query.reply_message_ids.insert("receipt-query-reply".into());
    query
        .required_keys
        .insert(TaskKey::new(TaskFamily::Task, &task.id));
    query.parent_turn_ids.insert("receipt-query-turn".into());
    query.birth_after = Some(0);
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            Some(&query),
        )
    })?;
    let prepared = db.recovery_transaction_once(|tx| {
        commit_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            Some(&query),
            candidate,
        )
    })?;
    let coverage = &prepared.resident.coverage().query;
    assert!(coverage.reply_task_ids["receipt-query-reply"].contains(&task.id));
    assert!(coverage.born_task_seqs.contains_key(&task.id));
    assert!(!coverage.parent_turn_tasks["receipt-query-turn"].is_empty());
    let captured = prepared.resident.capture_query(&query)?;
    assert_eq!(
        captured.reply_task_ids("receipt-query-reply")?,
        &BTreeSet::from([task.id.clone()])
    );
    assert!(captured.output_source(&task.id)?.is_none());
    Ok(())
}

#[test]
fn receipt_reader_candidate_keeps_scheduler_recovery_source_authority_separate() -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    let mut selectors = TaskCaptureRequest::current();
    selectors
        .recovery_source_ids
        .insert("scheduler-source".into());
    let error = db
        .read_for_preparation(|tx| {
            prepare_task_recovery_candidate_tx(
                tx,
                &scope(),
                &state,
                &selectors,
                &[],
                4096,
                4 * 1024 * 1024,
                &budget,
                None,
            )
            .map(|_| ())
        })
        .unwrap_err();
    assert!(
        error.to_string().contains("frozen Wait/replay"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn receipt_reader_candidate_rechecks_negative_pending_result_address() -> Result<()> {
    let (_dir, db, mut state, budget) = fixture()?;
    let task = result_task_for_joint("receipt-negative-result", "receipt-negative-source");
    let source = db.transaction(|tx| {
        write_task(tx, &state, &task)?;
        let mut source = result_message(&task.id, task.parent_message_id.as_deref().unwrap());
        source.work_item_id = task.work_item_id.clone();
        append_pending(tx, &mut state, &source)
    })?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            std::slice::from_ref(&source),
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
        )
    })?;
    // A raw late Result cannot become admitted by sharing the source ID. The
    // negative address must change even without advancing qualified Task state.
    db.transaction(|tx| {
        let result =
            TaskResultSettlementRecord::pending(&task, &source, at())?.context("pending")?;
        upsert_pending_tx(tx, &result)?;
        Ok(())
    })?;
    let error = db
        .recovery_transaction_once(|tx| {
            commit_task_recovery_candidate_tx(
                tx,
                &scope(),
                &state,
                &TaskCaptureRequest::current(),
                std::slice::from_ref(&source),
                4096,
                4 * 1024 * 1024,
                &budget,
                None,
                candidate,
            )
            .map(|_| ())
        })
        .unwrap_err();
    assert!(
        error.is::<task_snapshot::TaskPreparationCandidateStale>(),
        "{error:#}"
    );
    Ok(())
}

fn insert_recovery_stop(
    tx: &Transaction<'_>,
    state: &AgentState,
    budget: &InputEvidenceBudgetLedger,
) -> Result<crate::runtime_db::task_stop::TaskStopRecord> {
    use crate::runtime_db::{
        input_mutation_receipts::{
            canonical_command_digest, insert_input_mutation_receipt_tx, InputMutationFamily,
            InputMutationOutcome, InputMutationResult, InputMutationScopeOutcome,
            PreparedInputMutation,
        },
        task_stop::{
            insert_task_stop_with_observation_ack_tx, prepare_task_stop_tx, PreparedTaskStop,
            TaskStopSignal,
        },
    };
    let mut task = task("receipt-stop", TaskStatus::Running);
    task.detail = Some(serde_json::json!({"cancel_requested":false}));
    write_task(tx, state, &task)?;
    let request = load_request();
    let selected =
        task_snapshot::prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &request, budget)?
            .require_current()?
            .resident
            .capture(&request.capture)?;
    let key = TaskKey::new(TaskFamily::Task, &task.id);
    let keys = BTreeSet::from([key.clone()]);
    let original = prepare_task_write_tx(tx, &scope(), &keys)?;
    let intent = PreparedTaskStop {
        task: selected.lookup(&key)?.context("Task")?.locator.clone(),
        kind: task.kind.clone(),
        created_at: task.created_at,
        rejoin: None,
        owner_epoch: 1,
        signal: TaskStopSignal::Cancel,
        lease_id: Some("receipt-stop-lease".into()),
        child: None,
        child_unavailable: None,
        worktree: None,
        worktree_unavailable: None,
        result: None,
    };
    let prepared = PreparedInputMutation::new(
        InputMutationFamily::Task,
        "receipt-stop-nonce".into(),
        vec![scope()],
        canonical_command_digest(&intent)?,
        at(),
    )?;
    let proof = prepare_task_stop_tx(tx, &prepared, &intent, &original, &request, budget)?;
    task.status = TaskStatus::Cancelling;
    task.detail.as_mut().unwrap()["cancel_requested"] = serde_json::json!(true);
    repositories::upsert_task_tx(tx, &task)?;
    let (record, inserted) = insert_task_stop_with_observation_ack_tx(&proof, &task)?;
    task_snapshot::register_task_writes_with_stop_observation_tx(
        tx, &original, &keys, state, &request, budget, None, &inserted,
    )?;
    let outcome = InputMutationOutcome {
        result: InputMutationResult::Task {
            task_id: task.id,
            deletion: None,
            stop: Some(record.receipt_binding()),
        },
        applied: true,
        delivery_receipt: None,
        scopes: vec![InputMutationScopeOutcome {
            scope: scope(),
            install_keys: Default::default(),
            audit_events: vec![],
            notify_memory_index: false,
            notify_scheduler: false,
        }],
    };
    insert_input_mutation_receipt_tx(tx, &prepared, &outcome, at())?;
    Ok(record)
}

#[test]
fn receipt_reader_candidate_closes_stop_heads_without_writer_stop_json() -> Result<()> {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let (_dir, db, state, budget) = fixture()?;
    let stop = db.transaction(|tx| insert_recovery_stop(tx, &state, &budget))?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
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
                table_name: "task_stop_effects",
                column_name: "prepared_json" | "intent_json" | "progress_json" | "ack_task_json",
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }));
        let result = commit_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
            candidate,
        );
        tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        result
    })?;
    let captured = prepared.resident.capture_pending_stops()?;
    assert_eq!(
        captured
            .pending_stops()?
            .map(|stop| stop.effect_seq)
            .collect::<Vec<_>>(),
        vec![stop.effect_seq]
    );
    drop(captured);
    drop(prepared);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn receipt_reader_candidate_rechecks_stop_revision_without_task_lane_change() -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    let stop = db.transaction(|tx| insert_recovery_stop(tx, &state, &budget))?;
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &TaskCaptureRequest::current(),
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            None,
        )
    })?;
    db.transaction(|tx| {
        tx.execute(
            "UPDATE task_stop_effects SET revision=revision+1 WHERE effect_seq=?1",
            [stop.effect_seq],
        )?;
        Ok(())
    })?;
    let error = db
        .recovery_transaction_once(|tx| {
            commit_task_recovery_candidate_tx(
                tx,
                &scope(),
                &state,
                &TaskCaptureRequest::current(),
                &[],
                4096,
                4 * 1024 * 1024,
                &budget,
                None,
                candidate,
            )
            .map(|_| ())
        })
        .unwrap_err();
    assert!(
        error.is::<task_snapshot::TaskPreparationCandidateStale>(),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn receipt_reader_candidate_keeps_output_query_outside_terminal_window() -> Result<()> {
    let (_dir, db, state, budget) = fixture()?;
    let task = task("receipt-output-outside-window", TaskStatus::Completed);
    db.transaction(|tx| write_task(tx, &state, &task))?;
    let mut selectors = TaskCaptureRequest::current();
    selectors.terminal_limit = 0;
    let mut query = crate::runtime::task_queries::TaskQuerySelection::default();
    query.output_task_ids.insert(task.id.clone());
    // Match TaskQueryRegistry::register: an Output role owns its Task root,
    // independently of active and terminal frontiers.
    query
        .required_keys
        .insert(TaskKey::new(TaskFamily::Task, &task.id));
    let candidate = db.read_for_preparation(|tx| {
        prepare_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &selectors,
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            Some(&query),
        )
    })?;
    let prepared = db.recovery_transaction_once(|tx| {
        commit_task_recovery_candidate_tx(
            tx,
            &scope(),
            &state,
            &selectors,
            &[],
            4096,
            4 * 1024 * 1024,
            &budget,
            Some(&query),
            candidate,
        )
    })?;
    let capture = prepared.resident.capture_query(&query)?;
    assert!(capture
        .lookup(&TaskKey::new(TaskFamily::Task, &task.id))?
        .is_some());
    assert!(capture.output_source(&task.id)?.is_none());
    Ok(())
}
