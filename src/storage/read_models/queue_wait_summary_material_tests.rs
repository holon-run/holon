use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    types::{ExternalWaitRecoverability, WaitConditionSummary, WakeSource, WorkItemPlanStatus},
    work_item_scheduling::{
        derive_queue_work_item_scheduling, derive_work_item_scheduling, WorkItemSchedulingFacts,
    },
};
use std::{cell::Cell, sync::Arc};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "queue-wait-summary".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn shape(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            2 + fields.len() * 2
                + fields.len().saturating_sub(1)
                + fields.values().map(shape).sum::<usize>()
        }
        serde_json::Value::Array(values) => {
            2 + values.len().saturating_sub(1) + values.iter().map(shape).sum::<usize>()
        }
        serde_json::Value::String(_) => 1,
        _ => 0,
    }
}

// An independent JSON-tree oracle for exactly the retained owning fields.
// This uses the old consuming conversion, not the new borrowed conversion/gate.
fn clone_cost(wait: &WaitConditionRecord) -> Result<(u64, usize)> {
    let summary = WaitConditionSummary::from(wait.clone());
    let fields = serde_json::json!([
        summary.id,
        summary.work_item_id,
        summary.source,
        summary.subject_ref,
        summary.waiting_for,
        summary.wake_sources,
        summary.no_fallback_reason,
        summary.continuation,
    ]);
    let cells = shape(&fields);
    Ok((
        (serde_json::to_vec(&fields)?.len() as u64 * 4 + cells as u64 * 128 + 512) * 8,
        (cells + 16) * 8,
    ))
}

fn gates(waits: &[WaitConditionRecord]) -> Result<Vec<(&'static str, (u64, usize))>> {
    if waits.is_empty() {
        return Ok(vec![]);
    }
    let count = waits.len();
    let mut gates = vec![
        (
            "queue wait references admission",
            (
                (count * std::mem::size_of::<(usize, &WaitConditionRecord)>()) as u64 * 8,
                count * 8,
            ),
        ),
        (
            "queue wait summary slots admission",
            (
                (count * std::mem::size_of::<WaitConditionSummary>()) as u64 * 8,
                count * 8,
            ),
        ),
    ];
    let mut sorted = waits.to_vec();
    sorted.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    for wait in sorted {
        gates.push(("queue wait summary clone admission", clone_cost(&wait)?));
    }
    Ok(gates)
}

pub(super) fn cost(waits: &[WaitConditionRecord]) -> Result<(u64, usize)> {
    Ok(gates(waits)?
        .into_iter()
        .fold((0, 0), |sum, (_, next)| add(sum, next)))
}

fn item() -> WorkItemRecord {
    let mut item = WorkItemRecord::new("web", "summary 汉字\0🦀", WorkItemState::Open);
    item.id = "work-summary".into();
    item.plan_status = WorkItemPlanStatus::Ready;
    item
}

fn waits(count: usize) -> Vec<WaitConditionRecord> {
    (0..count)
        .rev()
        .map(|index| {
            let now = "2026-10-01T00:00:00Z".parse().unwrap();
            WaitConditionRecord {
                id: format!("wait-{:03}", index % 3),
                agent_id: "not-returned".into(),
                work_item_id: Some("work-summary".into()),
                status: WaitConditionStatus::Active,
                kind: WaitConditionKind::External,
                source: Some("source \"[]{}\"".into()),
                subject_ref: Some(format!("subject-{index}")),
                waiting_for: "wait 汉字\0🦀".into(),
                wake_sources: vec![WakeSource::ExternalIngress {
                    external_trigger_id: Some(format!("trigger-{index}")),
                }],
                continuation: Some(match index % 5 {
                    0 => serde_json::json!({"no_fallback_reason": " no upstream 汉字 "}),
                    1 => serde_json::json!({"explicit_no_fallback_reason": "\t",
                    "recoverability": {"kind": "no_fallback", "reason": "nested 🦀"}}),
                    2 => {
                        serde_json::json!({"fallback_source": "durable", "body": ["\"[]{}", null]})
                    }
                    3 => serde_json::json!({}),
                    _ => serde_json::json!({"recovery": {"kind": "recoverable"}}),
                }),
                created_at: now,
                updated_at: now,
                expires_at: None,
                resolved_at: None,
                cancelled_at: None,
                turn_id: Some("ignored-turn".into()),
                trigger_message_id: Some("ignored-message".into()),
                triggered_at: None,
            }
        })
        .collect()
}

#[test]
fn summary_matches_public_api_stable_equal_keys_filtering_and_recoverability() -> Result<()> {
    let record = item();
    let mut conditions = waits(67);
    let mut foreign = waits(1).pop().unwrap();
    foreign.work_item_id = Some("foreign".into());
    conditions.insert(0, foreign.clone());
    foreign.work_item_id = None;
    conditions.push(foreign);
    let deliveries = BTreeMap::from([
        ("trigger-0".into(), "2026-10-03T00:00:00Z".parse()?),
        ("trigger-1".into(), "2026-10-02T00:00:00Z".parse()?),
    ]);
    for state in [
        WorkItemState::Open,
        WorkItemState::Completing,
        WorkItemState::Completed,
    ] {
        for current in [false, true] {
            for yielded in [false, true] {
                let mut record = record.clone();
                record.state = state.clone();
                let expected = derive_work_item_scheduling(WorkItemSchedulingFacts {
                    work_item: &record,
                    is_current: current,
                    is_yielded: yielded,
                    active_wait_conditions: &conditions,
                    trigger_delivery_by_id: &deliveries,
                });
                let actual = derive_queue_work_item_scheduling(
                    record,
                    current,
                    yielded,
                    &conditions,
                    &|id| deliveries.get(id).copied(),
                )?;
                assert_eq!(actual, expected);
                assert_eq!(
                    serde_json::to_value(&actual)?,
                    serde_json::to_value(&expected)?
                );
            }
        }
    }
    for wait in waits(5) {
        let index: usize = wait
            .subject_ref
            .as_ref()
            .unwrap()
            .strip_prefix("subject-")
            .unwrap()
            .parse()?;
        let summary = WaitConditionSummary::from(&wait);
        let (recovery, reason) = match index {
            0 => (
                ExternalWaitRecoverability::ExplicitNoFallback,
                Some(" no upstream 汉字 "),
            ),
            1 => (
                ExternalWaitRecoverability::ExplicitNoFallback,
                Some("nested 🦀"),
            ),
            3 => (ExternalWaitRecoverability::Weak, None),
            _ => (ExternalWaitRecoverability::Recoverable, None),
        };
        assert_eq!(summary.external_recoverability, Some(recovery));
        assert_eq!(summary.no_fallback_reason.as_deref(), reason);
        assert_eq!(summary, WaitConditionSummary::from(wait.clone()));
        assert_ne!(summary.id.as_ptr(), wait.id.as_ptr());
        assert_ne!(summary.waiting_for.as_ptr(), wait.waiting_for.as_ptr());
        assert_eq!(wait.no_fallback_reason_ref(), reason);
    }
    Ok(())
}

#[test]
fn summary_exact_material_retains_only_returned_fields_and_last_holder() -> Result<()> {
    for count in [1, 9, 65] {
        let conditions = waits(count);
        let fixture = item();
        let expected =
            derive_queue_work_item_scheduling(fixture.clone(), false, false, &conditions, &|_| {
                None
            })?;
        let total = add(header(), cost(&conditions)?);
        for ignored_size in [0, 4096] {
            let mut conditions = conditions.clone();
            for wait in &mut conditions {
                wait.agent_id = "ignored 汉字\0🦀".repeat(ignored_size);
                wait.turn_id = Some("ignored".repeat(ignored_size));
                wait.trigger_message_id = Some("ignored".repeat(ignored_size));
                wait.id.reserve(100);
                wait.waiting_for.reserve(133);
            }
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
            let holder = phase.guard()?;
            let original = fixture.clone();
            let pointer = original.objective.as_ptr();
            let row = {
                let _entry = phase.enter();
                let row = derive_queue_work_item_scheduling(
                    original,
                    false,
                    false,
                    &conditions,
                    &|_| None,
                )?;
                assert!(Arc::ptr_eq(
                    &holder,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
                row
            };
            assert_eq!(row, expected);
            assert_eq!(row.objective.as_ptr(), pointer);
            assert_eq!(row.active_wait_conditions.capacity(), count);
            assert_eq!(phase.finish()?, total);
            let last = holder.clone();
            drop((row, phase, holder));
            assert_eq!(ledger.report()?.observed_bytes, total.0);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn summary_each_gate_rejects_before_clone_or_lookup_and_latches_original_phase() -> Result<()> {
    let conditions = waits(2);
    let mut prefix = header();
    for (index, (context, added)) in gates(&conditions)?.into_iter().enumerate() {
        let total = add(prefix, added);
        for byte_short in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                total.0 - u64::from(byte_short),
                total.1 - usize::from(!byte_short),
            )?;
            let holder = phase.guard()?;
            let calls = Cell::new(0);
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &holder,
                    &ResourceReadMaterial::active_guard()?.unwrap()
                ));
                for attempt in 0..2 {
                    let error = derive_queue_work_item_scheduling(
                        item(),
                        false,
                        false,
                        &conditions,
                        &|_| {
                            calls.set(calls.get() + 1);
                            None
                        },
                    )
                    .unwrap_err();
                    assert_eq!(
                        error.to_string(),
                        if attempt == 0 {
                            context
                        } else {
                            "queue wait references admission"
                        }
                    );
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(calls.get(), index.saturating_sub(2));
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                    assert!(phase.finish().is_err());
                    assert!(ResourceReadMaterial::active_guard().is_err());
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        prefix = total;
    }
    Ok(())
}

#[test]
fn empty_or_foreign_summaries_have_no_cost_even_on_failed_phase() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    let mut conditions = waits(3);
    for wait in &mut conditions {
        wait.work_item_id = None;
    }
    let _entry = phase.enter();
    for failed in [false, true] {
        if failed {
            assert!(resource_io::charge_owned_metadata(1, 0).is_err());
        }
        for waits in [&[][..], conditions.as_slice()] {
            let row = derive_queue_work_item_scheduling(item(), false, false, waits, &|_| {
                panic!("foreign lookup")
            })?;
            assert!(row.active_wait_conditions.is_empty());
            assert_eq!(row.active_wait_conditions.capacity(), 0);
            assert!(!row.has_active_waits);
            assert_eq!(ledger.report()?.observed_bytes, header().0);
        }
        if failed {
            assert!(phase.finish().is_err());
        } else {
            assert_eq!(phase.finish()?, header());
        }
    }
    drop((_entry, phase));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn shared_queue_summary_failure_returns_no_partial_queue() -> Result<()> {
    let mut conditions = waits(1);
    conditions[0].kind = WaitConditionKind::Task;
    let slots = (
        std::mem::size_of::<WorkItemSchedulingProjection>() as u64 * 8,
        8,
    );
    let reference_slots = gates(&conditions)?[0].1;
    let prefix = add(header(), slots);
    let total = add(prefix, reference_slots);
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            total.0 - u64::from(bytes),
            total.1 - usize::from(!bytes),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            for attempt in 0..2 {
                let row = item();
                let error = derive_context_work_queue(
                    None,
                    QueueLatestWork::from([(row.id.clone(), row)]),
                    vec![],
                    conditions.clone(),
                    vec![],
                )
                .unwrap_err();
                assert_eq!(
                    error.to_string(),
                    if attempt == 0 {
                        "queue wait references admission"
                    } else {
                        "queue items slots admission"
                    }
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, prefix.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
