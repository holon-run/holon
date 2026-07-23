use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::Serialize;

use crate::{
    config::AppConfig,
    domain::scheduler_protocol::{ActivationSlot, ActivationState},
    runtime_db::RuntimeDb,
    types::{
        AgentStatus, TurnRecord, TurnTerminalKind, TurnTerminalRecord, TurnTerminalSummary,
        TurnTriggerSummary, WorkItemPlanStatus,
    },
};

use super::{scheduler, scheduler_executor, InitialWorkspaceBinding, RuntimeHandle};

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerTerminalRecoveryFixture {
    pub agent_id: String,
    pub work_item_id: String,
    pub message_id: String,
    pub turn_id: String,
    pub activation_id: String,
    pub admitted_generation: u64,
    pub queue_status: String,
    pub activation_state: String,
    pub slot_state: String,
}

pub async fn seed_scheduler_terminal_recovery_fixture(
    config: &AppConfig,
    agent_id: &str,
    objective: String,
) -> Result<SchedulerTerminalRecoveryFixture> {
    super::require_scheduler_acceptance_fixtures_enabled()?;
    let runtime_db =
        RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())?;
    let agent_home = config.agent_root_dir().join(agent_id);
    std::fs::create_dir_all(&agent_home)
        .with_context(|| format!("creating agent home {}", agent_home.display()))?;
    let runtime = RuntimeHandle::new_offline_with_runtime_db(
        agent_id,
        agent_home,
        InitialWorkspaceBinding::Detached,
        runtime_db,
    )?;
    if !runtime.scheduler_protocol_production_commands_enabled() {
        return Err(anyhow!(
            "scheduler recovery fixture requires HOLON_SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS=true"
        ));
    }

    let work_item = runtime
        .create_work_item(objective, Some(WorkItemPlanStatus::Ready), None, Vec::new())
        .await?;
    let agent_state = runtime.agent_state().await?;
    let projection = scheduler::SchedulerProjection::from_state_with_queue_len(
        &runtime.inner.storage,
        &agent_state,
        agent_state.pending,
    )?;
    let decision = scheduler::decide_next_action(
        &projection,
        scheduler::SchedulerBoundary::IdleTick,
        scheduler::SchedulerInput::IdleSignal(scheduler::SchedulerIdleSignal::QueuedAvailable {
            work_item: &work_item,
            duplicate: None,
        }),
    );
    if !matches!(
        decision.kind,
        scheduler::SchedulerDecisionKind::EmitSystemTick
    ) {
        return Err(anyhow!(
            "scheduler recovery fixture could not emit queued work item: {}",
            decision.reason
        ));
    }
    let shadow_comparison = scheduler::shadow_comparison_for_work_queue_tick(
        &projection,
        &work_item,
        "queued_available",
        &decision,
        scheduler::SchedulerBoundary::IdleTick,
    );
    runtime
        .emit_system_tick_from_work_queue(
            &work_item,
            "queued_available",
            shadow_comparison,
            Some(&decision),
        )
        .await?;
    let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await?
    {
        scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
        scheduler_executor::RunLoopPoll::Idle => {
            return Err(anyhow!(
                "scheduler recovery fixture did not claim the work queue message"
            ));
        }
        scheduler_executor::RunLoopPoll::Stopped(_, _) => {
            return Err(anyhow!(
                "scheduler recovery fixture agent was stopped before the claim"
            ));
        }
        scheduler_executor::RunLoopPoll::Shutdown => {
            return Err(anyhow!(
                "scheduler recovery fixture runtime shut down before the claim"
            ));
        }
    };
    let message = scheduled.message;
    let turn_id = message
        .turn_id
        .clone()
        .ok_or_else(|| anyhow!("claimed scheduler fixture message has no turn id"))?;
    let activation_id = scheduler_executor::canonical_activation_id(&message.id);
    let snapshot = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let activation = snapshot
        .activations
        .get(&activation_id)
        .ok_or_else(|| anyhow!("scheduler fixture claim did not create canonical activation"))?;
    if activation.state != ActivationState::Running
        || !matches!(
            snapshot.slot,
            ActivationSlot::Running {
                activation_id: ref slot_activation_id,
                ..
            } if slot_activation_id == &activation_id
        )
    {
        return Err(anyhow!(
            "scheduler fixture claim did not retain a running canonical activation"
        ));
    }

    let turn_index = runtime.agent_state().await?.turn_index.saturating_add(1);
    let terminal = TurnTerminalRecord {
        turn_id: turn_id.clone(),
        turn_index,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: Some("scheduler recovery fixture terminal".into()),
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    };
    let mut turn = TurnRecord::new(agent_id, &turn_id, turn_index);
    turn.run_id = scheduled.running_state.current_run_id.clone();
    turn.current_work_item_id = Some(work_item.id.clone());
    turn.trigger = Some(TurnTriggerSummary::from_message(&message));
    turn.input_message_ids = vec![message.id.clone()];
    turn.terminal = Some(TurnTerminalSummary::from_terminal(&terminal));
    runtime.inner.storage.append_turn(&turn)?;

    let mut stopped = runtime.agent_state().await?;
    stopped.status = AgentStatus::Stopped;
    stopped.current_run_id = None;
    stopped.pending = 0;
    runtime.inner.storage.write_agent(&stopped)?;

    Ok(SchedulerTerminalRecoveryFixture {
        agent_id: agent_id.to_string(),
        work_item_id: work_item.id,
        message_id: message.id,
        turn_id,
        activation_id,
        admitted_generation: activation.admitted_generation,
        queue_status: "dequeued".into(),
        activation_state: "running".into(),
        slot_state: "running".into(),
    })
}
