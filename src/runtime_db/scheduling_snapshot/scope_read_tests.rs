use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime_db::scheduling_snapshot::tests::{ledger, legacy, request, scope};

#[test]
fn input_preparation_reuses_canonical_context_state_and_rejects_drift() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    let weak = Arc::downgrade(&phase.guard()?);
    let prepared = {
        let _entry = phase.enter();
        db.transaction(|tx| {
            let read = ContextDurableRead::new(tx, &scope.agent_id).scope_read()?;
            let prepared = prepare_scheduling_with_context_scope_tx(
                tx, &scope, &request, &ledger, false, &read,
            )?
            .require_current()?;
            assert!(Arc::ptr_eq(
                prepared.facts.state.as_ref().unwrap(),
                read.facts().state.as_ref().unwrap(),
            ));
            let aliases: usize = tx.query_row(
                "SELECT COUNT(*) FROM scheduling_domain_records",
                [],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE agent_states SET control_revision=control_revision+1",
                [],
            )?;
            let error = prepare_scheduling_with_context_scope_tx(
                tx, &scope, &request, &ledger, false, &read,
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "context scope changed after its validated read"
            );
            assert_eq!(
                aliases,
                tx.query_row(
                    "SELECT COUNT(*) FROM scheduling_domain_records",
                    [],
                    |row| row.get::<_, usize>(0),
                )?
            );
            Ok(prepared)
        })?
    };
    phase.finish()?;
    drop(phase);
    assert!(weak.upgrade().is_some());
    drop(prepared);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn scheduling_state_sizing_preserves_exact_canonical_decode_errors() -> Result<()> {
    for payload in ["{", "not JSON", "[", "null", "[]", "{\"id\":4,"] {
        let (_home, db) = legacy(true, |_| Ok(()))?;
        let ledger = ledger()?;
        let scope = scope();
        let request = request();
        let phase = ResourceReadMaterial::new(&ledger, &scope)?;
        let expected = repositories::decode_agent_state_payload(payload).unwrap_err();
        db.transaction(|tx| {
            tx.execute("UPDATE agent_states SET payload_json=?1", [payload])?;
            let _entry = phase.enter();
            let mut material = Material::new(&scope, &request, &ledger);
            let actual = scope_facts(tx, &scope, &mut material, false)
                .err()
                .context("malformed State unexpectedly accepted")?;
            assert_eq!(format!("{actual:#}"), format!("{expected:#}"), "{payload}");
            phase.finish()?;
            Ok(())
        })?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn validated_scheduling_scope_moves_original_state_without_another_decode() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    db.transaction(|tx| {
        let _entry = phase.enter();
        let header = phase.finish()?;
        let mut material = Material::new(&scope, &request, &ledger);
        let read = read_scope_facts(tx, &scope, &mut material, false)?;
        let original = read.facts().state.as_ref().unwrap().id.as_ptr();
        let used = phase.finish()?;
        assert_eq!(
            used,
            (
                header.0 + material.bytes as u64,
                header.1 + material.records
            )
        );
        assert_eq!(ledger.report()?.observed_bytes, used.0);
        let facts = read.into_facts(tx, &scope, false)?;
        assert_eq!(facts.state.as_ref().unwrap().id.as_ptr(), original);
        assert_eq!(phase.finish()?, used);
        Ok(())
    })?;
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn validated_scheduling_scope_rejects_another_transaction_and_scope() -> Result<()> {
    let (_home, db) = legacy(true, |_| Ok(()))?;
    let (_other_home, other) = legacy(true, |_| Ok(()))?;
    let ledger = ledger()?;
    let scope = scope();
    let request = request();
    db.transaction(|tx| {
        let mut material = Material::new(&scope, &request, &ledger);
        let mut read = Some(read_scope_facts(tx, &scope, &mut material, false)?);
        other.transaction(|other_tx| {
            let error = read
                .take()
                .expect("one cross-transaction attempt")
                .into_facts(other_tx, &scope, false)
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Scheduling scope read belongs to another transaction or scope"
            );
            Ok(())
        })?;
        let read = read_scope_facts(tx, &scope, &mut material, false)?;
        let mut foreign = scope.clone();
        foreign.identity_incarnation += 1;
        assert_eq!(
            read.into_facts(tx, &foreign, false)
                .unwrap_err()
                .to_string(),
            "Scheduling scope read belongs to another transaction or scope"
        );
        let read = read_scope_facts(tx, &scope, &mut material, false)?;
        assert_eq!(
            read.into_facts(tx, &scope, true).unwrap_err().to_string(),
            "Scheduling scope read belongs to another transaction or scope"
        );
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn validated_scheduling_scope_rejects_in_transaction_canonical_drift() -> Result<()> {
    for sql in [
        "UPDATE agent_states SET control_revision=control_revision+1",
        "UPDATE agent_states SET payload_json='not JSON'",
        "UPDATE agent_identities SET payload_json='not JSON'",
        "UPDATE agent_identities SET status='deleting'",
    ] {
        let (_home, db) = legacy(true, |_| Ok(()))?;
        let ledger = ledger()?;
        let scope = scope();
        let request = request();
        db.transaction(|tx| {
            let mut material = Material::new(&scope, &request, &ledger);
            let read = read_scope_facts(tx, &scope, &mut material, false)?;
            tx.execute(sql, [])?;
            assert_eq!(
                read.into_facts(tx, &scope, false).unwrap_err().to_string(),
                "Scheduling scope changed after its validated read",
                "{sql}"
            );
            let aliases: usize = tx.query_row(
                "SELECT COUNT(*) FROM scheduling_domain_records",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(aliases, 0);
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
