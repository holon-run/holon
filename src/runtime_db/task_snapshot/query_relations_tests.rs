use super::tests::*;
use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;
use crate::types::TaskStatus;

fn query(after: Option<u64>, replies: &[&str]) -> TaskDomainLoadRequest {
    let mut request = request();
    request.record_limit = MAX_RECORDS;
    request.byte_limit = 8 * 1024 * 1024;
    request.query.birth_after = after;
    request
        .query
        .reply_message_ids
        .extend(replies.iter().map(|id| id.to_string()));
    request
}
fn actual_tasks(
    tx: &Transaction<'_>,
    values: &[crate::types::TaskRecord],
    load: &TaskDomainLoadRequest,
) -> Result<CommittedTaskFacts> {
    let keys = values
        .iter()
        .map(|task| TaskKey::new(TaskFamily::Task, &task.id))
        .collect();
    let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
    for task in values {
        repositories::upsert_task_tx(tx, task)?;
    }
    let state = ContextDurableRead::new(tx, &scope().agent_id)
        .scope_facts()?
        .state
        .context("actual State missing")?;
    register_task_writes_tx(tx, &proof, &keys, &state, load)
}

#[test]
fn native_task_births_are_complete_beyond_terminal_window_and_updates_keep_the_cursor() -> Result<()>
{
    let (_home, db) = fresh()?;
    let values = (0..130)
        .map(|i| task(&format!("born-{i:03}"), TaskStatus::Completed))
        .collect::<Vec<_>>();
    let load = query(Some(0), &[]);
    db.transaction(|tx| {
        let facts = actual_tasks(tx, &values, &load)?;
        assert_eq!(facts.delta.revision, facts.delta.base_revision + 1);
        assert_eq!(facts.delta.coverage.query.birth_cursor, Some(130));
        assert_eq!(facts.delta.coverage.query.born_task_seqs.len(), 130);
        Ok(())
    })?;
    let captured = db.transaction(|tx| {
        let domain = prepare_task_domain_snapshot_tx(tx, &scope(), &load)?
            .require_current()?
            .resident;
        assert_eq!(
            domain
                .capture(&TaskCaptureRequest::current())?
                .records
                .len(),
            128
        );
        domain.capture_query(&load.query)
    })?;
    db.transaction(|tx| {
        tx.execute_batch("ALTER TABLE tasks RENAME TO tasks_birth_capture_forbidden")?;
        Ok(())
    })?;
    assert_eq!(captured.task_birth_cursor()?, 130);
    assert_eq!(captured.born_task_ids_after(0)?.count(), 130);
    assert_eq!(
        captured.born_task_ids_after(129)?.collect::<Vec<_>>(),
        vec!["born-129"]
    );
    Ok(())
}

#[test]
fn existing_task_update_and_failed_original_tx_do_not_allocate_another_birth() -> Result<()> {
    let (_home, db) = fresh()?;
    let original = task("one-native-id", TaskStatus::Queued);
    db.transaction(|tx| {
        actual_tasks(tx, std::slice::from_ref(&original), &query(Some(0), &[]))?;
        Ok(())
    })?;
    let mut updated = original.clone();
    updated.status = TaskStatus::Running;
    updated.updated_at += chrono::Duration::seconds(1);
    db.transaction(|tx| {
        actual_tasks(tx,std::slice::from_ref(&updated),&query(Some(0),&[]))?;
        assert_eq!(tx.query_row("SELECT high_water FROM task_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=1",[&scope().agent_id],|row|row.get::<_,u64>(0))?,1);
        Ok(())
    })?;
    let error = db
        .transaction::<()>(|tx| {
            actual_tasks(
                tx,
                &[task("rolled-back-native-id", TaskStatus::Queued)],
                &query(Some(0), &[]),
            )?;
            anyhow::bail!("original receipt fault")
        })
        .unwrap_err();
    assert!(error.to_string().contains("receipt fault"));
    db.transaction(|tx| {
        let domain = prepare_task_domain_snapshot_tx(tx, &scope(), &query(Some(0), &[]))?
            .require_current()?
            .resident;
        let capture = domain.capture_query(&query(Some(0), &[]).query)?;
        assert_eq!(capture.task_birth_cursor()?, 1);
        assert_eq!(
            capture.born_task_ids_after(0)?.collect::<Vec<_>>(),
            vec!["one-native-id"]
        );
        Ok(())
    })
}

#[test]
fn fixed_and_first_json_import_task_ids_have_birth_zero_without_late_adoption() -> Result<()> {
    let (_home, db) = legacy(false, |tx| {
        repositories::upsert_task_tx(tx, &task("original-fixed", TaskStatus::Completed))?;
        Ok(())
    })?;
    db.tasks()
        .import_legacy(vec![task("first-json-member", TaskStatus::Completed)])?;
    db.context_episodes().import_legacy(Vec::new())?;
    db.transaction(|tx| {
        repositories::upsert_task_tx(tx,&task("raw-late-id",TaskStatus::Completed))?;
        let domain=prepare_task_domain_snapshot_tx(tx,&scope(),&query(Some(0),&[]))?.require_current()?.resident;
        let capture=domain.capture_query(&query(Some(0),&[]).query)?;
        assert_eq!(capture.task_birth_cursor()?,0);
        assert_eq!(capture.born_task_ids_after(0)?.count(),0);
        let rows:Vec<(String,u64)>=tx.prepare("SELECT task_id,birth_seq FROM task_query_task_births WHERE agent_id=?1 ORDER BY task_id")?.query_map([&scope().agent_id],|row|Ok((row.get(0)?,row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        assert_eq!(rows,vec![("first-json-member".into(),0),("original-fixed".into(),0)]);
        Ok(())
    })
}

#[test]
fn completed_reply_relation_is_complete_beyond_128_and_observes_empty_and_pruned() -> Result<()> {
    let (_home, db) = fresh()?;
    let values = (0..130)
        .map(|i| {
            let mut task = task(&format!("reply-task-{i:03}"), TaskStatus::Completed);
            task.kind = crate::types::TaskKind::AgentMessageWait;
            task.detail = Some(serde_json::json!({"message_id":"original-reply"}));
            task
        })
        .collect::<Vec<_>>();
    let load = query(None, &["original-reply", "observed-empty-reply"]);
    db.transaction(|tx| {
        actual_tasks(tx, &values, &load)?;
        Ok(())
    })?;
    let original = db.transaction(|tx| {
        let domain = prepare_task_domain_snapshot_tx(tx, &scope(), &load)?
            .require_current()?
            .resident;
        let capture = domain.capture_query(&load.query)?;
        assert_eq!(capture.reply_task_ids("original-reply")?.len(), 130);
        assert!(capture.reply_task_ids("observed-empty-reply")?.is_empty());
        assert!(capture.reply_task_ids("unobserved-reply").is_err());
        Ok(capture)
    })?;
    db.transaction(|tx| {
        let keys=BTreeSet::from([TaskKey::new(TaskFamily::Task,"reply-task-000")]);
        let proof=prepare_task_write_tx(tx,&scope(),&keys)?;
        tx.execute("DELETE FROM tasks WHERE task_id='reply-task-000'",[])?;
        let state=ContextDurableRead::new(tx,&scope().agent_id).scope_facts()?.state.unwrap();
        register_task_writes_tx(tx,&proof,&keys,&state,&load)?;
        let current=prepare_task_domain_snapshot_tx(tx,&scope(),&load)?.require_current()?.resident.capture_query(&load.query)?;
        assert!(matches!(current.reply_task_ids("original-reply").unwrap_err().downcast_ref::<TaskReadError>(),Some(TaskReadError::UnavailableQuery(key)) if key.id=="reply-task-000"));
        assert_eq!(original.reply_task_ids("original-reply")?.len(),130);
        Ok(())
    })
}

#[test]
fn query_relation_scalar_budget_precedes_member_id_or_task_body_decode() -> Result<()> {
    let (_home, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let big = "x".repeat(64 * 1024);
    db.transaction(|tx| {
        tx.execute("INSERT INTO task_query_task_members(agent_id,identity_incarnation,task_id,content_digest,projection_digest,reply_message_id,created_at) VALUES(?1,1,?2,?3,?3,'budget-reply','bad-date')",params![scope().agent_id,big,"a".repeat(64)])?;
        let mut load=query(None,&["budget-reply"]);load.byte_limit=4096;
        let error=prepare_task_domain_snapshot_with_budget_tx(tx,&scope(),&load,&ledger).unwrap_err();
        assert!(matches!(error.downcast_ref::<TaskSnapshotError>(),Some(TaskSnapshotError::ResourceExhausted(_))));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn query_capture_holds_only_selected_arcs_and_releases_weak_budget_at_last_drop() -> Result<()> {
    let (_home, db) = fresh()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let load = query(Some(0), &[]);
    db.transaction(|tx| {
        actual_tasks(tx, &[task("weak-born", TaskStatus::Queued)], &load)?;
        Ok(())
    })?;
    let domain = db.transaction(|tx| {
        Ok(
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &load, &ledger)?
                .require_current()?
                .resident,
        )
    })?;
    let weak = Arc::downgrade(&domain.capture_query(&load.query)?.records[0]);
    let capture = domain.capture_query(&load.query)?;
    drop(domain);
    assert!(weak.upgrade().is_some());
    assert_eq!(
        capture.born_task_ids_after(0)?.collect::<Vec<_>>(),
        vec!["weak-born"]
    );
    assert!(ledger.report()?.observed_bytes > 0);
    drop(capture);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn terminal_live_birth_and_reply_relations_are_current_under_real_tiny_history_ledger() -> Result<()>
{
    use crate::runtime::input_evidence_budget::InputMemoryCategory;
    for reply in [false, true] {
        let (_home, db) = fresh()?;
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits {
            history_per_agent_bytes: 1024,
            history_host_bytes: 1024,
            ..Default::default()
        })?;
        let values = (0..130)
            .map(|i| {
                let mut record = task(&format!("current-terminal-{i:03}"), TaskStatus::Completed);
                if reply {
                    record.kind = crate::types::TaskKind::AgentMessageWait;
                    record.detail = Some(serde_json::json!({"message_id":"live-reply"}));
                }
                record
            })
            .collect::<Vec<_>>();
        let load = if reply {
            query(None, &["live-reply"])
        } else {
            query(Some(0), &[])
        };
        db.transaction(|tx| {
            actual_tasks(tx, &values, &load)?;
            Ok(())
        })?;
        let domain = db.transaction(|tx| {
            Ok(
                prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &load, &ledger)?
                    .require_current()?
                    .resident,
            )
        })?;
        let report = ledger.report()?;
        assert_eq!(
            report
                .categories
                .get(&InputMemoryCategory::History)
                .map_or(0, |usage| usage.bytes),
            0
        );
        assert!(
            report
                .categories
                .get(&InputMemoryCategory::Current)
                .unwrap()
                .bytes
                > 1024
        );
        let capture = domain.capture_query(&load.query)?;
        assert_eq!(capture.records.len(), 130);
        drop(domain);
        if reply {
            assert_eq!(capture.reply_task_ids("live-reply")?.len(), 130);
        } else {
            assert_eq!(capture.born_task_ids_after(0)?.count(), 130);
        }
        drop(capture);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn next_actual_ack_without_query_role_demotes_terminal_bodies_to_history_and_backpressures(
) -> Result<()> {
    use crate::runtime::input_evidence_budget::{InputEvidenceBudgetError, InputMemoryCategory};
    let (_home, db) = fresh()?;
    let tiny = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits {
        history_per_agent_bytes: 1024,
        history_host_bytes: 1024,
        ..Default::default()
    })?;
    let live = query(Some(0), &[]);
    let values = (0..130)
        .map(|i| task(&format!("released-terminal-{i:03}"), TaskStatus::Completed))
        .collect::<Vec<_>>();
    db.transaction(|tx| {
        actual_tasks(tx, &values, &live)?;
        Ok(())
    })?;
    let original = db.transaction(|tx| {
        Ok(
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &live, &tiny)?
                .require_current()?
                .resident,
        )
    })?;
    let next = task("next-after-role-release", TaskStatus::Queued);
    let ordinary = query(None, &[]);
    let failed = db
        .transaction(|tx| {
            let keys = BTreeSet::from([TaskKey::new(TaskFamily::Task, &next.id)]);
            let proof = prepare_task_write_tx(tx, &scope(), &keys)?;
            repositories::upsert_task_tx(tx, &next)?;
            let state = ContextDurableRead::new(tx, &scope().agent_id)
                .scope_facts()?
                .state
                .unwrap();
            register_task_writes_with_budget_tx(tx, &proof, &keys, &state, &ordinary, &tiny)
        })
        .unwrap_err();
    assert!(matches!(
        failed.downcast_ref::<InputEvidenceBudgetError>(),
        Some(InputEvidenceBudgetError::ResourceExhausted { .. })
    ));
    assert_eq!(original.coverage().query.birth_cursor, Some(130));
    db.transaction(|tx| {
        assert!(!tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1)",
            [&next.id],
            |row| row.get::<_, bool>(0)
        )?);
        assert_eq!(
            loader::current_coverage(tx, &scope())?
                .unwrap()
                .query
                .birth_cursor,
            Some(130)
        );
        Ok(())
    })?;
    drop(original);
    assert_eq!(tiny.report()?.observed_bytes, 0);
    let roomy = InputEvidenceBudgetLedger::new(Default::default())?;
    let final_domain = db.transaction(|tx| {
        actual_tasks(tx, std::slice::from_ref(&next), &ordinary)?;
        Ok(
            prepare_task_domain_snapshot_with_budget_tx(tx, &scope(), &ordinary, &roomy)?
                .require_current()?
                .resident,
        )
    })?;
    assert!(final_domain.coverage().query.born_task_seqs.is_empty());
    let report = roomy.report()?;
    assert!(
        report
            .categories
            .get(&InputMemoryCategory::History)
            .unwrap()
            .bytes
            > 1024
    );
    assert_eq!(
        final_domain
            .capture(&TaskCaptureRequest::current())?
            .records
            .len(),
        129
    );
    drop(final_domain);
    assert_eq!(roomy.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn exact_initialized_query_inventory_never_rescans_fixed_members_or_revives_old_reply_versions(
) -> Result<()> {
    let mut original = task("fixed-query-once", TaskStatus::Running);
    original.kind = crate::types::TaskKind::AgentMessageWait;
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_task_tx(tx, &original)?;
        Ok(())
    })?;
    let mut current = original.clone();
    current.status = TaskStatus::Completed;
    current.updated_at += chrono::Duration::seconds(1);
    current.detail = Some(serde_json::json!({"message_id":"actual-new-reply"}));
    let load = query(None, &["actual-new-reply", "original-observed-empty"]);
    db.transaction(|tx| {
        assert!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM task_query_task_inventory WHERE agent_id=?1)",[&scope().agent_id],|row|row.get::<_,bool>(0))?);
        // Real ACK precedes first fixed initialization, and must remain stronger.
        actual_tasks(tx,std::slice::from_ref(&current),&load)?;
        let row:(Option<String>,u64)=tx.query_row("SELECT m.reply_message_id,b.birth_seq FROM task_query_task_members m JOIN task_query_task_births b USING(agent_id,identity_incarnation,task_id) WHERE m.agent_id=?1 AND m.task_id=?2",params![scope().agent_id,current.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(row,(Some("actual-new-reply".into()),0));
        tx.execute_batch("ALTER TABLE task_query_fixed_tasks RENAME TO query_fixed_member_scan_forbidden;
            CREATE VIEW task_query_fixed_tasks AS SELECT inventory_id,task_id,content_digest,projection_digest,
            reply_message_id,json_extract('fixed bulk rescan forbidden','$') AS created_at
            FROM query_fixed_member_scan_forbidden;")?;
        // Manifest COUNT remains available. Only a forbidden repeated full row
        // INSERT SELECT would evaluate the poisoned created_at projection.
        for _ in 0..2 {
            let capture=prepare_task_domain_snapshot_tx(tx,&scope(),&load)?.require_current()?.resident.capture_query(&load.query)?;
            assert_eq!(capture.reply_task_ids("actual-new-reply")?.len(),1);
            assert!(capture.reply_task_ids("original-observed-empty")?.is_empty());
        }
        let keys=BTreeSet::from([TaskKey::new(TaskFamily::Task,&current.id)]);
        let proof=prepare_task_write_tx(tx,&scope(),&keys)?;
        tx.execute("DELETE FROM tasks WHERE task_id=?1",[&current.id])?;
        let state=ContextDurableRead::new(tx,&scope().agent_id).scope_facts()?.state.unwrap();
        register_task_writes_tx(tx,&proof,&keys,&state,&load)?;
        let capture=prepare_task_domain_snapshot_tx(tx,&scope(),&load)?.require_current()?.resident.capture_query(&load.query)?;
        assert!(matches!(capture.reply_task_ids("actual-new-reply").unwrap_err().downcast_ref::<TaskReadError>(),Some(TaskReadError::UnavailableQuery(_))));
        let retained:Option<String>=tx.query_row("SELECT reply_message_id FROM task_query_task_members WHERE agent_id=?1 AND task_id=?2",params![scope().agent_id,current.id],|row|row.get(0))?;
        assert_eq!(retained.as_deref(),Some("actual-new-reply"));
        Ok(())
    })
}
