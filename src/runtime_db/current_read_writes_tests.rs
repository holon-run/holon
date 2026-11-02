//! Real Native material and SQLite rollback, not a fabricated Current ACK.
#[path = "current_read_writes_tests/event_page.rs"]
mod event_page;
#[path = "current_read_writes_tests/history_schema.rs"]
mod history_schema;
#[path = "current_read_writes_tests/shared_ack.rs"]
mod shared_ack;
#[path = "current_read_writes_tests/timer_cancellation.rs"]
mod timer_cancellation;

use std::sync::Arc;

use super::*;
use crate::{
    runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    runtime_db::{
        current_read_writes::{
            CommittedCurrentWrites, CurrentAddressFact, CurrentFamily, OriginalCurrentWrites,
        },
        evidence::append_audit_event_tx,
        input_evidence_snapshot::native_birth::NativeInputWriteProof,
    },
};

fn attempt(
    f: &Fixture,
    ledger: &InputEvidenceBudgetLedger,
    observed: &[(CurrentFamily, &str)],
    actual: &[(CurrentFamily, &str)],
    mut action: impl FnMut(&rusqlite::Transaction<'_>) -> Result<()>,
) -> Result<CommittedCurrentWrites> {
    f.db.transaction(|tx| {
        let mut native = NativeInputWriteProof::prepare_tx(
            tx,
            &f.scope,
            ledger,
            (4 * 1024 * 1024, 4096),
            None,
            |_| Ok(()),
        )?
        .context("original Native source")?;
        let current = native
            .with_input_phase(tx, ledger, None, |material| {
                OriginalCurrentWrites::prepare(tx, &f.scope, material, |visit| {
                    for (family, id) in observed {
                        visit(*family, id)?;
                    }
                    Ok(())
                })
            })?
            .context("Current schema")?;
        action(tx)?;
        let ack = native.with_input_phase(tx, ledger, None, |material| {
            current.finish(material, |visit| {
                for (family, id) in actual {
                    visit(*family, id)?;
                }
                Ok(())
            })
        })?;
        native.finish_delta_tx(tx, &mut Default::default(), ledger, None)?;
        Ok(ack)
    })
}

#[test]
fn cached_current_statements_rebind_addresses_and_observe_commits_not_old_results() -> Result<()> {
    use crate::runtime_db::current_query::{prepare, CurrentSelection};

    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let a = AuditEvent::legacy("cached-current-a", serde_json::json!({"version": 1}));
    let b = AuditEvent::legacy("cached-current-b", serde_json::json!({"version": 2}));
    let keys = [
        (CurrentFamily::Event, a.id.as_str()),
        (CurrentFamily::Event, b.id.as_str()),
    ];
    let read = || {
        f.db.transaction(|tx| {
            let selection = CurrentSelection {
                events_after: Some(0),
                ..Default::default()
            };
            let query = prepare(
                tx,
                &f.scope,
                Some(&selection),
                &ledger,
                (4 * 1024 * 1024, 4096),
            )?;
            let captured = query.capture(&selection, &ledger)?;
            Ok(captured
                .events()
                .filter(|event| event.id == a.id || event.id == b.id)
                .map(|event| (event.id.clone(), event.data.clone()))
                .collect::<std::collections::BTreeMap<_, _>>())
        })
    };
    assert!(read()?.is_empty());
    attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &a)?;
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &b)?;
        Ok(())
    })?;
    let first = read()?;
    assert_eq!(first[&a.id], a.data);
    assert_eq!(first[&b.id], b.data);
    attempt(&f, &ledger, &keys[..1], &keys[..1], |tx| {
        tx.execute(
            "UPDATE audit_events SET data_json=json_set(data_json,'$.data.version',3) WHERE audit_event_id=?1",
            [&a.id],
        )?;
        Ok(())
    })?;
    let next = read()?;
    assert_eq!(next[&a.id], serde_json::json!({"version": 3}));
    assert_eq!(next[&b.id], b.data);
    assert_eq!(first[&a.id], a.data);
    // A cached SQL program must still reject an unqualified body mutation.
    f.db.connection()?.execute(
        "UPDATE audit_events SET data_json=json_set(data_json,'$.data.version',4) WHERE audit_event_id=?1",
        [&a.id],
    )?;
    assert!(read().is_err());
    Ok(())
}

#[test]
fn current_prepared_empty_query_retains_scope_material_until_last_holder() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let scope_bytes = f.db.transaction(|_| {
        let material =
            crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial::new(
                &f.scope,
                &ledger,
                (1024 * 1024, 1024),
                None,
            )?;
        let bytes = ledger.report()?.observed_bytes;
        drop(material);
        Ok(bytes)
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let query = f.db.transaction(|tx| {
        crate::runtime_db::current_query::prepare(tx, &f.scope, None, &ledger, (1024 * 1024, 1024))
    })?;
    let retained = ledger.report()?.observed_bytes;
    assert!(
        retained > scope_bytes,
        "scope and query metadata must both remain owned"
    );
    let last = query.clone();
    drop(query);
    assert_eq!(ledger.report()?.observed_bytes, retained);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn current_query_identity_uses_exact_integer_incarnation() -> Result<()> {
    use crate::runtime_db::current_query::{prepare, CurrentSelection};
    for invalid in [
        serde_json::json!("1"),
        serde_json::json!(1.0),
        serde_json::json!(true),
    ] {
        let f = Fixture::new()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        f.db.transaction(|tx| {
            let query = prepare(tx, &f.scope, None, &ledger, (1024 * 1024, 1024))?;
            assert_eq!(query.capture(&CurrentSelection::default(), &ledger)?.events().count(), 0);
            tx.execute(
                "UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',json(?2)) WHERE agent_id=?1",
                rusqlite::params![f.scope.agent_id, invalid.to_string()],
            )?;
            assert!(prepare(tx, &f.scope, None, &ledger, (1024 * 1024, 1024)).is_err());
            Ok(())
        })?;
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn current_canonical_event_ack_is_checked_before_its_single_owning_clone() -> Result<()> {
    use crate::runtime_db::current_read_writes::CurrentRecord;
    for tamper in [false, true] {
        let f = Fixture::new()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let event = AuditEvent::legacy(
            "canonical-event",
            serde_json::json!({"nested": [null, {"text": "路\\\"{}[]:,"}]}),
        );
        let result = f.db.transaction(|tx| {
            let mut native = NativeInputWriteProof::prepare_tx(
                tx,
                &f.scope,
                &ledger,
                (1024 * 1024, 1024),
                None,
                |_| Ok(()),
            )?
            .context("original Native source")?;
            let current = native
                .with_input_phase(tx, &ledger, None, |material| {
                    OriginalCurrentWrites::prepare(tx, &f.scope, material, |visit| {
                        visit(CurrentFamily::Event, &event.id)
                    })
                })?
                .context("Current schema")?;
            let (mut canonical, _) = append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
            if tamper {
                canonical.data = serde_json::json!({"different": true});
            }
            let ack = native.with_input_phase(tx, &ledger, None, |material| {
                current.finish_with_events(material, &[canonical], |visit| {
                    visit(CurrentFamily::Event, &event.id)
                })
            })?;
            native.finish_delta_tx(tx, &mut Default::default(), &ledger, None)?;
            Ok(ack)
        });
        if tamper {
            assert!(format!("{:#}", result.unwrap_err())
                .contains("Current canonical ACK differs from SQL payload"));
            let counts: (usize, usize, usize) = f.db.connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_read_revisions),(SELECT COUNT(*) FROM current_read_qualification)",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(counts, (0, 0, 0));
        } else {
            let ack = result?;
            ack.validate_binding(&f.scope, &ledger)?;
            let body = ack
                .record(&(CurrentFamily::Event, event.id.clone()))
                .unwrap()
                .clone();
            let CurrentRecord::Event(actual) = body.record() else {
                panic!("expected canonical Event");
            };
            assert_eq!(actual.data, event.data);
            assert!(actual.event_seq > 0);
            let guard = body.weak_material();
            drop(ack);
            assert!(guard.upgrade().is_some());
            drop(body);
            assert!(guard.upgrade().is_none());
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn current_original_address_batch_reserves_one_owner_and_rejects_growing_second_pass() -> Result<()>
{
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    f.db.transaction(|tx| {
        let mut native = NativeInputWriteProof::prepare_tx(
            tx,
            &f.scope,
            &ledger,
            (1024 * 1024, 1024),
            None,
            |_| Ok(()),
        )?
        .context("Native source")?;
        native.with_input_phase(tx, &ledger, None, |material| {
            let before = material.remaining_limits();
            let current = OriginalCurrentWrites::prepare(tx, &f.scope, material, |visit| {
                for id in ["first-路", "second-路", "third-路"] {
                    visit(CurrentFamily::Event, id)?;
                }
                Ok(())
            })?
            .context("Current source")?;
            assert_eq!(before.1 - material.remaining_limits().1, 8 + 3 * 8 + 1);
            drop(current);
            let mut pass = 0;
            let error = OriginalCurrentWrites::prepare(tx, &f.scope, material, |visit| {
                pass += 1;
                visit(
                    CurrentFamily::Event,
                    if pass == 1 {
                        "short"
                    } else {
                        "longer-second-pass"
                    },
                )
            })
            .err()
            .context("changed original footprint accepted")?;
            assert!(format!("{error:#}").contains("Current original footprint changed"));
            assert_eq!(pass, 2);
            Ok(())
        })
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn current_original_address_slot_moves_to_ack_without_another_map_allocation() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let key = (CurrentFamily::Event, "absent-original-路".to_owned());
    let ack = f.db.transaction(|tx| {
        let mut native = NativeInputWriteProof::prepare_tx(
            tx,
            &f.scope,
            &ledger,
            (1024 * 1024, 1024),
            None,
            |_| Ok(()),
        )?
        .context("original Native source")?;
        let (current, identity) = native.with_input_phase(tx, &ledger, None, |material| {
            let current = OriginalCurrentWrites::prepare(tx, &f.scope, material, |visit| {
                visit(key.0, &key.1)
            })?
            .context("Current schema")?;
            let identity = current.slot_identity_for_fixture(&key);
            Ok((current, identity))
        })?;
        let ack = native.with_input_phase(tx, &ledger, None, |material| {
            let remaining = material.remaining_limits();
            let ack = current.finish(material, |visit| visit(key.0, &key.1))?;
            assert_eq!(material.remaining_limits(), remaining);
            assert_eq!(ack.slot_identity_for_fixture(&key), identity);
            Ok(ack)
        })?;
        native.finish_delta_tx(tx, &mut Default::default(), &ledger, None)?;
        Ok(ack)
    })?;
    assert!(!ack.is_empty());
    assert_eq!(ack.facts().get(&key), Some(&CurrentAddressFact::Unborn));
    ack.validate_binding(&f.scope, &ledger)?;
    drop(ack);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn current_canonical_queue_ack_matches_sql_before_clone_and_rolls_back_tampering() -> Result<()> {
    use crate::{
        runtime_db::{current_read_writes::CurrentRecord, repositories::upsert_queue_entry_tx},
        types::{QueueEntryRecord, QueueEntryStatus},
    };
    for tamper in [false, true] {
        let f = Fixture::new()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        let queue = QueueEntryRecord {
            message_id: "queue-canonical-路".into(),
            agent_id: f.scope.agent_id.clone(),
            priority: Priority::Normal,
            status: QueueEntryStatus::Queued,
            created_at: f.at,
            updated_at: f.at,
        };
        let key = (CurrentFamily::Queue, queue.message_id.clone());
        let result = f.db.transaction(|tx| {
            let mut native = NativeInputWriteProof::prepare_tx(
                tx,
                &f.scope,
                &ledger,
                (1024 * 1024, 1024),
                None,
                |_| Ok(()),
            )?
            .context("original Native source")?;
            let current = native
                .with_input_phase(tx, &ledger, None, |material| {
                    OriginalCurrentWrites::prepare(tx, &f.scope, material, |visit| {
                        visit(key.0, &key.1)
                    })
                })?
                .context("Current schema")?;
            upsert_queue_entry_tx(tx, &queue)?;
            let mut canonical = queue.clone();
            if tamper {
                canonical.status = QueueEntryStatus::Aborted;
            }
            let ack = native.with_input_phase(tx, &ledger, None, |material| {
                current
                    .finish_with_records(material, &[], &[canonical], |visit| visit(key.0, &key.1))
            })?;
            native.finish_delta_tx(tx, &mut Default::default(), &ledger, None)?;
            Ok(ack)
        });
        if tamper {
            assert!(format!("{:#}", result.unwrap_err())
                .contains("Current canonical ACK differs from SQL payload"));
            let counts: (usize, usize) = f.db.connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM queue_entries),(SELECT COUNT(*) FROM current_read_qualification)",
                [], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(counts, (0, 0));
        } else {
            let ack = result?;
            ack.validate_binding(&f.scope, &ledger)?;
            let body = ack.record(&key).unwrap().clone();
            assert!(matches!(body.record(), CurrentRecord::Queue(actual) if actual == &queue));
            let guard = body.weak_material();
            drop(ack);
            assert!(guard.upgrade().is_some());
            drop(body);
            assert!(guard.upgrade().is_none());
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn current_body_capture_releases_ack_and_siblings_but_keeps_original_material() -> Result<()> {
    use crate::runtime_db::current_read_writes::CurrentRecord;
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let a = AuditEvent::legacy("current-owned-a", serde_json::json!({"text":"路\\\"{}[]"}));
    let b = AuditEvent::legacy("current-owned-b", serde_json::json!({"text":"other"}));
    let keys = [
        (CurrentFamily::Event, a.id.as_str()),
        (CurrentFamily::Event, b.id.as_str()),
    ];
    let ack = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &a)?;
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &b)?;
        Ok(())
    })?;
    ack.validate_binding(&f.scope, &ledger)?;
    let selected = ack
        .record(&(CurrentFamily::Event, a.id.clone()))
        .unwrap()
        .clone();
    let material = selected.weak_material();
    let sibling = Arc::downgrade(ack.record(&(CurrentFamily::Event, b.id.clone())).unwrap());
    drop(ack);
    assert!(sibling.upgrade().is_none());
    f.db.connection()?
        .execute_batch("DROP TABLE audit_events;")?;
    let CurrentRecord::Event(actual) = selected.record() else {
        panic!("expected original Event");
    };
    assert_eq!(actual.id, a.id);
    assert_eq!(actual.data, a.data);
    assert!(actual.event_seq > 0);
    assert!(material.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    let last = selected.clone();
    drop(selected);
    assert!(material.upgrade().is_some());
    drop(last);
    assert!(material.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn current_install_rejects_an_old_body_even_at_the_same_qualified_address() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy("current-old-body", serde_json::json!({"version":1}));
    let key = (CurrentFamily::Event, event.id.clone());
    let keys = [(CurrentFamily::Event, event.id.as_str())];
    let before = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })?;
    let mut after = attempt(&f, &ledger, &keys, &keys, |tx| {
        tx.execute(
            "UPDATE audit_events SET data_json=json_set(data_json,'$.data.version',2) WHERE audit_event_id=?1",
            [&event.id],
        )?;
        Ok(())
    })?;
    after.validate_binding(&f.scope, &ledger)?;
    after.replace_record_for_fixture(&key, before.record(&key).unwrap().clone());
    let error = after.validate_binding(&f.scope, &ledger).unwrap_err();
    assert!(format!("{error:#}").contains("owning body differs"));
    Ok(())
}

#[test]
fn current_body_admission_failure_rolls_back_original_write_and_qualification() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy(
        "current-large-body",
        serde_json::json!({"text":"x".repeat(1024 * 1024)}),
    );
    let keys = [(CurrentFamily::Event, event.id.as_str())];
    let error = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
        Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_))
    ));
    assert_eq!(qualified_count(&f)?, 0);
    let count: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE audit_event_id=?1",
        [&event.id],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn current_generated_addresses_cover_original_and_command_waits_not_late_or_raw() -> Result<()> {
    use crate::runtime_db::transitions::wait_trigger_audit_id;
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let message = "result-路";
    let ids = [
        wait_trigger_audit_id(message, None, false),
        wait_trigger_audit_id(message, None, true),
        wait_trigger_audit_id(message, Some("active-路"), false),
        wait_trigger_audit_id(message, Some("triggered-路"), false),
        wait_trigger_audit_id(message, Some("command-路"), false),
        wait_trigger_audit_id(message, Some("late-路"), false),
        wait_trigger_audit_id(message, Some("closed-路"), false),
    ];
    for id in [
        "active-路",
        "triggered-路",
        "closed-路",
        "command-路",
        "late-路",
    ] {
        let mut owner = crate::types::WorkItemRecord::new(
            &f.scope.agent_id,
            "independent Wait owner",
            crate::types::WorkItemState::Open,
        );
        owner.id = id.into();
        f.db.work_items().insert_new(&owner)?;
    }
    let wait = |id: &str, status: &str| -> Result<crate::types::WaitConditionRecord> {
        Ok(serde_json::from_value(serde_json::json!({
            "id": id, "agent_id": f.scope.agent_id, "status": status,
            "work_item_id": id,
            "kind": "task", "waiting_for": "address only",
            "created_at": f.at, "updated_at": f.at,
        }))?)
    };
    let ack = f.db.transaction(|tx| {
        for (id, status) in [
            ("active-路", "active"),
            ("triggered-路", "triggered"),
            ("closed-路", "cancelled"),
        ] {
            crate::runtime_db::repositories::upsert_wait_condition_tx(tx, &wait(id, status)?)?;
        }
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM wait_conditions WHERE status IN ('active','triggered')",
                [],
                |row| row.get::<_, usize>(0),
            )?,
            2
        );
        let mut raw = AuditEvent::legacy("raw", serde_json::json!({}));
        raw.id = ids[1].clone();
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &raw)?;
        let mut native = NativeInputWriteProof::prepare_tx(
            tx,
            &f.scope,
            &ledger,
            (4 * 1024 * 1024, 4096),
            None,
            |_| Ok(()),
        )?
        .context("Native source")?;
        let current = native.with_input_phase(tx, &ledger, None, |material| {
            let mut current = OriginalCurrentWrites::prepare(tx, &f.scope, material, |_| Ok(()))?
                .context("Current source")?;
            current.prepare_trigger_events(message, ["command-路"], material)?;
            Ok(current)
        })?;
        crate::runtime_db::repositories::upsert_wait_condition_tx(tx, &wait("late-路", "active")?)?;
        for id in &ids {
            if id == &ids[1] {
                continue;
            }
            let mut event = AuditEvent::legacy("generated", serde_json::json!({}));
            event.id = id.clone();
            append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        }
        native.with_input_phase(tx, &ledger, None, |material| {
            current.finish(material, |visit| {
                for id in &ids {
                    visit(CurrentFamily::Event, id)?;
                }
                Ok(())
            })
        })
    })?;
    for (index, id) in ids.iter().enumerate() {
        let fact = &ack.facts()[&(CurrentFamily::Event, id.clone())];
        if [1, 5, 6].contains(&index) {
            assert_eq!(fact, &CurrentAddressFact::Unprepared);
        } else {
            assert!(
                matches!(fact, CurrentAddressFact::Qualified(_)),
                "{id}: {fact:?}"
            );
        }
    }
    let guard = ack.weak_guard_for_fixture();
    drop(ack);
    assert!(guard.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn current_generated_address_one_byte_short_cannot_replenish_original_phase_and_rolls_back(
) -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let error =
        f.db.transaction(|tx| {
            let mut native = NativeInputWriteProof::prepare_tx(
                tx,
                &f.scope,
                &ledger,
                (4 * 1024 * 1024, 4096),
                None,
                |_| Ok(()),
            )?
            .context("Native source")?;
            let mut current = native
                .with_input_phase(tx, &ledger, None, |material| {
                    OriginalCurrentWrites::prepare(tx, &f.scope, material, |_| Ok(()))
                })?
                .context("Current source")?;
            let first = native
                .with_input_phase(tx, &ledger, None, |material| {
                    let batch_bytes = crate::runtime_db::transitions::WAIT_TRIGGERED_AUDIT_PREFIX
                        .len()
                        + crate::runtime_db::transitions::WAIT_STALE_AUDIT_PREFIX.len()
                        + 2 * "result-路".len()
                        + 2 * 1024;
                    let overhead = std::mem::size_of::<
                        crate::runtime::input_evidence_budget::BudgetViewGuard,
                    >() + 2 * std::mem::size_of::<
                        std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
                    >();
                    let consume = material.remaining_limits().0 - batch_bytes - 2 * overhead + 1;
                    material.observe_native_footprint(consume, 0)?;
                    assert_eq!(material.remaining_limits().0, batch_bytes + overhead - 1);
                    current.prepare_trigger_events("result-路", std::iter::empty(), material)
                })
                .unwrap_err();
            let retry = native
                .with_input_phase(tx, &ledger, None, |material| {
                    current.prepare_trigger_events("result-路", std::iter::empty(), material)
                })
                .unwrap_err();
            assert_eq!(format!("{first:#}"), format!("{retry:#}"));
            Err::<(), _>(first)
        })
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>(),
        Some(crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError::ResourceExhausted(_))
    ));
    let revisions: usize =
        f.db.connection()?
            .query_row("SELECT COUNT(*) FROM current_read_revisions", [], |row| {
                row.get(0)
            })?;
    assert_eq!(revisions, 0);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn current_finish_rejects_a_different_material_ledger_before_commit() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy("foreign-finish", serde_json::json!({}));
    let error =
        f.db.transaction(|tx| {
            let mut native = NativeInputWriteProof::prepare_tx(
                tx,
                &f.scope,
                &ledger,
                (4 * 1024 * 1024, 4096),
                None,
                |_| Ok(()),
            )?
            .context("original Native source")?;
            let current = native
                .with_input_phase(tx, &ledger, None, |material| {
                    OriginalCurrentWrites::prepare(tx, &f.scope, material, |visit| {
                        visit(CurrentFamily::Event, &event.id)
                    })
                })?
                .context("Current schema")?;
            append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
            let mut material =
                crate::runtime_db::input_evidence_snapshot::read_material::InputReadMaterial::new(
                    &f.scope,
                    &foreign,
                    (4 * 1024 * 1024, 4096),
                    None,
                )?;
            current.finish(&mut material, |visit| {
                visit(CurrentFamily::Event, &event.id)
            })?;
            Ok(())
        })
        .unwrap_err();
    assert!(format!("{error:#}").contains("owned material ledger differs"));
    let counts: (usize, usize, usize) = f.db.connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_read_revisions),(SELECT COUNT(*) FROM current_read_qualification)",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(counts, (0, 0, 0));
    Ok(())
}

#[test]
fn current_installed_missing_schema_is_not_legacy_absence() -> Result<()> {
    for tables in [
        "DROP TABLE current_read_qualification;",
        "DROP TABLE current_read_revisions;",
        "DROP TABLE current_read_qualification; DROP TABLE current_read_revisions;",
    ] {
        let f = Fixture::new()?;
        let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
        f.db.connection()?.execute_batch(tables)?;
        let error = attempt(&f, &ledger, &[], &[], |_| Ok(())).unwrap_err();
        assert!(format!("{error:#}").contains("installed Current source is incomplete"));
        // Only a genuinely uninstalled source may return None.
        f.db.connection()?.execute_batch(
            "DROP TABLE IF EXISTS current_read_qualification; DROP TABLE IF EXISTS current_read_revisions;",
        )?;
        f.db.connection()?.execute(
            "DELETE FROM schema_migrations WHERE version>=?1",
            [crate::runtime_db::current_read_writes::VERSION],
        )?;
        f.db.transaction(|tx| {
            let mut native = NativeInputWriteProof::prepare_tx(
                tx,
                &f.scope,
                &ledger,
                (4 * 1024 * 1024, 4096),
                None,
                |_| Ok(()),
            )?
            .context("original Native source")?;
            let current = native.with_input_phase(tx, &ledger, None, |material| {
                OriginalCurrentWrites::prepare(tx, &f.scope, material, |_| Ok(()))
            })?;
            assert!(current.is_none());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn current_deleting_scope_keeps_original_qualification_but_cannot_birth() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let known = AuditEvent::legacy("before-deleting", serde_json::json!({}));
    let keys = [(CurrentFamily::Event, known.id.as_str())];
    let original = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &known)?;
        Ok(())
    })?;
    f.db.connection()?.execute(
        "UPDATE agent_identities SET status='deleting',payload_json=json_set(payload_json,'$.status','deleting') WHERE agent_id=?1",
        [&f.scope.agent_id],
    )?;
    let next = attempt(&f, &ledger, &keys, &keys, |tx| {
        tx.execute(
            "UPDATE audit_events SET data_json=json_set(data_json,'$.data.recovery',true) WHERE audit_event_id=?1",
            [&known.id],
        )?;
        Ok(())
    })?;
    assert!(next.is_complete());
    assert_ne!(original.facts(), next.facts());
    let new = AuditEvent::legacy("after-deleting", serde_json::json!({}));
    let keys = [(CurrentFamily::Event, new.id.as_str())];
    let denied = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &new)?;
        Ok(())
    })?;
    assert!(!denied.is_complete());
    assert!(denied.facts().values().all(|fact| matches!(
        fact,
        crate::runtime_db::current_read_writes::CurrentAddressFact::Unprepared
    )));
    assert_eq!(qualified_count(&f)?, 1);
    assert_eq!(denied.base_revision, next.revision);
    assert_eq!(denied.revision, next.revision + 1);
    Ok(())
}

fn qualified_count(f: &Fixture) -> Result<usize> {
    Ok(f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM current_read_qualification WHERE qualification_state='qualified'",
        [],
        |row| row.get(0),
    )?)
}

#[test]
fn current_unborn_observation_preserves_original_first_birth() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy("current-unborn", serde_json::json!({}));
    let key = (CurrentFamily::Event, event.id.clone());
    let keys = [(CurrentFamily::Event, event.id.as_str())];
    let unborn = attempt(&f, &ledger, &keys, &keys, |_| Ok(()))?;
    assert_eq!(unborn.facts().get(&key), Some(&CurrentAddressFact::Unborn));
    let still_unborn = attempt(&f, &ledger, &keys, &keys, |_| Ok(()))?;
    assert_eq!(still_unborn.facts(), unborn.facts());
    let born = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })?;
    assert!(born.is_complete());
    assert!(born.qualifies(&key));
    assert_eq!(born.base_revision, still_unborn.revision);
    assert_eq!(qualified_count(&f)?, 1);
    Ok(())
}

#[test]
fn current_unborn_observation_does_not_qualify_an_earlier_raw_insertion() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy("current-unborn-raw", serde_json::json!({}));
    let key = (CurrentFamily::Event, event.id.clone());
    let keys = [(CurrentFamily::Event, event.id.as_str())];
    let unborn = attempt(&f, &ledger, &keys, &keys, |_| Ok(()))?;
    assert_eq!(unborn.facts().get(&key), Some(&CurrentAddressFact::Unborn));
    f.db.transaction(|tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })?;
    let denied = attempt(&f, &ledger, &keys, &keys, |_| Ok(()))?;
    assert!(!denied.is_complete());
    assert_eq!(
        denied.facts().get(&key),
        Some(&CurrentAddressFact::Unprepared)
    );
    assert_eq!(qualified_count(&f)?, 0);
    Ok(())
}

#[test]
fn current_unborn_observation_cannot_birth_in_deleting_scope() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy("current-unborn-deleting", serde_json::json!({}));
    let key = (CurrentFamily::Event, event.id.clone());
    let keys = [(CurrentFamily::Event, event.id.as_str())];
    let unborn = attempt(&f, &ledger, &keys, &keys, |_| Ok(()))?;
    assert_eq!(unborn.facts().get(&key), Some(&CurrentAddressFact::Unborn));
    f.db.connection()?.execute(
        "UPDATE agent_identities SET status='deleting',payload_json=json_set(payload_json,'$.status','deleting') WHERE agent_id=?1",
        [&f.scope.agent_id],
    )?;
    let denied = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })?;
    assert!(!denied.is_complete());
    assert_eq!(
        denied.facts().get(&key),
        Some(&CurrentAddressFact::Unprepared)
    );
    assert_eq!(qualified_count(&f)?, 0);
    Ok(())
}

#[test]
fn current_original_delete_keeps_absence_fence_and_cannot_rebirth() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy("current-delete", serde_json::json!({}));
    let key = (CurrentFamily::Event, event.id.clone());
    let keys = [(CurrentFamily::Event, event.id.as_str())];
    let born = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })?;
    let removed = attempt(&f, &ledger, &keys, &keys, |tx| {
        tx.execute(
            "DELETE FROM audit_events WHERE audit_event_id=?1",
            [&event.id],
        )?;
        Ok(())
    })?;
    assert!(removed.is_complete());
    assert_eq!(removed.facts().get(&key), Some(&CurrentAddressFact::Absent));
    assert_eq!(removed.base_revision, born.revision);
    assert_eq!(removed.revision, born.revision + 1);
    assert_eq!(qualified_count(&f)?, 0);
    let absent = attempt(&f, &ledger, &keys, &keys, |_| Ok(()))?;
    assert!(absent.is_complete());
    assert_eq!(absent.facts(), removed.facts());
    // This is a negative address fact, never new body-birth authority.
    let reborn = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })?;
    assert!(!reborn.is_complete());
    assert_eq!(
        reborn.facts().get(&key),
        Some(&CurrentAddressFact::Unprepared)
    );
    assert_eq!(qualified_count(&f)?, 0);
    assert!(born.qualifies(&key));
    assert_eq!(removed.facts().get(&key), Some(&CurrentAddressFact::Absent));
    Ok(())
}

#[test]
fn current_raw_and_late_deletions_do_not_become_known_absence() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    for late in [false, true] {
        let event = AuditEvent::legacy("unprepared-delete", serde_json::json!({}));
        let key = (CurrentFamily::Event, event.id.clone());
        let keys = [(CurrentFamily::Event, event.id.as_str())];
        f.db.transaction(|tx| {
            append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
            Ok(())
        })?;
        let removed = attempt(&f, &ledger, if late { &[] } else { &keys }, &keys, |tx| {
            tx.execute(
                "DELETE FROM audit_events WHERE audit_event_id=?1",
                [&event.id],
            )?;
            Ok(())
        })?;
        assert!(!removed.is_complete());
        assert_eq!(
            removed.facts().get(&key),
            Some(&CurrentAddressFact::Unprepared)
        );
        // Clearing canonical storage cannot launder the raw/late address.
        let absent = attempt(&f, &ledger, &keys, &keys, |_| Ok(()))?;
        assert!(!absent.is_complete());
        assert_eq!(absent.facts(), removed.facts());
        let retry = attempt(&f, &ledger, &keys, &keys, |tx| {
            append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
            Ok(())
        })?;
        assert!(!retry.is_complete());
        assert_eq!(retry.facts(), removed.facts());
    }
    assert_eq!(qualified_count(&f)?, 0);
    Ok(())
}

#[test]
fn current_absence_does_not_cross_incarnation() -> Result<()> {
    let mut f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy("incarnation-absence", serde_json::json!({}));
    let keys = [(CurrentFamily::Event, event.id.as_str())];
    let absent = attempt(&f, &ledger, &keys, &keys, |_| Ok(()))?;
    assert!(absent.is_complete());
    f.scope.identity_incarnation += 1;
    f.db.connection()?.execute(
        "UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',?2) WHERE agent_id=?1",
        params![f.scope.agent_id, f.scope.identity_incarnation],
    )?;
    let reborn = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })?;
    assert!(!reborn.is_complete());
    assert!(reborn
        .facts()
        .values()
        .all(|fact| *fact == CurrentAddressFact::Unprepared));
    assert_eq!(qualified_count(&f)?, 0);
    Ok(())
}

#[test]
fn current_original_birth_update_binding_and_final_holder() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let event = AuditEvent::legacy("current-original", serde_json::json!({"phase":"first"}));
    let keys = [(CurrentFamily::Event, event.id.as_str())];
    let ack = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })?;
    assert!(ack.is_complete());
    assert_eq!(ack.base_revision, 0);
    assert_eq!(ack.revision, 1);
    assert_eq!(qualified_count(&f)?, 1);
    ack.validate_binding(&f.scope, &ledger)?;
    let other_ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(ack.validate_binding(&f.scope, &other_ledger).is_err());
    let mut foreign = f.scope.clone();
    foreign.identity_incarnation += 1;
    assert!(ack.validate_binding(&foreign, &ledger).is_err());
    let first = ack.facts().clone();
    let next = attempt(&f, &ledger, &keys, &keys, |tx| {
        // A legitimate known-address update is observed before changing it.
        tx.execute(
            "UPDATE audit_events SET data_json=json_set(data_json,'$.data.phase','second') WHERE audit_event_id=?1",
            [&event.id],
        )?;
        Ok(())
    })?;
    assert!(next.is_complete());
    assert_eq!(next.base_revision, 1);
    assert_eq!(next.revision, 2);
    assert_ne!(first, next.facts());
    assert_eq!(ack.facts(), first);
    let weak = next.weak_guard_for_fixture();
    let last = next.clone();
    drop(next);
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[test]
fn current_raw_late_and_changed_canonical_values_never_gain_authority() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let raw = AuditEvent::legacy("raw", serde_json::json!({}));
    f.db.transaction(|tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &raw)?;
        Ok(())
    })?;
    let keys = [(CurrentFamily::Event, raw.id.as_str())];
    let denied = attempt(&f, &ledger, &keys, &keys, |_| Ok(()))?;
    assert!(!denied.is_complete());
    assert!(denied.facts().values().all(|fact| matches!(
        fact,
        crate::runtime_db::current_read_writes::CurrentAddressFact::Unprepared
    )));
    assert_eq!(qualified_count(&f)?, 0);
    let late = AuditEvent::legacy("late", serde_json::json!({}));
    let late_keys = [(CurrentFamily::Event, late.id.as_str())];
    let denied = attempt(&f, &ledger, &[], &late_keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &late)?;
        Ok(())
    })?;
    assert!(!denied.is_complete());
    assert_eq!(qualified_count(&f)?, 0);
    let new = AuditEvent::legacy("new", serde_json::json!({}));
    let new_keys = [(CurrentFamily::Event, new.id.as_str())];
    let accepted = attempt(&f, &ledger, &new_keys, &new_keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &new)?;
        Ok(())
    })?;
    assert!(accepted.is_complete());
    f.db.connection()?.execute(
        "UPDATE audit_events SET data_json=json_set(data_json,'$.data.raw_tamper',1) WHERE audit_event_id=?1",
        [&new.id],
    )?;
    let denied = attempt(&f, &ledger, &new_keys, &new_keys, |_| Ok(()))?;
    assert!(!denied.is_complete());
    assert!(denied.facts().values().all(|fact| matches!(
        fact,
        crate::runtime_db::current_read_writes::CurrentAddressFact::Unprepared
    )));
    assert_eq!(qualified_count(&f)?, 0);
    Ok(())
}

#[test]
fn current_finalizer_failure_rolls_back_original_business_and_revision() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    f.db.connection()?.execute_batch(
        "CREATE TRIGGER current_finalizer_fault BEFORE INSERT ON current_read_qualification BEGIN SELECT RAISE(ABORT,'Current finalizer fault'); END;",
    )?;
    let event = AuditEvent::legacy("rollback", serde_json::json!({}));
    let keys = [(CurrentFamily::Event, event.id.as_str())];
    let error = attempt(&f, &ledger, &keys, &keys, |tx| {
        append_audit_event_tx(tx, Some(&f.scope.agent_id), &event)?;
        Ok(())
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("Current finalizer fault"));
    assert_eq!(qualified_count(&f)?, 0);
    let connection = f.db.connection()?;
    let counts: (usize, usize) = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_read_revisions)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(counts, (0, 0));
    Ok(())
}

#[test]
fn current_empty_ack_keeps_revision_and_native_phase_remains_cumulative() -> Result<()> {
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let ack = attempt(&f, &ledger, &[], &[], |_| Ok(()))?;
    assert_eq!(ack.base_revision, 0);
    assert_eq!(ack.revision, 0);
    assert!(ack.is_empty());
    assert!(ack.is_complete());
    f.db.transaction(|tx| {
        let mut native = NativeInputWriteProof::prepare_tx(
            tx, &f.scope, &ledger, (4 * 1024 * 1024, 4096), None, |_| Ok(()),
        )?.unwrap();
        native.with_input_phase(tx, &ledger, None, |material| {
            let before = material.remaining_limits();
            let current = OriginalCurrentWrites::prepare(tx, &f.scope, material, |_| Ok(()))?
                .unwrap();
            let after = material.remaining_limits();
            assert!(after.0 < before.0 && after.1 < before.1);
            let error = material.observe_native_footprint(after.0, after.1).unwrap_err();
            assert!(error.downcast_ref::<crate::runtime_db::input_evidence_snapshot::InputEvidenceSnapshotError>().is_some());
            drop(current);
            Ok(())
        })?;
        Ok(())
    })
}
