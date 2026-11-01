use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use std::cell::Cell;

const TIMER: &str = r#"{"id":"body-timer","agent_id":"body-agent","created_at":"2026-10-01T00:00:00Z","duration_ms":1000,"interval_ms":1000,"repeat":true,"status":"active","summary":"{,:} \"x\" \\ 路","next_fire_at":"2026-10-01T00:00:00Z","last_fired_at":null,"fire_count":0}"#;
const MESSAGE: &str = r#"{"id":"body-wake","agent_id":"body-agent","created_at":"2026-10-01T00:00:00Z","kind":"timer_tick","origin":{"kind":"timer","timer_id":"body-timer"},"authority_class":"runtime_instruction","priority":"next","body":{"type":"text","text":"{,:} \"x\" \\ 路"},"metadata":null,"correlation_id":null,"causation_id":null}"#;
const ID: &str = "drop-wake";

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "body-agent".into(),
        identity_incarnation: 1,
    }
}
fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn cost(bytes: usize, slots: usize) -> (u64, usize) {
    (
        (bytes as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots + 16) * 8,
    )
}
fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}
fn message() -> Result<MessageEnvelope> {
    Ok(serde_json::from_str(MESSAGE)?)
}

#[test]
fn timer_and_message_effects_charge_exact_transaction_and_last_holder() -> Result<()> {
    let timer: crate::types::TimerRecord = serde_json::from_str(TIMER)?;
    let message = message()?;
    assert_eq!(serde_json::to_string(&timer)?, TIMER);
    assert_eq!(serde_json::to_string(&message)?, MESSAGE);
    // Timer: eleven fields, six string values. Message: fifteen fields over
    // three objects, ten string values. Quoted punctuation is not structure.
    let body = add(
        add(cost(TIMER.len(), 40), cost(MESSAGE.len(), 58)),
        add(cost(MESSAGE.len(), 58), cost(11, 1)),
    );
    let exact = add(add(header(), header()), body);
    let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    let mut effects = PostCommitEffects::default();
    {
        let _active = resume_material(&transaction)?;
        effects.timers.push(repositories::clone_timer_body(&timer)?);
        push_wake_message_effects(&mut effects, &message)?;
        effects
            .removed_queue_message_ids
            .push(repositories::clone_timer_body(&ID.to_owned())?);
    }
    assert_eq!(phase.finish()?, exact);
    assert_eq!(effects.timers, vec![timer]);
    assert_eq!(effects.messages, vec![message.clone()]);
    assert_eq!(effects.queued_messages, vec![message]);
    assert_eq!(effects.removed_queue_message_ids, vec![ID]);
    effects
        .scheduling_material_budget_views
        .push(transaction.guard()?);
    let last = effects.scheduling_material_budget_views.clone();
    drop((transaction, phase));
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(effects);
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn each_wake_effect_copy_refuses_before_publication_and_keeps_failure() -> Result<()> {
    for copy in 0..2 {
        for bytes_bound in [false, true] {
            let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let one = cost(MESSAGE.len(), 58);
            let limit = add(header(), (one.0 * (copy + 1), one.1 * (copy as usize + 1)));
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if bytes_bound {
                    limit.0 - 1
                } else {
                    16 * 1024 * 1024
                },
                if bytes_bound { 65536 } else { limit.1 - 1 },
            )?;
            let mut effects = PostCommitEffects::default();
            {
                let _active = resume_material(&phase)?;
                let error = push_wake_message_effects(&mut effects, &message()?).unwrap_err();
                assert!(format!("{error:#}")
                    .to_lowercase()
                    .contains("resourceexhausted"));
                assert!(push_wake_message_effects(&mut effects, &message()?).is_err());
            }
            assert!(effects.messages.is_empty());
            assert!(effects.queued_messages.is_empty());
            assert!(resume_material(&phase).is_err());
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

struct CloneProbe<'a, T> {
    value: &'a T,
    clones: &'a Cell<usize>,
}
impl<T> Clone for CloneProbe<'_, T> {
    fn clone(&self) -> Self {
        self.clones.set(self.clones.get() + 1);
        Self {
            value: self.value,
            clones: self.clones,
        }
    }
}
impl<T: serde::Serialize> serde::Serialize for CloneProbe<'_, T> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}

fn refuses_without_cloning<T: serde::Serialize>(value: &T) -> Result<()> {
    for bytes_bound in [false, true] {
        let budget = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &budget,
            &scope(),
            if bytes_bound {
                header().0
            } else {
                16 * 1024 * 1024
            },
            if bytes_bound { 65536 } else { header().1 },
        )?;
        let outer = ResourceReadMaterial::new(&budget, &scope())?;
        let clones = Cell::new(0);
        {
            let _outer = outer.enter();
            let original = ResourceReadMaterial::active_guard()?.unwrap();
            {
                let _active = resume_material(&phase)?;
                assert!(repositories::clone_timer_body(&CloneProbe {
                    value,
                    clones: &clones
                })
                .is_err());
                assert!(repositories::clone_timer_body(&CloneProbe {
                    value,
                    clones: &clones
                })
                .is_err());
            }
            assert!(Arc::ptr_eq(
                &original,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
        }
        assert_eq!(clones.get(), 0);
        assert!(phase.finish().is_err());
        drop((phase, outer));
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn record_message_state_and_id_admit_before_clone_and_restore_caller_tls() -> Result<()> {
    refuses_without_cloning(&serde_json::from_str::<crate::types::TimerRecord>(TIMER)?)?;
    refuses_without_cloning(&message()?)?;
    let mut state = AgentState::new("body-agent");
    state
        .attached_workspaces
        .push("workspace-with-owned-state".into());
    refuses_without_cloning(&state)?;
    refuses_without_cloning(&ID.to_owned())?;
    Ok(())
}

#[test]
fn legacy_typed_clone_does_not_create_detached_material() -> Result<()> {
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let message = message()?;
    assert_eq!(repositories::clone_timer_body(&message)?, message);
    let state = AgentState::new("body-agent");
    assert_eq!(repositories::clone_timer_body(&state)?, state);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}
