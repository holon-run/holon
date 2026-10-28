use super::*;

fn predict(
    tx: &Transaction<'_>,
    message: &MessageEnvelope,
    queue: &QueueEntryRecord,
    budget: &InputEvidenceBudgetLedger,
) -> Result<(
    Option<WaitConditionRecord>,
    Option<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
)> {
    predicted_trigger_wait_tx(
        tx,
        &scope(),
        message,
        queue,
        &crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
            capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
            record_limit: 4096,
            byte_limit: 16 * 1024 * 1024,
        },
        budget,
        None,
    )
}
fn body_costs(source: &WaitConditionRecord, trigger: &str) -> Result<[(u64, usize); 2]> {
    // Reuse only the test AST formula, not production gates or sampled usage.
    Ok([cost(source, false)?, cost(&trigger, false)?])
}

#[test]
fn exact_cost_keeps_original_material_until_last_holder() -> Result<()> {
    let source = record();
    let (_home, db) = fixture(&source)?;
    let (mut message, mut queue) = message();
    message.id = "prediction {,:} \"\\ 路".into();
    queue.message_id = message.id.clone();
    let budget = ledger()?;
    let material = ResourceReadMaterial::new(&budget, &scope())?;
    let exact = body_costs(&source, &message.id)?
        .into_iter()
        .fold(add(add(header(), header()), pool_cost(&source)?), add);
    let (predicted, projection) = {
        let _entry = material.enter();
        db.transaction(|tx| predict(tx, &message, &queue, &budget))?
    };
    // The separate Work projection bound is not this allocation phase.
    assert!(projection.is_some());
    drop(projection);
    let mut expected = source.clone();
    expected.mark_triggered(&message.id, queue.updated_at);
    assert_eq!(predicted, Some(expected));
    assert_unwritten(&db, &source)?;
    assert_eq!(material.finish()?, exact);
    let held = material.guard()?;
    let last = held.clone();
    drop(material);
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop((predicted, held));
    assert_eq!(budget.report()?.observed_bytes, exact.0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn each_body_gate_refuses_and_releases_its_projection() -> Result<()> {
    let source = record();
    let (_home, db) = fixture(&source)?;
    let (message, queue) = message();
    let mut prefix = add(add(header(), header()), pool_cost(&source)?);
    for next in body_costs(&source, &message.id)? {
        for bytes_bound in [true, false] {
            let budget = ledger()?;
            let material = phase(&budget, add(prefix, next), bytes_bound)?;
            {
                let _entry = material.enter();
                exhausted(
                    &db.transaction(|tx| predict(tx, &message, &queue, &budget))
                        .unwrap_err(),
                );
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                exhausted(
                    &db.transaction(|tx| predict(tx, &message, &queue, &budget))
                        .unwrap_err(),
                );
                assert_eq!(budget.report()?.observed_bytes, prefix.0);
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            assert!(material.finish().is_err());
            drop(material);
            assert_eq!(budget.report()?.observed_bytes, 0);
            assert_unwritten(&db, &source)?;
        }
        prefix = add(prefix, next);
    }
    Ok(())
}

#[test]
fn original_remainder_and_outer_tls_are_preserved() -> Result<()> {
    use std::sync::Arc;
    let source = record();
    let (_home, db) = fixture(&source)?;
    let (message, queue) = message();
    let gates = body_costs(&source, &message.id)?;
    // Each original prediction also registers its independently retained projection.
    let one = gates
        .into_iter()
        .fold(add(header(), pool_cost(&source)?), add);
    let exact = add(add(header(), one), one);
    for bytes_bound in [true, false] {
        let budget = ledger()?;
        let material = phase(&budget, exact, bytes_bound)?;
        let outside_budget = ledger()?;
        let outside = ResourceReadMaterial::new(&outside_budget, &scope())?;
        {
            let _outer = outside.enter();
            let outer = ResourceReadMaterial::active_guard()?.unwrap();
            {
                let _entry = material.enter();
                let original = ResourceReadMaterial::active_guard()?.unwrap();
                let (first, projection) =
                    db.transaction(|tx| predict(tx, &message, &queue, &budget))?;
                drop(projection);
                assert_eq!(
                    first.as_ref().map(|wait| wait.id.as_str()),
                    Some(source.id.as_str())
                );
                assert_eq!(budget.report()?.observed_bytes, add(header(), one).0);
                assert!(Arc::ptr_eq(
                    &original,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
                exhausted(
                    &db.transaction(|tx| predict(tx, &message, &queue, &budget))
                        .unwrap_err(),
                );
                assert_eq!(budget.report()?.observed_bytes, exact.0 - gates[1].0);
                exhausted(
                    &db.transaction(|tx| predict(tx, &message, &queue, &budget))
                        .unwrap_err(),
                );
                assert_eq!(budget.report()?.observed_bytes, exact.0 - gates[1].0);
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert!(Arc::ptr_eq(
                &outer,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
        }
        assert!(material.finish().is_err());
        drop(material);
        assert_eq!(budget.report()?.observed_bytes, 0);
        assert_eq!(outside.finish()?, header());
        assert_unwritten(&db, &source)?;
    }
    Ok(())
}

#[test]
fn shortcuts_do_not_charge_or_publish_a_body() -> Result<()> {
    for branch in ["no-match", "existing", "triggered", "generation", "claimed"] {
        let mut source = record();
        let (mut message, queue) = message();
        match branch {
            "no-match" => {
                message.origin = MessageOrigin::Timer {
                    timer_id: "other".into(),
                };
            }
            "triggered" => source.mark_triggered(&message.id, at()),
            "generation" => {
                message
                    .source_refs
                    .insert("wait_generation".into(), "invalid".into());
            }
            _ => {}
        }
        let (_home, db) = fixture(&source)?;
        db.transaction(|tx| {
            if branch == "existing" {
                crate::runtime_db::evidence::append_message_tx(tx, &message)?;
            } else if branch == "claimed" {
                let mut claimed = source.clone();
                claimed.id = "previous-claim".into();
                claimed.mark_triggered(&message.id, at());
                claimed.status = WaitConditionStatus::Cancelled;
                claimed.cancelled_at = Some(at());
                crate::runtime_db::repositories::upsert_wait_condition_tx(tx, &claimed)?;
            }
            Ok(())
        })?;
        let exact = if branch == "existing" {
            add(header(), header())
        } else {
            add(add(header(), header()), pool_cost(&source)?)
        };
        let budget = ledger()?;
        let material = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
        let (predicted, projection) = {
            let _entry = material.enter();
            db.transaction(|tx| predict(tx, &message, &queue, &budget))?
        };
        assert!(predicted.is_none(), "{branch}");
        drop(projection);
        assert_eq!(material.finish()?, exact, "{branch}");
        if branch != "existing" {
            assert_unwritten(&db, &source)?;
        } else {
            db.transaction(|tx| {
                let payload: String = tx.query_row(
                    "SELECT payload_json FROM wait_conditions WHERE wait_condition_id=?1",
                    [&source.id],
                    |row| row.get(0),
                )?;
                assert_eq!(
                    serde_json::from_str::<WaitConditionRecord>(&payload)?,
                    source
                );
                Ok(())
            })?;
        }
        drop(material);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
