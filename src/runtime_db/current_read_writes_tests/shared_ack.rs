use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    runtime_db::{
        current_read_writes::CurrentRecord, evidence::append_audit_event_ack_tx,
        transitions::AuditEventAck,
    },
};

fn acknowledge(
    f: &Fixture,
    ledger: &InputEvidenceBudgetLedger,
    event: &AuditEvent,
    original_writer: bool,
    tamper: Option<bool>,
    expect_reuse: bool,
) -> Result<(CommittedCurrentWrites, AuditEventAck)> {
    f.db.transaction(|tx| {
        let mut native = NativeInputWriteProof::prepare_tx(
            tx, &f.scope, ledger, (4 * 1024 * 1024, 4096), None, |_| Ok(()),
        )?.context("original Native source")?;
        let current = native.with_input_phase(tx, ledger, None, |material| {
            OriginalCurrentWrites::prepare(tx, &f.scope, material, |visit| visit(CurrentFamily::Event, &event.id))
        })?.context("Current schema")?;
        let source = if original_writer {
            append_audit_event_ack_tx(tx, Some(&f.scope.agent_id), event)?.0
        } else {
            AuditEventAck::new(append_audit_event_tx(tx, Some(&f.scope.agent_id), event)?.0)?
        };
        if let Some(foreign_owner) = tamper {
            if foreign_owner {
                tx.execute("UPDATE audit_events SET agent_id='foreign' WHERE audit_event_id=?1", [&event.id])?;
            } else {
                tx.execute("UPDATE audit_events SET data_json=json_set(data_json,'$.data.body','tampered') WHERE audit_event_id=?1", [&event.id])?;
            }
        }
        let writes = native.with_input_phase(tx, ledger, None, |material| {
            let before = material.remaining_limits();
            let writes = current.finish_with_audit_acks(
                material, std::slice::from_ref(&source), |visit| visit(CurrentFamily::Event, &event.id),
            )?;
            let after = material.remaining_limits();
            if expect_reuse {
                assert_eq!(after, before, "body and address slots were already admitted");
            } else {
                assert!(after.0 < before.0 && after.1 < before.1, "unproven/foreign material still needs Native admission");
            }
            Ok(writes)
        })?;
        native.finish_delta_tx(tx, &mut Default::default(), ledger, None)?;
        Ok((writes, source))
    })
}

#[test]
fn current_shared_audit_ack_keeps_wire_and_last_original_holder_without_body_clone() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &f.scope)?;
    let event = AuditEvent::legacy(
        "shared-audit",
        serde_json::json!({
            "nested": vec!["原始正文 \\\"{}[]:,"; 40],
        }),
    );
    let (writes, mut source) = phase.read(|| acknowledge(&f, &ledger, &event, true, None, true))?;
    let body = writes
        .record(&(CurrentFamily::Event, event.id.clone()))
        .context("shared body")?
        .clone();
    let CurrentRecord::Event(selected) = body.record() else {
        panic!("expected Event")
    };
    writes.qualify_notification(&mut source)?;
    assert!(source.current_record().is_some());
    assert!(
        selected.current_record().is_none(),
        "Current body cannot point back to itself"
    );
    let current_record = Arc::downgrade(&body);
    assert!(
        std::ptr::eq::<AuditEvent>(&*source, &**selected),
        "holders must reference the same body"
    );
    assert_eq!(
        serde_json::to_value(&source)?,
        serde_json::to_value(&*source)?
    );
    let replay: AuditEventAck = serde_json::from_value(serde_json::to_value(&source)?)?;
    assert_eq!(replay, source);
    assert!(
        replay.original_material().is_none(),
        "wire replay cannot manufacture writer observation"
    );
    assert!(
        replay.current_record().is_none(),
        "wire replay is not Current qualification"
    );
    let original = Arc::downgrade(
        source
            .original_material()
            .context("original writer material")?,
    );
    let last = source.clone();
    drop((phase, source, writes, replay, body));
    assert!(original.upgrade().is_some());
    assert!(current_record.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(last);
    assert!(original.upgrade().is_none());
    assert!(
        current_record.upgrade().is_none(),
        "qualified ACK must not form a record cycle"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn current_shared_audit_ack_still_checks_sql_before_using_original_material() -> Result<()> {
    for foreign_owner in [false, true] {
        let f = Fixture::new()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &f.scope)?;
        let event = AuditEvent::legacy(
            "shared-audit-tamper",
            serde_json::json!({"body": "original"}),
        );
        let result =
            phase.read(|| acknowledge(&f, &ledger, &event, true, Some(foreign_owner), true));
        let error = format!("{:#}", result.unwrap_err());
        assert!(
            error.contains(if foreign_owner {
                "Current actual owner differs"
            } else {
                "Current canonical ACK differs from SQL payload"
            }),
            "{error}"
        );
        assert_eq!(
            f.db.connection()?
                .query_row("SELECT COUNT(*) FROM audit_events", [], |r| r
                    .get::<_, usize>(0))?,
            0
        );
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn current_owned_or_foreign_audit_phase_is_not_original_writer_material() -> Result<()> {
    for foreign_ledger in [false, true] {
        let f = Fixture::new()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let other = InputEvidenceBudgetLedger::new(Default::default())?;
        let phase =
            ResourceReadMaterial::new(if foreign_ledger { &other } else { &ledger }, &f.scope)?;
        let event = AuditEvent::legacy(
            "owned-audit",
            serde_json::json!({"body": vec!["original"; 20]}),
        );
        let (writes, source) =
            phase.read(|| acknowledge(&f, &ledger, &event, foreign_ledger, None, false))?;
        assert_eq!(source.original_material().is_some(), foreign_ledger);
        drop((writes, source, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(other.report()?.observed_bytes, 0);
    }
    Ok(())
}
