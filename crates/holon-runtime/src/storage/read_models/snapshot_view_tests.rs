use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    storage::AppStorage,
};
use std::sync::Arc;

#[test]
fn snapshot_view_borrows_original_database_and_identity_including_clones() -> Result<()> {
    let home = tempfile::tempdir()?;
    let storage = AppStorage::new_for_test(home.path())?;
    let id = "web_汉字\0🦀".repeat(37);
    let view = RuntimeReadModels::borrowed_for_agent(&storage.runtime_db, &id)?;
    for view in [&view, &view.clone()] {
        assert!(matches!(view.runtime_db, Cow::Borrowed(_)));
        assert!(std::ptr::eq(view.runtime_db.as_ref(), &storage.runtime_db));
        assert!(matches!(view.agent_id, Some(Cow::Borrowed(_))));
        let borrowed = view.agent_id.as_deref().unwrap();
        assert_eq!(borrowed, id);
        assert_eq!(borrowed.as_ptr(), id.as_ptr());
    }
    let owned = storage.read_models();
    assert!(matches!(owned.runtime_db, Cow::Owned(_)));
    assert!(matches!(owned.agent_id, Some(Cow::Owned(_))));
    assert_eq!(owned.agent_id.as_deref(), Some("default"));
    Ok(())
}

#[test]
fn snapshot_view_preserves_scope_validation_and_posture_query_semantics() -> Result<()> {
    let home = tempfile::tempdir()?;
    let storage = AppStorage::new_for_test_with_agent(home.path(), "web")?;
    for id in ["", " ", "\t\r\n", "\u{2003}"] {
        let error = RuntimeReadModels::borrowed_for_agent(&storage.runtime_db, id).unwrap_err();
        let original =
            AppStorage::open_read_only_for_agent(home.path(), id, storage.runtime_db.clone())
                .unwrap_err();
        assert_eq!(error.to_string(), original.to_string());
        assert_eq!(
            error.to_string(),
            "agent-scoped storage requires a non-empty agent id"
        );
    }
    let borrowed = RuntimeReadModels::borrowed_for_agent(&storage.runtime_db, "web")?;
    let owned = storage.read_models();
    for status in [
        AgentStatus::Stopped,
        AgentStatus::AwakeIdle,
        AgentStatus::AwakeRunning,
        AgentStatus::AwaitingTask,
    ] {
        let mut agent = AgentState::new("web");
        agent.status = status;
        let expected = storage.agent_posture_projection(&agent)?;
        assert_eq!(borrowed.agent_posture_for_snapshot(&agent)?, expected);
        assert_eq!(owned.agent_posture_for_snapshot(&agent)?, expected);
    }
    Ok(())
}

#[test]
fn snapshot_view_construction_adds_no_material_or_phase_even_after_refusal() -> Result<()> {
    let home = tempfile::tempdir()?;
    let storage = AppStorage::new_for_test(home.path())?;
    let id = "web_汉字".repeat(1024);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: "snapshot-view".into(),
        identity_incarnation: 1,
    };
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let holder = phase.guard()?;
    let before = phase.finish()?;
    {
        let _entry = phase.enter();
        let view = RuntimeReadModels::borrowed_for_agent(&storage.runtime_db, &id)?;
        assert_eq!(phase.finish()?, before);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        assert!(charge_owned_metadata(u64::MAX, 1).is_err());
        let after_refusal = RuntimeReadModels::borrowed_for_agent(&storage.runtime_db, &id)?;
        assert!(std::ptr::eq(
            after_refusal.runtime_db.as_ref(),
            view.runtime_db.as_ref()
        ));
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard().is_err());
        assert_eq!(ledger.report()?.observed_bytes, before.0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, before.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn trigger_reader_fixture() -> Result<(
    tempfile::TempDir,
    RuntimeDb,
    InputEvidenceScope,
    (u64, usize),
)> {
    let home = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        home.path().join("runtime.sqlite"),
        home.path().join("runtime.lock"),
    )?;
    let record: crate::types::ExternalTriggerRecord = serde_json::from_value(serde_json::json!({
        "external_trigger_id": "reader-fixture",
        "target_agent_id": "web",
        "scope": "agent",
        "delivery_mode": "wake_hint",
        "token": "{,:} \"quoted\" \\ 汉字\0🦀",
        "token_hash": "fixture-hash",
        "status": "revoked",
        "created_at": "2026-10-01T00:00:00Z",
        "revoked_at": null,
        "last_delivered_at": null,
        "delivery_count": 0
    }))?;
    db.external_triggers().upsert(&record)?;
    // Eleven keys/colons, ten commas, two braces and eight string values.
    let cells = 42;
    let scope = InputEvidenceScope {
        agent_id: "trigger-reader-view".into(),
        identity_incarnation: 1,
    };
    let exact = (
        (scope.agent_id.len() as u64
            + 256
            + (4 * std::mem::size_of::<crate::types::ExternalTriggerRecord>()) as u64
            + serde_json::to_vec(&record)?.len() as u64 * 4
            + cells * 128
            + 512)
            * 8,
        64 + 4 * 8 + (cells as usize * 4 + 16) * 8,
    );
    Ok((home, db, scope, exact))
}

#[test]
fn work_queue_trigger_reader_preserves_original_phase_on_scoped_and_global_callers() -> Result<()> {
    let (_home, db, scope, exact) = trigger_reader_fixture()?;
    for scoped in [false, true] {
        let view = RuntimeReadModels::new(db.clone(), scoped.then(|| "web".into()));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
        let original = phase.guard()?;
        let queue = {
            let _entry = phase.enter();
            let queue = view.work_queue()?;
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
            queue
        };
        assert!(queue.items.is_empty());
        assert_eq!(phase.finish()?, exact);
        let held = (queue, original.clone());
        drop((phase, original));
        assert_eq!(ledger.report()?.observed_bytes, exact.0);
        drop(held);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn work_queue_trigger_reader_propagates_slot_refusal_without_partial_result_or_reset() -> Result<()>
{
    let (_home, db, scope, exact) = trigger_reader_fixture()?;
    db.connection()?
        .execute("UPDATE external_triggers SET payload_json = '{'", [])?;
    let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
    let slots = (
        (4 * std::mem::size_of::<crate::types::ExternalTriggerRecord>()) as u64 * 8,
        32,
    );
    for scoped in [false, true] {
        for bytes in [true, false] {
            let view = RuntimeReadModels::new(db.clone(), scoped.then(|| "web".into()));
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                if bytes {
                    header.0 + slots.0 - 1
                } else {
                    exact.0
                },
                if bytes {
                    exact.1
                } else {
                    header.1 + slots.1 - 1
                },
            )?;
            let original = phase.guard()?;
            {
                let _entry = phase.enter();
                for _ in 0..2 {
                    let error = view.work_queue().unwrap_err();
                    assert_eq!(error.to_string(), "trigger list slots admission");
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                }
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
            assert_eq!(ledger.report()?.observed_bytes, header.0);
            assert!(phase.finish().is_err());
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, header.0);
            drop(original);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}
