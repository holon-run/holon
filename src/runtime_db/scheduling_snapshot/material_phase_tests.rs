use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;

#[test]
fn sql_material_never_substitutes_a_host_or_foreign_phase_for_its_owned_view() -> Result<()> {
    let scope = scope();
    let request = request();
    let ledger = ledger()?;
    let foreign_ledger = super::ledger()?;
    let mut foreign_scope = scope.clone();
    foreign_scope.identity_incarnation += 1;
    let parent = ResourceReadMaterial::new(&ledger, &scope)?;
    let host = {
        let _entry = parent.enter();
        ResourceReadMaterial::host(&ledger)?
    };
    for phase in [
        host,
        ResourceReadMaterial::new(&foreign_ledger, &scope)?,
        ResourceReadMaterial::new(&ledger, &foreign_scope)?,
    ] {
        let mut material = Material::new(&scope, &request, &ledger);
        let original = phase.guard()?;
        {
            let _entry = phase.enter();
            material.charge(1024, 3)?;
        }
        assert_eq!(material.guards.len(), 1);
        assert!(!Arc::ptr_eq(&material.guards[0], &original));
        material.guards[0].validate_owned_material_binding(
            &scope,
            &ledger,
            material.bytes as u64,
        )?;
        let held = material.guards.clone();
        drop(material);
        assert!(held[0]
            .validate_owned_material_binding(&scope, &ledger, 1024)
            .is_ok());
        drop(held);
    }
    drop(parent);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(foreign_ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn sql_material_uses_original_remainder_once_and_holds_its_guard() -> Result<()> {
    let scope = scope();
    let request = request();
    let ledger = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, 1024 * 1024, 4096)?;
    let baseline = phase.finish()?;
    let weak = Arc::downgrade(&phase.guard()?);
    let mut material = Material::new(&scope, &request, &ledger);
    {
        let _entry = phase.enter();
        material.charge(1024, 3)?;
        assert_eq!(
            phase.finish()?,
            (
                baseline.0 + material.bytes as u64,
                baseline.1 + material.records
            )
        );
        material.charge(1024, 3)?;
        assert_eq!(
            phase.finish()?,
            (
                baseline.0 + material.bytes as u64,
                baseline.1 + material.records
            )
        );
    }
    let full = phase.finish()?;
    assert_eq!(material.guards.len(), 1);
    assert!(Arc::ptr_eq(&material.guards[0], &phase.guard()?));
    assert_eq!(ledger.report()?.observed_bytes, full.0);
    let one = (material.bytes as u64 / 2, material.records / 2);
    let holders = material.guards.clone();
    drop(material);
    drop(phase);
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(holders);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for bytes_bound in [false, true] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            if bytes_bound {
                full.0 - one.0
            } else {
                1024 * 1024
            },
            if bytes_bound { 4096 } else { full.1 - one.1 },
        )?;
        let mut material = Material::new(&scope, &request, &ledger);
        {
            let _entry = phase.enter();
            material.charge(1024, 3)?;
            let error = material.charge(1024, 3).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("ResourceExhausted: context resource read material"),
                "{error:#}"
            );
        }
        assert!(phase.finish().is_err());
        drop(material);
        drop(phase);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    // Without a Context phase the original independent allowance still applies.
    let mut independent = Material::new(&scope, &request, &ledger);
    independent.charge(1024, 3)?;
    assert_eq!((independent.bytes as u64, independent.records), one);
    drop(independent);
    let mut small = request;
    small.byte_limit = 1;
    let mut local = Material::new(&scope, &small, &ledger);
    let error = local.charge(1024, 3).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SchedulingSnapshotError>(),
        Some(SchedulingSnapshotError::ResourceExhausted(_))
    ));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn sql_body_admission_precedes_canonical_decode_and_preserves_the_row() -> Result<()> {
    let id = format!("raw-material-{}", "x".repeat(1024));
    let (_home, db) = legacy(true, |tx| {
        repositories::upsert_timer_tx(tx, &timer(&id, TimerStatus::Active))
    })?;
    let raw = "{".repeat(32 * 1024);
    db.connection()?.execute(
        "UPDATE timers SET payload_json=?2 WHERE timer_id=?1",
        params![id, raw],
    )?;
    let scope = scope();
    let request = request();
    let ledger = ledger()?;
    let key = SchedulingKey::new(SchedulingFamily::Timer, &id);
    for bytes_bound in [false, true] {
        let baseline = ResourceReadMaterial::new(&ledger, &scope)?.finish()?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            if bytes_bound { 8192 } else { 16 * 1024 * 1024 },
            if bytes_bound { 4096 } else { baseline.1 },
        )?;
        let error = {
            let _entry = phase.enter();
            let mut material = Material::new(&scope, &request, &ledger);
            db.transaction(|tx| stored::read_stored(tx, &key, &scope.agent_id, Some(&mut material)))
                .unwrap_err()
        };
        assert!(
            error
                .to_string()
                .contains("ResourceExhausted: context resource read material"),
            "{error:#}"
        );
        assert!(phase.finish().is_err());
        drop(phase);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let ample = ResourceReadMaterial::new(&ledger, &scope)?;
    let error = {
        let _entry = ample.enter();
        let mut material = Material::new(&scope, &request, &ledger);
        db.transaction(|tx| stored::read_stored(tx, &key, &scope.agent_id, Some(&mut material)))
            .unwrap_err()
    };
    assert!(
        !error.to_string().contains("ResourceExhausted"),
        "{error:#}"
    );
    assert!(ample.finish()?.0 > 32 * 1024);
    drop(ample);
    let saved: (String, usize) = db.connection()?.query_row(
        "SELECT payload_json,(SELECT COUNT(*) FROM scheduling_domain_current) FROM timers WHERE timer_id=?1",
        [&id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(saved, (raw, 0));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn retired_sql_snapshot_keeps_original_phase_until_its_last_holder() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let mut scope = scope();
    scope.identity_incarnation += 1;
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let weak = Arc::downgrade(&phase.guard()?);
    let snapshot = {
        let _entry = phase.enter();
        db.transaction(|tx| prepare_scheduling_domain_snapshot_tx(tx, &scope, &request(), &ledger))?
    };
    assert!(matches!(
        &snapshot,
        SchedulingBootstrapSnapshot::Retired { .. }
    ));
    assert!(phase.finish()?.0 > 2048);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(snapshot);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
