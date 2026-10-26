use super::*;
use crate::runtime::resident_input_evidence::{InputEvidenceFamily, InputEvidenceKey};
use std::collections::BTreeSet;

const LIMITS: (usize, usize) = (4096, 1024 * 1024);
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "query-owner".into(),
        identity_incarnation: 1,
    }
}
fn selection(id: &str, after: u64) -> InputQuerySelection {
    InputQuerySelection {
        required_keys: BTreeSet::from([InputEvidenceKey::new(InputEvidenceFamily::Message, id)]),
        birth_after: Some(after),
    }
}

#[test]
fn frozen_roles_keep_only_exact_live_owner_union_until_original_tx_finishes() -> Result<()> {
    let registry = InputQueryRegistry::default();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let first = registry.register(&scope(), 7, &selection("first", 3), &ledger, LIMITS)?;
    let second = registry.register(&scope(), 7, &selection("second", 9), &ledger, LIMITS)?;
    let old = registry.register(&scope(), 6, &selection("old", 0), &ledger, LIMITS)?;
    let successor = InputEvidenceScope {
        identity_incarnation: 2,
        ..scope()
    };
    let foreign = registry.register(&successor, 7, &selection("foreign", 0), &ledger, LIMITS)?;
    assert!(registry.validate_lease(&first, &scope(), 6).is_err());
    assert!(registry.validate_lease(&first, &successor, 7).is_err());
    assert!(InputQueryRegistry::default()
        .validate_lease(&first, &scope(), 7)
        .is_err());
    let snapshot = registry.snapshot(&scope(), 7, &ledger, LIMITS)?;
    assert_eq!(snapshot.selection.birth_after, Some(3));
    assert_eq!(
        snapshot.selection.required_keys,
        BTreeSet::from([
            InputEvidenceKey::new(InputEvidenceFamily::Message, "first"),
            InputEvidenceKey::new(InputEvidenceFamily::Message, "second"),
        ])
    );
    let other_ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(snapshot.validate_binding(&scope(), &other_ledger).is_err());
    snapshot.validate_binding(&scope(), &ledger)?;
    drop(first);
    assert_eq!(
        registry
            .snapshot(&scope(), 7, &ledger, LIMITS)?
            .selection
            .birth_after,
        Some(3)
    );
    drop(snapshot);
    let remaining = registry.snapshot(&scope(), 7, &ledger, LIMITS)?;
    assert_eq!(remaining.selection, *second.selection());
    drop((remaining, second, old, foreign));
    assert!(registry.0.slots.lock().unwrap().is_empty());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn matching_live_role_cannot_import_another_ledgers_selector() -> Result<()> {
    let registry = InputQueryRegistry::default();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let foreign_ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let local = registry.register(&scope(), 7, &selection("local", 3), &ledger, LIMITS)?;
    let foreign = registry.register(
        &scope(),
        7,
        &selection("foreign", 0),
        &foreign_ledger,
        LIMITS,
    )?;
    let before = ledger.report()?.observed_bytes;
    let foreign_before = foreign_ledger.report()?.observed_bytes;
    let error = registry.snapshot(&scope(), 7, &ledger, (0, 0)).unwrap_err();
    assert!(error.to_string().contains("owned material ledger differs"));
    assert_eq!(ledger.report()?.observed_bytes, before);
    assert_eq!(foreign_ledger.report()?.observed_bytes, foreign_before);
    drop(foreign);
    let roles = registry.snapshot(&scope(), 7, &ledger, LIMITS)?;
    assert_eq!(roles.selection, *local.selection());
    roles.validate_binding(&scope(), &ledger)?;
    drop((roles, local));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(foreign_ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn borrowed_roles_reject_exact_byte_and_node_underflow_before_cloning_or_slot_insertion(
) -> Result<()> {
    let registry = InputQueryRegistry::default();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let selection = selection(&"\u{1}".repeat(512), 0);
    let (bytes, records) = footprint(&scope(), &selection)?;
    let bytes = usize::try_from(bytes.checked_mul(8).unwrap())?;
    let records = records.checked_mul(8).unwrap();
    for limits in [(records, bytes - 1), (records - 1, bytes)] {
        let error = registry
            .register(&scope(), 1, &selection, &ledger, limits)
            .err()
            .context("under-budget Input role accepted")?;
        assert!(matches!(
            error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
            Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_))
        ));
        assert!(registry.0.slots.lock().unwrap().is_empty());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let accepted = registry.register(&scope(), 1, &selection, &ledger, (records, bytes))?;
    drop(accepted);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn contended_drop_and_failed_snapshot_reclaim_slots_and_lease_does_not_retain_registry(
) -> Result<()> {
    let registry = InputQueryRegistry::default();
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let lease = registry.register(&scope(), 1, &selection("held", 0), &ledger, LIMITS)?;
    let slots = registry.0.slots.lock().unwrap();
    drop(lease);
    assert_eq!(slots.len(), 1);
    assert!(ledger.report()?.observed_bytes > 0);
    drop(slots);
    assert!(registry.snapshot(&scope(), 1, &ledger, (1, 1)).is_err());
    assert!(registry.0.slots.lock().unwrap().is_empty());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let lease = registry.register(&scope(), 1, &selection("survivor", 0), &ledger, LIMITS)?;
    let weak = Arc::downgrade(&registry.0);
    drop(registry);
    assert!(weak.upgrade().is_none());
    assert_eq!(lease.selection(), &selection("survivor", 0));
    drop(lease);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn original_queue_writer_advances_live_sessions_and_prunes_finished_roles_without_changing_old_root(
) -> Result<()> {
    use crate::{runtime::tests::support::LifecycleHarness, types::*};
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let message = |text: &str| {
        MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text { text: text.into() },
        )
    };
    let (_, first) = runtime.begin_task_birth_session().await?;
    let one = runtime.enqueue(message("first Input birth")).await?;
    let old = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("old root")?;
    let old_inputs = old.input_evidence.as_ref().context("old Input")?;
    let original = old_inputs.query_coverage()?;
    assert!(original.born.values().any(|key| key.id == one.id));
    let (_, second) = runtime.begin_task_birth_session().await?;
    drop(first);
    let two = runtime.enqueue(message("second Input birth")).await?;
    let next = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("next root")?;
    let next_inputs = next.input_evidence.as_ref().context("next Input")?;
    let remaining = next_inputs.query_coverage()?;
    assert_eq!(remaining.birth_after, Some(original.birth_cursor));
    assert!(!remaining.born.values().any(|key| key.id == one.id));
    assert!(remaining.born.values().any(|key| key.id == two.id));
    remaining.validate()?;
    assert_eq!(old_inputs.query_coverage()?, original);
    runtime.capture_run_once(&second)?;
    drop(second);
    runtime.enqueue(message("no live Input role")).await?;
    let final_root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("final root")?;
    let finished = final_root
        .input_evidence
        .as_ref()
        .context("final Input")?
        .query_coverage()?;
    assert_eq!(finished.birth_after, None);
    assert!(finished.required_keys.is_empty());
    assert!(
        finished.born.is_empty(),
        "accepted coverage cannot recreate a finished role"
    );
    assert!(runtime
        .inner
        .input_queries
        .0
        .slots
        .lock()
        .unwrap()
        .is_empty());
    assert_eq!(old_inputs.query_coverage()?, original);
    Ok(())
}
