use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "agent-a".into(),
        identity_incarnation: 1,
    }
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn id_cost(id: &str) -> (u64, usize) {
    // Independent JSON string shape: one slot, including escaped wire bytes.
    let bytes = serde_json::to_string(id).unwrap().len() as u64;
    ((bytes * 4 + 128 + 512) * 8, 17 * 8)
}
fn cell_cost() -> (u64, usize) {
    (std::mem::size_of::<String>() as u64 * 8, 8)
}
fn ambiguous_fixture() -> (MessageEnvelope, Vec<WaitConditionRecord>) {
    (
        task_result("task-a", Some("work-a")),
        ["wait-z", "wait-\"\\\n路", "wait-a"]
            .map(|id| task_wait(id, Some("work-a"), "task-a"))
            .into(),
    )
}
fn exhausted(error: &anyhow::Error) {
    assert!(
        format!("{error:#}")
            .to_lowercase()
            .contains("resourceexhausted"),
        "{error:#}"
    );
}

#[test]
fn streaming_matches_use_only_header_and_preserve_all_task_tiebreaks() -> Result<()> {
    let mut message = task_result("task-a", Some("work-a"));
    message.task_id = Some("task-a".into());
    let mut waits: Vec<_> = ["wait-z", "wait-b", "wait-a", "wait-foreign"]
        .map(|id| task_wait(id, Some("work-a"), "task-a"))
        .into();
    for wait in &mut waits {
        wait.created_at = waits_at();
    }
    waits[3].work_item_id = Some("foreign".into());
    waits[3].created_at -= chrono::Duration::days(1);
    waits[0].created_at += chrono::Duration::seconds(1);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    {
        let _active = phase.enter();
        assert_eq!(
            select_wait_to_trigger(&message, &[])?,
            WaitTriggerSelection::NoMatch
        );
        for candidates in [&waits[..1], waits.as_slice()] {
            let WaitTriggerSelection::Match { condition, wake } =
                select_wait_to_trigger(&message, candidates)?
            else {
                panic!("expected a borrowed match");
            };
            let index = if candidates.len() == 1 { 0 } else { 2 };
            assert!(std::ptr::eq(condition, &waits[index]));
            assert_eq!(wake.source, "task_result");
            let MessageOrigin::Task { task_id } = &message.origin else {
                unreachable!()
            };
            assert_eq!(wake.subject_ref.unwrap().as_ptr(), task_id.as_ptr());
        }
        message
            .source_refs
            .insert("wait_id".into(), waits[1].id.clone());
        let WaitTriggerSelection::Match { condition, .. } =
            select_wait_to_trigger(&message, &waits)?
        else {
            panic!("exact correlation must override task preference")
        };
        assert!(std::ptr::eq(condition, &waits[1]));
        message.source_refs.clear();
        message.turn_id = Some("turn-wait".into());
        assert_eq!(
            select_wait_to_trigger(&message, &waits)?,
            WaitTriggerSelection::NoMatch
        );
    }
    assert_eq!(phase.finish()?, header());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

fn waits_at() -> chrono::DateTime<Utc> {
    "2026-10-17T12:13:00Z".parse().unwrap()
}

#[test]
fn wake_subjects_borrow_message_and_condition_fields() -> Result<()> {
    let mut message = task_result("task-a", Some("work-a"));
    let mut wait = task_wait("wait-a", Some("work-a"), "task-a");
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    {
        let _active = phase.enter();
        message.kind = MessageKind::TimerTick;
        message.origin = MessageOrigin::Timer {
            timer_id: "task-a".into(),
        };
        wait.wake_sources = vec![WakeSource::Timer {
            wake_at: waits_at(),
        }];
        let MessageOrigin::Timer { timer_id } = &message.origin else {
            unreachable!()
        };
        assert_eq!(
            matching_wake_source(&message, &wait)
                .unwrap()
                .subject_ref
                .unwrap()
                .as_ptr(),
            timer_id.as_ptr()
        );

        message.kind = MessageKind::OperatorPrompt;
        message.origin = MessageOrigin::Operator {
            actor_id: Some("actor".into()),
            actor_display_name: None,
        };
        wait.wake_sources = vec![WakeSource::OperatorInput];
        let MessageOrigin::Operator { actor_id, .. } = &message.origin else {
            unreachable!()
        };
        assert_eq!(
            matching_wake_source(&message, &wait)
                .unwrap()
                .subject_ref
                .unwrap()
                .as_ptr(),
            actor_id.as_ref().unwrap().as_ptr()
        );

        message.kind = MessageKind::CallbackEvent;
        message.origin = MessageOrigin::System {
            subsystem: "callback".into(),
        };
        message
            .source_refs
            .insert("external_trigger_id".into(), "trigger-a".into());
        wait.wake_sources = vec![WakeSource::ExternalIngress {
            external_trigger_id: Some("trigger-a".into()),
        }];
        assert_eq!(
            matching_wake_source(&message, &wait)
                .unwrap()
                .subject_ref
                .unwrap()
                .as_ptr(),
            message.source_refs["external_trigger_id"].as_ptr()
        );

        message.kind = MessageKind::SystemTick;
        message.metadata =
            Some(serde_json::json!({"wake_hint": {"external_trigger_id": "trigger-a"}}));
        message
            .source_refs
            .insert("wait_id".into(), wait.id.clone());
        let hint = message.metadata.as_ref().unwrap()["wake_hint"]["external_trigger_id"]
            .as_str()
            .unwrap();
        assert_eq!(
            matching_wake_source(&message, &wait)
                .unwrap()
                .subject_ref
                .unwrap()
                .as_ptr(),
            hint.as_ptr()
        );
        wait.wake_sources = vec![WakeSource::SystemTick];
        let MessageOrigin::System { subsystem } = &message.origin else {
            unreachable!()
        };
        assert_eq!(
            matching_wake_source(&message, &wait)
                .unwrap()
                .subject_ref
                .unwrap()
                .as_ptr(),
            subsystem.as_ptr()
        );

        message.origin = MessageOrigin::System {
            subsystem: "wait_condition_recheck".into(),
        };
        message.delivery_surface = Some(MessageDeliverySurface::RuntimeSystem);
        let recheck = waits_at().to_rfc3339();
        wait.continuation = Some(serde_json::json!({"recheck_at": recheck}));
        message.metadata = Some(serde_json::json!({"wait_condition_recheck": {
            "wait_id": wait.id, "recheck_at": recheck,
        }}));
        let wake = matching_wake_source(&message, &wait).unwrap();
        assert_eq!(wake.source, "wait_recheck");
        assert_eq!(wake.subject_ref.unwrap().as_ptr(), wait.id.as_ptr());
    }
    assert_eq!(phase.finish()?, header());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn owning_selectors_share_exact_remainder_and_last_phase_holder() -> Result<()> {
    let (mut message, waits) = ambiguous_fixture();
    let stale_id = "stale-\"\\\n路";
    let expected = waits
        .iter()
        .fold(add(header(), id_cost(stale_id)), |total, wait| {
            add(total, add(id_cost(&wait.id), cell_cost()))
        });
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), expected.0, expected.1)?;
    let holders;
    {
        let _active = phase.enter();
        message
            .source_refs
            .insert("wait_id".into(), stale_id.into());
        let stale = select_wait_to_trigger(&message, &waits)?;
        assert_eq!(
            stale,
            WaitTriggerSelection::StaleExact {
                wait_id: stale_id.into()
            }
        );
        message.source_refs.clear();
        let ambiguous = select_wait_to_trigger(&message, &waits)?;
        assert_eq!(
            ambiguous,
            WaitTriggerSelection::Ambiguous {
                wait_ids: waits.iter().map(|wait| wait.id.clone()).collect(),
            }
        );
        assert_eq!(phase.finish()?, expected);
        let guard = ResourceReadMaterial::active_guard()?.unwrap();
        holders = (guard.clone(), guard);
        exhausted(&select_wait_to_trigger(&message, &waits).unwrap_err());
        assert!(phase.finish().is_err());
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, expected.0);
    drop(holders.0);
    assert_eq!(ledger.report()?.observed_bytes, expected.0);
    drop(holders.1);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn every_owning_selector_gate_refuses_and_latches_original_phase() -> Result<()> {
    let (mut message, waits) = ambiguous_fixture();
    for stale in [true, false] {
        let gates = if stale {
            message.source_refs.insert("wait_id".into(), "stale".into());
            vec![id_cost("stale")]
        } else {
            message.source_refs.clear();
            waits
                .iter()
                .flat_map(|wait| [id_cost(&wait.id), cell_cost()])
                .collect()
        };
        let mut prefix = header();
        for next in gates {
            for bytes_bound in [true, false] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    if bytes_bound {
                        prefix.0 + next.0 - 1
                    } else {
                        1024 * 1024
                    },
                    if bytes_bound {
                        8192
                    } else {
                        prefix.1 + next.1 - 1
                    },
                )?;
                {
                    let _active = phase.enter();
                    for _ in 0..2 {
                        exhausted(&select_wait_to_trigger(&message, &waits).unwrap_err());
                        assert!(phase.finish().is_err());
                        assert!(ResourceReadMaterial::current_or_new(&ledger, &scope()).is_err());
                        assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                    }
                }
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
            prefix = add(prefix, next);
        }
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[test]
fn unphased_selection_preserves_ambiguity_and_checks_every_task_candidate() -> Result<()> {
    let (mut message, mut waits) = ambiguous_fixture();
    message.task_id = Some("task-a".into());
    waits[2].kind = WaitConditionKind::External;
    let WaitTriggerSelection::Ambiguous { wait_ids } = select_wait_to_trigger(&message, &waits)?
    else {
        panic!("a later non-task candidate must preserve ambiguity");
    };
    assert_eq!(
        wait_ids,
        waits.iter().map(|wait| wait.id.clone()).collect::<Vec<_>>()
    );
    waits[2].status = WaitConditionStatus::Cancelled;
    assert!(matches!(
        select_wait_to_trigger(&message, &waits)?,
        WaitTriggerSelection::Match { .. }
    ));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
