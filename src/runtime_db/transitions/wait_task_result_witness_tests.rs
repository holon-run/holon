use super::*;
use crate::{
    runtime::resident_work::{CanonicalWorkRecord, WorkCaptureRequest, WorkFamily, WorkKey},
    runtime_db::work_snapshot::{prepare_work_domain_snapshot_tx, WorkDomainLoadRequest},
};

fn write_waits(f: &Fixture, nonce: &str, waits: Vec<WaitConditionRecord>) -> Result<()> {
    write_waits_with_work(f, nonce, waits, Vec::new())
}

fn write_waits_with_work(
    f: &Fixture,
    nonce: &str,
    waits: Vec<WaitConditionRecord>,
    work_items: Vec<WorkItemMutation>,
) -> Result<()> {
    let mut command = original_wait_command(f, None)?;
    command.wait_conditions = waits;
    command.work_items = work_items;
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
        let mut request = WorkCaptureRequest::current();
        request.mandatory_roots.extend(
            live_waits
                .iter()
                .chain(&exact_waits)
                .map(|wait| WorkKey::new(WorkFamily::Wait, &wait.id)),
        );
        let snapshot = prepare_work_domain_snapshot_tx(
            tx,
            &f.scope,
            &WorkDomainLoadRequest {
                capture: request.clone(),
                record_limit: 4096,
                byte_limit: 4 * 1024 * 1024,
            },
        )?
        .require_current()?;
        let captured = snapshot.resident.capture(&request)?;
        let canonical_waits = |waits: &[WaitConditionRecord]| -> Result<Vec<WaitConditionRecord>> {
            waits
                .iter()
                .map(|wait| {
                    let qualified = captured
                        .lookup_parts(WorkFamily::Wait, &wait.id)?
                        .context("witness Wait missing from Work snapshot")?;
                    let CanonicalWorkRecord::Wait(wait) = &qualified.record else {
                        anyhow::bail!("witness Wait key returned another Work family");
                    };
                    Ok(wait.clone())
                })
                .collect()
        };
        let witness = WaitTaskResultWitness {
            scope: f.scope.clone(),
            work_revision: snapshot.resident.coverage().revision,
            live_waits: canonical_waits(&live_waits)?,
            exact_waits: canonical_waits(&exact_waits)?,
        };
        witness.validate_tx(tx, &f.scope.agent_id)?;
        Ok(witness)
    })
}

#[test]
fn bounded_wait_witness_accepts_nanosecond_original_write_from_work_snapshot() -> Result<()> {
    let f = Fixture::new()?;
    let mut live = namespace_wait(&f.scope.agent_id, f.at);
    live.expires_at = Some(f.at + chrono::Duration::seconds(60));
    let mut predecessor = live.clone();
    predecessor.id = "wait:nanosecond-predecessor".into();
    predecessor.status = WaitConditionStatus::Resolved;
    predecessor.resolved_at = Some(f.at);
    write_waits(
        &f,
        "witness:nanosecond-original",
        vec![live.clone(), predecessor.clone()],
    )?;
    let witness = capture(&f, vec![live.clone()], vec![predecessor.clone()])?;
    f.db.transaction(|tx| {
        for (original, canonical) in [
            (&live, &witness.live_waits[0]),
            (&predecessor, &witness.exact_waits[0]),
        ] {
            let (created_at, payload): (String, String) = tx.query_row(
                "SELECT created_at,payload_json FROM wait_conditions WHERE wait_condition_id=?1",
                [&original.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(original.created_at.timestamp_subsec_nanos(), 123_456_789);
            assert_eq!(
                serde_json::from_str::<WaitConditionRecord>(&payload)?,
                *original
            );
            assert_eq!(
                canonical.created_at,
                created_at.parse::<chrono::DateTime<Utc>>()?
            );
            assert_ne!(canonical.created_at, original.created_at);
        }
        assert_eq!(witness.validate_tx(tx, &f.scope.agent_id)?.len(), 2);
        Ok(())
    })
}

#[test]
fn bounded_wait_witness_rejects_consistent_canonical_and_payload_body_change() -> Result<()> {
    let f = Fixture::new()?;
    let live = namespace_wait(&f.scope.agent_id, f.at);
    write_waits(&f, "witness:consistent-body", vec![live.clone()])?;
    let witness = capture(&f, vec![live.clone()], vec![])?;
    f.db.transaction(|tx| {
        let mut changed = live.clone();
        changed.waiting_for = "changed canonical and payload body".into();
        tx.execute(
            "UPDATE wait_conditions SET waiting_for=?2,payload_json=?3 WHERE wait_condition_id=?1",
            params![
                changed.id,
                changed.waiting_for,
                serde_json::to_string(&changed)?
            ],
        )?;
        let canonical = crate::runtime_db::work_snapshot::canonical_wait_record_tx(
            tx,
            &f.scope.agent_id,
            &changed.id,
        )?
        .context("changed canonical Wait missing")?;
        assert_eq!(canonical.waiting_for, changed.waiting_for);
        assert_ne!(canonical, witness.live_waits[0]);
        assert!(witness.validate_tx(tx, &f.scope.agent_id).is_err());
        Ok(())
    })
}

#[test]
fn bounded_wait_witness_rejects_payload_only_body_change_and_malformed_json() -> Result<()> {
    for malformed in [false, true] {
        let f = Fixture::new()?;
        let live = namespace_wait(&f.scope.agent_id, f.at);
        write_waits(&f, "witness:payload-only", vec![live.clone()])?;
        let witness = capture(&f, vec![live.clone()], vec![])?;
        f.db.transaction(|tx| {
            let mut changed = live.clone();
            changed.waiting_for = "payload-only body replacement".into();
            let payload = if malformed {
                "not-json".into()
            } else {
                serde_json::to_string(&changed)?
            };
            tx.execute(
                "UPDATE wait_conditions SET payload_json=?2 WHERE wait_condition_id=?1",
                params![live.id, payload],
            )?;
            let waiting_for: String = tx.query_row(
                "SELECT waiting_for FROM wait_conditions WHERE wait_condition_id=?1",
                [&live.id],
                |row| row.get(0),
            )?;
            assert_eq!(waiting_for, live.waiting_for);
            assert!(witness.validate_tx(tx, &f.scope.agent_id).is_err());
            Ok(())
        })?;
    }
    Ok(())
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
    let mut added_work = WorkItemRecord::new(
        &f.scope.agent_id,
        "another qualified waiter",
        WorkItemState::Open,
    );
    added_work.id = "work:other-waiter".into();
    added.work_item_id = Some(added_work.id.clone());
    write_waits_with_work(
        &f,
        "witness:add",
        vec![added.clone()],
        vec![WorkItemMutation::Insert { record: added_work }],
    )?;
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
        let (digest, fingerprint): (String, String) = tx.query_row(
            "SELECT content_digest,fingerprint_json FROM work_domain_records
             WHERE agent_id=?1 AND identity_incarnation=?2
               AND family='wait' AND record_id=?3",
            params![f.scope.agent_id, f.scope.identity_incarnation, resolved.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        tx.execute(
            "UPDATE work_domain_records SET availability='pruned',
               content_digest=NULL,fingerprint_json=NULL
             WHERE agent_id=?1 AND identity_incarnation=?2
               AND family='wait' AND record_id=?3",
            params![f.scope.agent_id, f.scope.identity_incarnation, resolved.id],
        )?;
        assert!(!wait_task_result::has_resolved_claim_tx(tx, &guard)?);
        tx.execute(
            "UPDATE work_domain_records SET availability='present',
               content_digest=?4,fingerprint_json=?5
             WHERE agent_id=?1 AND identity_incarnation=?2
               AND family='wait' AND record_id=?3",
            params![
                f.scope.agent_id,
                f.scope.identity_incarnation,
                resolved.id,
                digest,
                fingerprint
            ],
        )?;
        guard.waiter_work_item_id = None;
        assert!(!wait_task_result::has_resolved_claim_tx(tx, &guard)?);
        Ok(())
    })
}
