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

fn timer_fire(work_item_id: Option<&str>) -> MessageEnvelope {
    let mut message = task_result("timer-a", work_item_id);
    message.kind = MessageKind::TimerTick;
    message.origin = MessageOrigin::Timer {
        timer_id: "timer-a".into(),
    };
    message.task_id = None;
    message
}

fn timer_wait_at(
    id: &str,
    work_item_id: Option<&str>,
    at: chrono::DateTime<Utc>,
) -> WaitConditionRecord {
    let mut wait = task_wait(id, work_item_id, "timer-a");
    wait.kind = WaitConditionKind::Timer;
    wait.subject_ref = Some("timer-a".into());
    wait.wake_sources = vec![WakeSource::Timer { wake_at: at }];
    wait.created_at = at;
    wait
}

#[test]
fn stale_exact_correlation_owns_its_id_and_releases_the_phase_holder() -> Result<()> {
    let (mut message, waits) = ambiguous_fixture();
    let stale_id = "stale-\"\\\n\u{8def}";
    let expected = add(header(), id_cost(stale_id));
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), expected.0, expected.1)?;
    let holders;
    {
        let _active = phase.enter();
        message
            .source_refs
            .insert("wait_id".into(), stale_id.into());
        assert_eq!(
            select_wait_to_trigger(&message, &waits)?,
            WaitTriggerSelection::StaleExact {
                wait_id: stale_id.into()
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
fn multi_match_timer_selection_needs_only_the_header() -> Result<()> {
    let message = timer_fire(Some("work-a"));
    let at = waits_at();
    let mut waits = vec![
        timer_wait_at("wait-a", Some("work-a"), at),
        timer_wait_at("wait-b", Some("work-a"), at + chrono::Duration::seconds(1)),
        timer_wait_at("wait-lifecycle", None, at - chrono::Duration::days(1)),
    ];
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    {
        let _active = phase.enter();
        let WaitTriggerSelection::Match { condition, wake } =
            select_wait_to_trigger(&message, &waits)?
        else {
            panic!("a multi-match timer fire must select one waiter");
        };
        // WorkItem scope wins over the older agent-lifecycle wait.
        assert_eq!(condition.id, "wait-a");
        assert_eq!(wake.source, "timer");
        // A later-discovered candidate cannot shadow an earlier one.
        waits[0].created_at = at + chrono::Duration::seconds(2);
        let WaitTriggerSelection::Match { condition, .. } =
            select_wait_to_trigger(&message, &waits)?
        else {
            panic!("a multi-match timer fire must select one waiter");
        };
        assert_eq!(condition.id, "wait-b");
        assert_eq!(phase.finish()?, header());
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn task_result_selection_prefers_the_exact_owner_and_never_charges_ids() -> Result<()> {
    let mut message = task_result("task-a", Some("work-a"));
    message.task_id = Some("task-a".into());
    let exact = task_wait("wait-owner", Some("work-a"), "task-a");
    let mut older_foreign = task_wait("wait-foreign", Some("work-b"), "task-a");
    older_foreign.created_at = exact.created_at - chrono::Duration::days(1);
    let mut earlier_non_task = task_wait("wait-non-task", Some("work-a"), "task-a");
    earlier_non_task.kind = WaitConditionKind::External;
    earlier_non_task.created_at = exact.created_at - chrono::Duration::days(2);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), header().0, header().1)?;
    {
        let _active = phase.enter();
        let candidates = [earlier_non_task, older_foreign, exact];
        let WaitTriggerSelection::Match { condition, wake } =
            select_wait_to_trigger(&message, &candidates)?
        else {
            panic!("a task result must select its exact owner waiter");
        };
        assert_eq!(condition.id, "wait-owner");
        assert_eq!(wake.source, "task_result");
        assert_eq!(phase.finish()?, header());
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
