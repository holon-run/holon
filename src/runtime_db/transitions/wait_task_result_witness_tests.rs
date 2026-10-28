use super::*;
use crate::runtime_db::work_snapshot::{prepare_work_domain_snapshot_tx, WorkDomainLoadRequest};

fn write_waits(f: &Fixture, nonce: &str, waits: Vec<WaitConditionRecord>) -> Result<()> {
    let mut command = original_wait_command(f, None)?;
    command.wait_conditions = waits;
    let prepared = PreparedInputTransition::wait(
        f.scope.clone(),
        nonce.into(),
        f.at,
        command,
        Default::default(),
        None,
    )?;
    drop(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&prepared)?,
    );
    Ok(())
}

fn capture(
    f: &Fixture,
    live_waits: Vec<WaitConditionRecord>,
    exact_waits: Vec<WaitConditionRecord>,
) -> Result<WaitTaskResultWitness> {
    f.db.transaction(|tx| {
        let snapshot = prepare_work_domain_snapshot_tx(
            tx,
            &f.scope,
            &WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 4096,
                byte_limit: 4 * 1024 * 1024,
            },
        )?
        .require_current()?;
        let witness = WaitTaskResultWitness {
            scope: f.scope.clone(),
            work_revision: snapshot.resident.coverage().revision,
            live_waits: live_waits.clone(),
            exact_waits: exact_waits.clone(),
        };
        witness.validate_tx(tx, &f.scope.agent_id)?;
        Ok(witness)
    })
}

#[test]
fn bounded_wait_witness_ignores_unrelated_history_and_checks_exact_body() -> Result<()> {
    let f = Fixture::new()?;
    let live = namespace_wait(&f.scope.agent_id, f.at);
    let mut predecessor = live.clone();
    predecessor.id = "wait:predecessor".into();
    predecessor.status = WaitConditionStatus::Resolved;
    predecessor.resolved_at = Some(f.at);
    let mut history = predecessor.clone();
    history.id = "wait:unrelated-history".into();
    history.waiting_for = "unused historical body".repeat(4096);
    write_waits(
        &f,
        "witness:seed",
        vec![live.clone(), predecessor.clone(), history.clone()],
    )?;
    let witness = capture(&f, vec![live], vec![predecessor.clone()])?;
    f.db.transaction(|tx| {
        // This body is outside both the live frontier and the exact dependency.
        tx.execute(
            "UPDATE wait_conditions SET payload_json='not-json'
             WHERE wait_condition_id=?1",
            [&history.id],
        )?;
        assert_eq!(witness.validate_tx(tx, &f.scope.agent_id)?.len(), 2);
        let mut changed = predecessor.clone();
        changed.waiting_for = "changed exact dependency".into();
        tx.execute(
            "UPDATE wait_conditions SET waiting_for=?2,payload_json=?3
             WHERE wait_condition_id=?1",
            params![
                changed.id,
                changed.waiting_for,
                serde_json::to_string(&changed)?
            ],
        )?;
        assert!(witness.validate_tx(tx, &f.scope.agent_id).is_err());
        Ok(())
    })
}

#[test]
fn bounded_wait_witness_rejects_new_frontier_revision_gaps_and_replaced_rows() -> Result<()> {
    let f = Fixture::new()?;
    let live = namespace_wait(&f.scope.agent_id, f.at);
    write_waits(&f, "witness:seed", vec![live.clone()])?;
    let witness = capture(&f, vec![live.clone()], vec![])?;
    let mut added = live.clone();
    added.id = "wait:new-frontier".into();
    added.work_item_id = Some("work:other-waiter".into());
    write_waits(&f, "witness:add", vec![added.clone()])?;
    f.db.transaction(|tx| {
        assert!(witness.validate_tx(tx, &f.scope.agent_id).is_err());
        Ok(())
    })?;
    let current = capture(&f, vec![live.clone(), added], vec![])?;
    f.db.transaction(|tx| {
        let mut incomplete = current.clone();
        incomplete.live_waits.pop();
        assert!(incomplete.validate_tx(tx, &f.scope.agent_id).is_err());
        let mut duplicate = current.clone();
        duplicate.live_waits[1] = live.clone();
        assert!(duplicate.validate_tx(tx, &f.scope.agent_id).is_err());
        let mut retired = current.clone();
        retired.scope.identity_incarnation += 1;
        assert!(retired.validate_tx(tx, &f.scope.agent_id).is_err());
        let mut wrong_scope = current.clone();
        wrong_scope.scope.agent_id = "another-agent".into();
        assert!(wrong_scope.validate_tx(tx, &f.scope.agent_id).is_err());
        let mut stale = current.clone();
        stale.work_revision += 1;
        assert!(stale.validate_tx(tx, &f.scope.agent_id).is_err());
        tx.execute(
            "UPDATE wait_conditions SET status='triggered' WHERE wait_condition_id=?1",
            [&live.id],
        )?;
        assert!(current.validate_tx(tx, &f.scope.agent_id).is_err());
        tx.execute(
            "UPDATE wait_conditions SET status='active' WHERE wait_condition_id=?1",
            [&live.id],
        )?;
        let mut changed = live.clone();
        changed.waiting_for = "replaced live body".into();
        tx.execute(
            "UPDATE wait_conditions SET waiting_for=?2,payload_json=?3
             WHERE wait_condition_id=?1",
            params![
                changed.id,
                changed.waiting_for,
                serde_json::to_string(&changed)?
            ],
        )?;
        assert!(current.validate_tx(tx, &f.scope.agent_id).is_err());
        Ok(())
    })
}

#[test]
fn resolved_exact_result_claim_survives_history_window_without_body_decode() -> Result<()> {
    let f = Fixture::new()?;
    let expected: crate::runtime_db::TaskResultSettlementRecord =
        serde_json::from_value(serde_json::json!({
            "result_identity":"result:claimed", "agent_id":f.scope.agent_id,
            "task_id":"task:claimed", "message_id":"message:claimed",
            "rejoin_generation":1, "parent_turn_id":"turn:parent",
            "task_status":"completed", "state":"persisted_pending",
            "created_at":f.at, "updated_at":f.at,
        }))?;
    let mut resolved = namespace_wait(&f.scope.agent_id, f.at);
    resolved.id = "wait:resolved-claim".into();
    resolved.status = WaitConditionStatus::Resolved;
    resolved.kind = crate::types::WaitConditionKind::Task;
    resolved.source = Some("WaitFor".into());
    resolved.subject_ref = Some(expected.task_id.clone());
    resolved.wake_sources = vec![crate::types::WakeSource::TaskResult {
        task_id: expected.task_id.clone(),
    }];
    resolved.trigger_message_id = Some(expected.message_id.clone());
    resolved.resolved_at = Some(f.at);
    write_waits(&f, "witness:resolved-claim", vec![resolved.clone()])?;
    let mut guard = WaitTaskResultObligation {
        source_message_id: expected.message_id.clone(),
        expected,
        current_activation_id: None,
        waiter_work_item_id: Some("work:other-waiter".into()),
        canonical_recovery: false,
        source_queue: None,
        wait_witness: capture(&f, vec![], vec![])?,
    };
    f.db.transaction(|tx| {
        // Qualified historical rows need not have a materialized relation edge.
        tx.execute(
            "DELETE FROM work_domain_wait_edges WHERE wait_id=?1",
            [&resolved.id],
        )?;
        tx.execute(
            "UPDATE wait_conditions SET payload_json='not-json'
             WHERE wait_condition_id=?1",
            [&resolved.id],
        )?;
        assert!(wait_task_result::has_resolved_claim_tx(tx, &guard)?);
        tx.execute(
            "UPDATE work_domain_records SET availability='pruned'
             WHERE agent_id=?1 AND identity_incarnation=?2
               AND family='wait' AND record_id=?3",
            params![f.scope.agent_id, f.scope.identity_incarnation, resolved.id],
        )?;
        assert!(!wait_task_result::has_resolved_claim_tx(tx, &guard)?);
        tx.execute(
            "UPDATE work_domain_records SET availability='present'
             WHERE agent_id=?1 AND identity_incarnation=?2
               AND family='wait' AND record_id=?3",
            params![f.scope.agent_id, f.scope.identity_incarnation, resolved.id],
        )?;
        guard.waiter_work_item_id = None;
        assert!(!wait_task_result::has_resolved_claim_tx(tx, &guard)?);
        Ok(())
    })
}
