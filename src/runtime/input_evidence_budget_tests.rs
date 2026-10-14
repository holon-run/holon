use super::*;
use crate::runtime::resident_input_evidence::{
    EvidenceLocator, InputEvidenceFamily, InputEvidenceKey, PreparedInputEvidence,
};
use crate::types::TurnRecord;

fn scope(id: &str, incarnation: u64) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: id.into(),
        identity_incarnation: incarnation,
    }
}
fn limits() -> StateMemoryLimits {
    StateMemoryLimits {
        history_per_agent_bytes: 100,
        history_host_bytes: 150,
        pending_per_agent_count: 2,
        pending_per_agent_bytes: 10,
        pending_host_bytes: 15,
        record_limit: 32,
    }
}
fn ticket(
    ledger: &InputEvidenceBudgetLedger,
    scope: &InputEvidenceScope,
    id: &str,
    bytes: u64,
) -> PendingAdmissionTicket {
    match ledger.reserve_pending(scope, id, bytes).unwrap() {
        PendingAdmission::Reserved(ticket) => ticket,
        other => panic!("unexpected admission: {other:?}"),
    }
}
fn accepted(
    ledger: &InputEvidenceBudgetLedger,
    scope: &InputEvidenceScope,
    id: &str,
    bytes: u64,
) -> PendingAdmissionTicket {
    let ticket = ticket(ledger, scope, id, bytes);
    ledger.mark_attempt_started(&ticket).unwrap();
    ledger.mark_accepted(&ticket).unwrap();
    ticket
}
fn record(scope: &InputEvidenceScope, id: &str, bytes: usize) -> Arc<QualifiedInputEvidence> {
    Arc::new(QualifiedInputEvidence {
        attempt_turn_keys: None,
        locator: EvidenceLocator {
            scope: scope.clone(),
            key: InputEvidenceKey::new(InputEvidenceFamily::Turn, id),
            revision: 1,
            content_digest: format!("digest-{id}"),
        },
        record: PreparedInputEvidence::Turn(TurnRecord::new(&scope.agent_id, id, 1)),
        created_at: "2026-10-13T00:00:00Z".into(),
        order_sequence: None,
        turn_index: Some(1),
        owner: None,
        resident_bytes: bytes,
    })
}
#[test]
fn pending_count_bytes_and_cross_agent_host_admission_are_checked_before_charge() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let a = scope("a", 1);
    let b = scope("b", 1);
    accepted(&ledger, &a, "a1", 5);
    accepted(&ledger, &a, "a2", 5);
    assert!(matches!(
        ledger.reserve_pending(&a, "a3", 0),
        Err(InputEvidenceBudgetError::ResourceExhausted {
            resource: BudgetResource::PendingAgentCount,
            ..
        })
    ));
    accepted(&ledger, &b, "b1", 5);
    assert!(matches!(
        ledger.reserve_pending(&b, "b2", 1),
        Err(InputEvidenceBudgetError::ResourceExhausted {
            resource: BudgetResource::PendingHostBytes,
            ..
        })
    ));
    let report = ledger.report().unwrap();
    assert_eq!(report.pending.count, 3);
    assert_eq!(report.pending.body_bytes, 15);
    ledger.release_accepted(&a, &["a1".into()]).unwrap();
    assert!(matches!(
        ledger.reserve_pending(&b, "b2", 6),
        Err(InputEvidenceBudgetError::ResourceExhausted {
            resource: BudgetResource::PendingAgentBytes,
            ..
        })
    ));
    assert_eq!(ledger.report().unwrap().pending.body_bytes, 10);
}
#[test]
fn concurrent_agents_cannot_oversubscribe_host_pending_bytes() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let workers = (0..2)
        .map(|i| {
            let ledger = ledger.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                ledger.reserve_pending(&scope(&format!("agent-{i}"), 1), "message", 10)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(ledger.report().unwrap().pending.body_bytes, 10);
}
#[test]
fn exact_reentry_unknown_drop_and_positive_resolution_preserve_one_charge() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let scope = scope("a", 1);
    let original = ticket(&ledger, &scope, "m", 6);
    let continuation = original.clone();
    assert!(matches!(
        ledger.reserve_pending(&scope, "m", 6),
        Err(InputEvidenceBudgetError::NeedsReconcile { .. })
    ));
    ledger.mark_attempt_started(&original).unwrap();
    ledger.mark_unknown(&original).unwrap();
    drop(original);
    assert_eq!(ledger.report().unwrap().pending.body_bytes, 6);
    assert!(matches!(
        ledger.definitely_not_committed(&continuation),
        Err(InputEvidenceBudgetError::InvalidPendingPhase)
    ));
    assert!(ledger.release_accepted(&scope, &["m".into()]).is_err());
    ledger.mark_accepted(&continuation).unwrap();
    ledger.mark_accepted(&continuation).unwrap();
    assert!(matches!(
        ledger.reserve_pending(&scope, "m", 6).unwrap(),
        PendingAdmission::ExistingAccepted
    ));
    assert!(matches!(
        ledger.reserve_pending(&scope, "m", 5),
        Err(InputEvidenceBudgetError::ConflictingMessage { .. })
    ));
    let report = ledger.report().unwrap();
    assert_eq!(report.pending.count, 1);
    assert_eq!(report.pending.unresolved_count, 0);
    assert!(ledger.definitely_not_committed(&continuation).is_err());
    ledger
        .release_accepted(&scope, &["m".into(), "m".into()])
        .unwrap();
    assert_eq!(ledger.report().unwrap().pending.count, 0);
}
#[test]
fn negative_resolution_and_pre_sql_cancel_are_explicit_and_stale_ticket_cannot_release_retry() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let scope = scope("a", 1);
    let pre = ticket(&ledger, &scope, "pre", 2);
    ledger.definitely_not_committed(&pre).unwrap();
    let original = ticket(&ledger, &scope, "m", 6);
    ledger.mark_attempt_started(&original).unwrap();
    ledger.mark_unknown(&original).unwrap();
    ledger.resolve_not_committed(&original).unwrap();
    let retry = ticket(&ledger, &scope, "m", 6);
    assert!(matches!(
        ledger.definitely_not_committed(&original),
        Err(InputEvidenceBudgetError::TicketMismatch)
    ));
    ledger.definitely_not_committed(&retry).unwrap();
    assert_eq!(ledger.report().unwrap().pending.count, 0);
    let other = InputEvidenceBudgetLedger::new(limits()).unwrap();
    assert!(matches!(
        other.definitely_not_committed(&retry),
        Err(InputEvidenceBudgetError::TicketMismatch)
    ));
}
#[test]
fn accepted_queue_removal_validates_whole_compound_before_releasing() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let scope = scope("a", 1);
    accepted(&ledger, &scope, "accepted", 4);
    let unknown = ticket(&ledger, &scope, "unknown", 4);
    ledger.mark_attempt_started(&unknown).unwrap();
    ledger.mark_unknown(&unknown).unwrap();
    assert!(ledger
        .release_accepted(&scope, &["accepted".into(), "unknown".into()])
        .is_err());
    assert_eq!(ledger.report().unwrap().pending.count, 2);
}
#[test]
fn bootstrap_over_limit_retains_actual_backlog_and_does_not_rewrite_unknown() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let scope = scope("a", 1);
    let actual = (0..3)
        .map(|i| PendingBodyCharge {
            message_id: format!("m{i}"),
            body_bytes: 5,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        ledger.install_actual_pending(&scope, &actual),
        Err(InputEvidenceBudgetError::ResourceExhausted { .. })
    ));
    let report = ledger.report().unwrap();
    assert_eq!(report.pending.count, 3);
    assert_eq!(report.pending.body_bytes, 15);
    assert_eq!(report.pending.unresolved_count, 0);
    assert!(ledger.reserve_pending(&scope, "new", 0).is_err());
    ledger.install_actual_pending(&scope, &actual[..1]).unwrap();
    assert_eq!(ledger.report().unwrap().pending.count, 1);
    let pending = ticket(&ledger, &scope, "inflight", 1);
    ledger.mark_attempt_started(&pending).unwrap();
    ledger.mark_unknown(&pending).unwrap();
    assert!(matches!(
        ledger.install_actual_pending(&scope, &[]),
        Err(InputEvidenceBudgetError::ScopeHasUnresolvedAttempt)
    ));
    assert_eq!(ledger.report().unwrap().pending.count, 2);
}
#[test]
fn new_incarnation_does_not_inherit_or_release_old_scope_charges() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let old = scope("a", 1);
    let new = scope("a", 2);
    accepted(&ledger, &old, "same-id", 5);
    accepted(&ledger, &new, "same-id", 5);
    ledger.install_actual_pending(&new, &[]).unwrap();
    let report = ledger.report().unwrap();
    assert_eq!(report.pending.count, 1);
    assert_eq!(report.scopes[0].scope, old);
    assert!(matches!(
        ledger.reserve_pending(&old, "same-id", 5).unwrap(),
        PendingAdmission::ExistingAccepted
    ));
}
#[test]
fn shared_allocations_are_physical_once_categories_overlap_and_pins_prevent_reclaim() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let scope = scope("a", 1);
    let item = record(&scope, "turn", 90);
    let history = ledger
        .register_view(
            &scope,
            InputMemoryCategory::History,
            &[item.clone(), item.clone()],
        )
        .unwrap();
    let pins = ledger
        .register_view(&scope, InputMemoryCategory::Pins, &[item.clone()])
        .unwrap();
    let report = ledger.report().unwrap();
    assert_eq!(
        report.physical_records,
        AllocationUsage {
            allocations: 1,
            bytes: 90
        }
    );
    assert_eq!(report.categories[&InputMemoryCategory::History].bytes, 90);
    assert_eq!(report.categories[&InputMemoryCategory::Pins].bytes, 90);
    assert_eq!(report.reclaimable_history.bytes, 0);
    let shared = pins.clone();
    drop(pins);
    assert_eq!(
        ledger.report().unwrap().categories[&InputMemoryCategory::Pins].bytes,
        90
    );
    drop(shared);
    assert_eq!(ledger.report().unwrap().reclaimable_history.bytes, 90);
    drop(history);
    assert_eq!(ledger.report().unwrap().retained_unclassified.bytes, 90);
    drop(item);
    assert_eq!(ledger.report().unwrap().physical_records.bytes, 0);
}
#[test]
fn new_root_does_not_remove_old_root_membership_and_reports_do_not_pin_records() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let scope = scope("a", 1);
    let old = record(&scope, "old", 60);
    let new = record(&scope, "new", 70);
    let old_view = ledger
        .register_view(&scope, InputMemoryCategory::History, &[old.clone()])
        .unwrap();
    let new_view = ledger
        .register_view(&scope, InputMemoryCategory::History, &[new.clone()])
        .unwrap();
    assert_eq!(
        ledger.report().unwrap().categories[&InputMemoryCategory::History].bytes,
        130
    );
    assert!(matches!(
        ledger.check_history_budget(),
        Err(InputEvidenceBudgetError::ResourceExhausted {
            resource: BudgetResource::HistoryAgentBytes,
            ..
        })
    ));
    let report = ledger.report().unwrap();
    let weak = Arc::downgrade(&old);
    drop(old);
    assert!(weak.upgrade().is_none());
    assert_eq!(report.physical_records.bytes, 130);
    assert_eq!(ledger.report().unwrap().physical_records.bytes, 70);
    drop(old_view);
    drop(new_view);
    drop(new);
    assert_eq!(ledger.report().unwrap().observed_bytes, 0);
}
#[test]
fn host_history_limit_deduplicates_shared_views_and_remains_separate_from_current() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let a = scope("a", 1);
    let b = scope("b", 1);
    let ar = record(&a, "a", 80);
    let br = record(&b, "b", 80);
    let _a = ledger
        .register_view(&a, InputMemoryCategory::History, &[ar.clone()])
        .unwrap();
    let _b = ledger
        .register_view(&b, InputMemoryCategory::History, &[br.clone()])
        .unwrap();
    let _shared = ledger
        .register_view(&a, InputMemoryCategory::History, &[ar.clone()])
        .unwrap();
    assert!(matches!(
        ledger.check_history_budget(),
        Err(InputEvidenceBudgetError::ResourceExhausted {
            resource: BudgetResource::HistoryHostBytes,
            ..
        })
    ));
    let current = record(&a, "current", 1_000_000);
    let _current = ledger
        .register_view(&a, InputMemoryCategory::Current, &[current.clone()])
        .unwrap();
    assert_eq!(
        ledger.report().unwrap().categories[&InputMemoryCategory::History].bytes,
        160
    );
}
#[test]
fn prompt_owned_allocation_guard_is_shared_and_reports_payload_metadata_coverage() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let scope = scope("a", 1);
    let guard = ledger
        .register_owned_bytes(
            &scope,
            InputMemoryCategory::PreparedPrompt,
            OwnedBytesEstimate {
                payload_bytes: 100,
                metadata_estimate_bytes: 20,
            },
        )
        .unwrap();
    let clone = guard.clone();
    let report = ledger.report().unwrap();
    assert_eq!(report.owned.payload_bytes, 100);
    assert_eq!(report.owned.metadata_estimate_bytes, 20);
    assert_eq!(report.observed_bytes, 120);
    assert_eq!(
        report.categories[&InputMemoryCategory::PreparedPrompt].allocations,
        1
    );
    drop(guard);
    assert_eq!(ledger.report().unwrap().observed_bytes, 120);
    drop(clone);
    assert_eq!(ledger.report().unwrap().observed_bytes, 0);
}
#[test]
fn invalid_limits_scopes_and_cross_scope_views_are_rejected_without_charge() {
    let mut bad = limits();
    bad.pending_host_bytes = 0;
    assert!(InputEvidenceBudgetLedger::new(bad).is_err());
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    assert!(ledger.reserve_pending(&scope("a", 0), "m", 1).is_err());
    assert!(ledger.reserve_pending(&scope("a", 1), "", 1).is_err());
    assert!(ledger
        .register_view(
            &scope("a", 1),
            InputMemoryCategory::Current,
            &[record(&scope("a", 2), "x", 10)]
        )
        .is_err());
    assert_eq!(ledger.report().unwrap().observed_bytes, 0);
}

#[test]
fn queue_observation_deduplicates_shared_bodies_and_does_not_pin_them() {
    use crate::types::{
        AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    };
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let input_scope = scope("a", 1);
    let message = Arc::new(MessageEnvelope::new(
        "a",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "x".repeat(16 * 1024),
        },
    ));
    let weak = Arc::downgrade(&message);
    let bytes = serde_json::to_vec(message.as_ref()).unwrap().len() as u64;
    let current = ledger
        .register_queue_view(&input_scope, InputMemoryCategory::Current, [&message])
        .unwrap();
    let candidate = ledger
        .register_queue_view(
            &input_scope,
            InputMemoryCategory::Candidate,
            [&message, &message],
        )
        .unwrap();
    let report = ledger.report().unwrap();
    assert_eq!(report.physical_records.allocations, 1);
    assert_eq!(report.observed_bytes, bytes);
    assert_eq!(
        report.categories[&InputMemoryCategory::Current].bytes,
        bytes
    );
    assert_eq!(
        report.categories[&InputMemoryCategory::Candidate].bytes,
        bytes
    );
    assert!(ledger
        .register_queue_view(&scope("a", 2), InputMemoryCategory::Current, [&message])
        .is_err());
    drop(current);
    drop(candidate);
    assert_eq!(ledger.report().unwrap().retained_unclassified.bytes, bytes);
    drop(message);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report().unwrap().observed_bytes, 0);
}

#[test]
fn cold_preflight_does_not_charge_conservative_bytes() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let a = scope("a", 1);
    let b = scope("b", 1);
    accepted(&ledger, &a, "a", 3);
    accepted(&ledger, &b, "b", 5);
    ledger.preflight_actual_pending(&a, 1, 9).unwrap();
    assert_eq!(ledger.report().unwrap().pending.body_bytes, 8);
    assert!(matches!(
        ledger.preflight_actual_pending(&a, 1, 11),
        Err(InputEvidenceBudgetError::ResourceExhausted {
            resource: BudgetResource::PendingAgentBytes,
            ..
        })
    ));
    assert_eq!(ledger.report().unwrap().pending.body_bytes, 8);
    ledger
        .install_actual_pending(
            &a,
            &[PendingBodyCharge {
                message_id: "a".into(),
                body_bytes: 3,
            }],
        )
        .unwrap();
    assert_eq!(ledger.report().unwrap().pending.body_bytes, 8);
}

#[test]
fn cold_preflight_does_not_replace_an_unknown_pending_ticket() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let a = scope("a", 1);
    let original = ticket(&ledger, &a, "a", 3);
    ledger.mark_attempt_started(&original).unwrap();
    ledger.mark_unknown(&original).unwrap();
    assert!(matches!(
        ledger.preflight_actual_pending(&a, 1, 3),
        Err(InputEvidenceBudgetError::ScopeHasUnresolvedAttempt)
    ));
    assert_eq!(ledger.report().unwrap().pending.unresolved_count, 1);
}

#[test]
fn original_ticket_group_rejects_invalid_member_without_partial_release() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let owner = scope("agent-a", 1);
    let first = ticket(&ledger, &owner, "first", 3);
    let second = ticket(&ledger, &owner, "second", 3);
    ledger
        .resolve_original_tickets(&[&first, &second], PendingTicketResolution::Started)
        .unwrap();
    ledger.mark_accepted(&second).unwrap();
    assert!(ledger
        .resolve_original_tickets(
            &[&first, &second],
            PendingTicketResolution::FreshNotCommitted
        )
        .is_err());
    let report = ledger.report().unwrap();
    assert_eq!(report.pending.count, 2);
    assert_eq!(report.pending.unresolved_count, 1);
    ledger.mark_unknown(&first).unwrap();
    ledger
        .resolve_original_tickets(&[&first], PendingTicketResolution::FreshNotCommitted)
        .unwrap();
    assert_eq!(ledger.report().unwrap().pending.count, 1);
    assert_eq!(ledger.report().unwrap().pending.unresolved_count, 0);
}

#[test]
fn current_pending_reload_keeps_the_original_accepted_ticket_nonce() {
    let ledger = InputEvidenceBudgetLedger::new(limits()).unwrap();
    let owner = scope("agent-a", 1);
    let original = accepted(&ledger, &owner, "retained", 3);
    ledger
        .install_actual_pending(
            &owner,
            &[PendingBodyCharge {
                message_id: "retained".into(),
                body_bytes: 3,
            }],
        )
        .unwrap();
    ledger
        .resolve_original_tickets(&[&original], PendingTicketResolution::Accepted)
        .unwrap();
    assert_eq!(ledger.report().unwrap().pending.count, 1);
    ledger
        .release_accepted(&owner, &["retained".into()])
        .unwrap();
    assert_eq!(ledger.report().unwrap().pending.count, 0);
}

#[test]
fn host_owned_metadata_needs_no_fake_identity_and_lives_until_last_guard() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(limits())?;
    let guard = ledger.register_host_owned_bytes(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 11,
            metadata_estimate_bytes: 37,
        },
    )?;
    let held = guard.clone();
    let report = ledger.report()?;
    assert!(report.scopes.is_empty());
    assert_eq!(report.pending.count, 0);
    assert_eq!(report.host_owned.payload_bytes, 11);
    assert_eq!(report.host_owned.metadata_estimate_bytes, 37);
    assert_eq!(report.owned.total()?, 48);
    assert_eq!(report.observed_bytes, 48);
    assert_eq!(report.categories[&InputMemoryCategory::Candidate].bytes, 48);
    drop(guard);
    assert_eq!(ledger.report()?.observed_bytes, 48);
    drop(held);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
#[test]
fn host_owned_metadata_does_not_change_scoped_pending_or_history_responsibility() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(limits())?;
    let s = scope("real-owner", 7);
    let _accepted = accepted(&ledger, &s, "original-accepted", 5);
    let scoped = ledger.register_owned_bytes(
        &s,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 3,
            metadata_estimate_bytes: 9,
        },
    )?;
    let before = ledger.report()?;
    let host = ledger.register_host_owned_bytes(
        InputMemoryCategory::Candidate,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: 81,
        },
    )?;
    let current = ledger.report()?;
    assert_eq!(current.scopes.len(), 1);
    assert_eq!(current.scopes[0].scope, s);
    assert_eq!(
        current.scopes[0].owned.total()?,
        before.scopes[0].owned.total()?
    );
    assert_eq!(current.pending.count, before.pending.count);
    assert_eq!(current.pending.body_bytes, before.pending.body_bytes);
    assert_eq!(
        current.reclaimable_history.bytes,
        before.reclaimable_history.bytes
    );
    assert_eq!(current.observed_bytes, before.observed_bytes + 81);
    drop(host);
    assert_eq!(ledger.report()?.observed_bytes, before.observed_bytes);
    drop(scoped);
    assert_eq!(ledger.report()?.pending.count, 1);
    assert!(ledger
        .register_host_owned_bytes(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::MAX,
                metadata_estimate_bytes: 1
            }
        )
        .is_err());
    assert_eq!(ledger.report()?.host_owned.total()?, 0);
    Ok(())
}
