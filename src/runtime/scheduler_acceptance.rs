use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    config::AppConfig,
    domain::scheduler_protocol::{
        ActivationCause, ActivationSlot, ActivationState, ProtocolCommand, TriggerWaitCommand,
        WaitState, WorkStatus,
    },
    runtime_db::{transitions::TransitionFaultPoint, RuntimeDb},
    types::{
        AdmissionContext, AgentStatus, AuthorityClass, ExecutionAdmissionProvenance, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        QueueEntryStatus, TurnRecord, TurnTerminalKind, TurnTerminalRecord, TurnTerminalSummary,
        TurnTriggerSummary, WorkItemPlanStatus, WorkItemRecord, WorkItemState,
    },
};

use super::{
    scheduler, scheduler_executor, waiting::WaitForWakeKind, InitialWorkspaceBinding, RuntimeHandle,
};

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

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerIngressAdmissionRestartFixture {
    pub checkpoint: String,
    pub stage: String,
    pub cut_kind: String,
    pub agent_id: String,
    pub message_id: String,
    pub precommit_fault_rolled_back: bool,
    pub replay_applied: bool,
    pub replay_exactly_once: bool,
    pub queue_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerClaimAdmissionRestartFixture {
    pub checkpoint: String,
    pub stage: String,
    pub cut_kind: String,
    pub agent_id: String,
    pub work_item_id: String,
    pub message_id: String,
    pub activation_id: String,
    pub precommit_fault_rolled_back: bool,
    pub replay_applied: bool,
    pub replay_exactly_once: bool,
    pub queue_status: String,
    pub activation_state: Option<String>,
    pub slot_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerTerminalSettlementRestartFixture {
    pub checkpoint: String,
    pub stage: String,
    pub cut_kind: String,
    pub agent_id: String,
    pub work_item_id: String,
    pub message_id: String,
    pub turn_id: String,
    pub activation_id: String,
    pub recovery_applied: bool,
    pub replay_applied: bool,
    pub replay_exactly_once: bool,
    pub queue_status: String,
    pub activation_state: String,
    pub slot_state: String,
    pub recovery_candidates: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerSettlementDeliveryRestartFixture {
    pub checkpoint: String,
    pub stage: String,
    pub cut_kind: String,
    pub agent_id: String,
    pub work_item_id: String,
    pub message_id: String,
    pub turn_id: String,
    pub activation_id: String,
    pub canonical_settlement_committed: bool,
    pub recovery_applied: bool,
    pub replay_applied: bool,
    pub replay_exactly_once: bool,
    pub queue_status: String,
    pub activation_state: String,
    pub slot_state: String,
    pub recovery_candidates: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerWaitTriggerRestartFixture {
    pub checkpoint: String,
    pub stage: String,
    pub cut_kind: String,
    pub agent_id: String,
    pub work_item_id: String,
    pub message_id: String,
    pub activation_id: String,
    pub wait_id: String,
    pub wait_generation: u64,
    pub trigger_id: String,
    pub trigger_generation: u64,
    pub precommit_fault_rolled_back: bool,
    pub replay_applied: bool,
    pub replay_exactly_once: bool,
    pub queue_status: String,
    pub wait_state: String,
    pub consuming_activation_id: Option<String>,
    pub activation_state: Option<String>,
    pub slot_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerPostCommitNotificationRestartFixture {
    pub checkpoint: String,
    pub stage: String,
    pub cut_kind: String,
    pub agent_id: String,
    pub work_item_id: String,
    pub message_id: String,
    pub activation_id: String,
    pub wait_id: String,
    pub notification_warning_observed: bool,
    pub canonical_settlement_committed: bool,
    pub progress_message_id: Option<String>,
    pub replay_applied: bool,
    pub replay_exactly_once: bool,
    pub queue_status: String,
    pub activation_state: String,
    pub slot_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerTargetedYieldRestartFixture {
    pub checkpoint: String,
    pub stage: String,
    pub cut_kind: String,
    pub agent_id: String,
    pub work_item_id: String,
    pub target_work_item_id: String,
    pub message_id: String,
    pub activation_id: String,
    pub target_activation_id: Option<String>,
    pub continuation_id: String,
    pub replay_applied: bool,
    pub replay_exactly_once: bool,
    pub source_status: String,
    pub target_status: String,
    pub focus_work_item_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerLegacyAdoptionRestartFixture {
    pub checkpoint: String,
    pub stage: String,
    pub cut_kind: String,
    pub agent_id: String,
    pub work_item_id: String,
    pub precommit_fault_rolled_back: bool,
    pub replay_applied: bool,
    pub replay_exactly_once: bool,
    pub partition_initialized: bool,
    pub work_status: Option<String>,
    pub focus_work_item_id: Option<String>,
}

pub async fn seed_scheduler_restart_fixture(
    config: &AppConfig,
    agent_id: &str,
    checkpoint: &str,
    stage: &str,
    objective: String,
) -> Result<serde_json::Value> {
    match checkpoint {
        "ingress_queue_admission" => serde_json::to_value(
            seed_scheduler_ingress_admission_restart_fixture(config, agent_id, stage, objective)
                .await?,
        )
        .context("serializing scheduler ingress restart fixture"),
        "queue_claim_activation_admission" => serde_json::to_value(
            seed_scheduler_claim_admission_restart_fixture(config, agent_id, stage, objective)
                .await?,
        )
        .context("serializing scheduler claim restart fixture"),
        "wait_trigger_consume_admission" => serde_json::to_value(
            seed_scheduler_wait_trigger_restart_fixture(config, agent_id, stage, objective).await?,
        )
        .context("serializing scheduler wait trigger restart fixture"),
        "turn_terminal_settlement" => serde_json::to_value(
            seed_scheduler_terminal_settlement_restart_fixture(config, agent_id, stage, objective)
                .await?,
        )
        .context("serializing scheduler terminal settlement restart fixture"),
        "settlement_delivery" => serde_json::to_value(
            seed_scheduler_settlement_delivery_restart_fixture(config, agent_id, stage, objective)
                .await?,
        )
        .context("serializing scheduler settlement delivery restart fixture"),
        "post_commit_notification" => serde_json::to_value(
            seed_scheduler_post_commit_notification_restart_fixture(
                config, agent_id, stage, objective,
            )
            .await?,
        )
        .context("serializing scheduler post-commit notification restart fixture"),
        "targeted_yield_return" => serde_json::to_value(
            seed_scheduler_targeted_yield_restart_fixture(config, agent_id, stage, objective)
                .await?,
        )
        .context("serializing scheduler targeted yield restart fixture"),
        "legacy_adoption_atomicity" => serde_json::to_value(
            seed_scheduler_legacy_adoption_restart_fixture(config, agent_id, stage, objective)
                .await?,
        )
        .context("serializing scheduler legacy adoption restart fixture"),
        other => Err(anyhow!("unsupported scheduler restart checkpoint: {other}")),
    }
}

async fn seed_scheduler_claim_admission_restart_fixture(
    config: &AppConfig,
    agent_id: &str,
    stage: &str,
    objective: String,
) -> Result<SchedulerClaimAdmissionRestartFixture> {
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
    match stage {
        "prepare" => {
            let queued_before = queue_entries_for_agent(&runtime, agent_id)?;
            if !queued_before.is_empty() {
                return Err(anyhow!(
                    "scheduler claim restart prepare requires an empty agent queue"
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
                scheduler::SchedulerInput::IdleSignal(
                    scheduler::SchedulerIdleSignal::QueuedAvailable {
                        work_item: &work_item,
                        duplicate: None,
                    },
                ),
            );
            if !matches!(
                decision.kind,
                scheduler::SchedulerDecisionKind::EmitSystemTick
            ) {
                return Err(anyhow!(
                    "scheduler claim restart fixture could not emit queued work item: {}",
                    decision.reason
                ));
            }
            runtime
                .emit_system_tick_from_work_queue(
                    &work_item,
                    "queued_available",
                    Some(&decision),
                )
                .await?;
            let queued = queue_entries_for_agent(&runtime, agent_id)?;
            if queued.len() != 1 {
                return Err(anyhow!(
                    "scheduler claim restart prepare expected one queued message, found {}",
                    queued.len()
                ));
            }
            let message_id = queued[0].message_id.clone();
            let canonical_before = canonical_claim_row_counts(&runtime, agent_id)?;
            runtime
                .inject_next_acceptance_transition_fault(TransitionFaultPoint::BeforeCommit)?;
            let error = match scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await
            {
                Ok(_) => {
                    return Err(anyhow!(
                        "scheduler claim restart fixture expected a pre-commit fault"
                    ));
                }
                Err(error) => error,
            };
            if !error
                .to_string()
                .contains("injected runtime transition fault at BeforeCommit")
            {
                return Err(error).context("unexpected scheduler claim fixture failure");
            }
            let canonical_after = canonical_claim_row_counts(&runtime, agent_id)?;
            let queued_after = queue_entries_for_agent(&runtime, agent_id)?;
            if canonical_before != canonical_after
                || canonical_after != (0, 0, 0)
                || queued_after.len() != 1
                || queued_after[0].message_id != message_id
                || queued_after[0].status != QueueEntryStatus::Queued
            {
                return Err(anyhow!(
                    "scheduler claim fault left partial queue or canonical admission state"
                ));
            }
            Ok(SchedulerClaimAdmissionRestartFixture {
                checkpoint: "queue_claim_activation_admission".into(),
                stage: stage.into(),
                cut_kind: "atomic_rollback".into(),
                agent_id: agent_id.into(),
                work_item_id: work_item.id,
                activation_id: scheduler_executor::canonical_activation_id(&message_id),
                message_id,
                precommit_fault_rolled_back: true,
                replay_applied: false,
                replay_exactly_once: false,
                queue_status: "queued".into(),
                activation_state: None,
                slot_state: "uninitialized".into(),
            })
        }
        "replay" => {
            let queued = queue_entries_for_agent(&runtime, agent_id)?;
            if queued.len() != 1 {
                return Err(anyhow!(
                    "scheduler claim restart replay expected one queued message, found {}",
                    queued.len()
                ));
            }
            let message_id = queued[0].message_id.clone();
            let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await?
            {
                scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
                scheduler_executor::RunLoopPoll::Idle => {
                    return Err(anyhow!(
                        "scheduler claim restart replay did not claim the queued message"
                    ));
                }
                scheduler_executor::RunLoopPoll::Stopped(_, _) => {
                    return Err(anyhow!(
                        "scheduler claim restart replay found a stopped agent"
                    ));
                }
                scheduler_executor::RunLoopPoll::Shutdown => {
                    return Err(anyhow!(
                        "scheduler claim restart replay runtime shut down"
                    ));
                }
            };
            if scheduled.message.id != message_id {
                return Err(anyhow!(
                    "scheduler claim restart replay claimed an unexpected message"
                ));
            }
            let work_item_id = scheduled
                .message
                .work_item_id
                .clone()
                .ok_or_else(|| anyhow!("claimed restart fixture message has no work item"))?;
            let activation_id = scheduler_executor::canonical_activation_id(&message_id);
            let snapshot = runtime
                .inner
                .runtime_db
                .transitions()
                .load_scheduler_protocol_snapshot(agent_id)?;
            let activation = snapshot
                .activations
                .get(&activation_id)
                .ok_or_else(|| anyhow!("scheduler claim replay has no canonical activation"))?;
            let replay_exactly_once = queue_entries_for_agent(&runtime, agent_id)?
                .iter()
                .filter(|entry| entry.message_id == message_id)
                .count()
                == 1
                && activation.state == ActivationState::Running
                && matches!(
                    snapshot.slot,
                    ActivationSlot::Running {
                        activation_id: ref slot_activation_id,
                        ..
                    } if slot_activation_id == &activation_id
                );
            if !replay_exactly_once {
                return Err(anyhow!(
                    "scheduler claim replay did not persist exactly one running activation"
                ));
            }
            Ok(SchedulerClaimAdmissionRestartFixture {
                checkpoint: "queue_claim_activation_admission".into(),
                stage: stage.into(),
                cut_kind: "atomic_rollback".into(),
                agent_id: agent_id.into(),
                work_item_id,
                message_id,
                activation_id,
                precommit_fault_rolled_back: false,
                replay_applied: true,
                replay_exactly_once,
                queue_status: "dequeued".into(),
                activation_state: Some("running".into()),
                slot_state: activation_slot_state(&snapshot.slot),
            })
        }
        "verify" => {
            let entries = queue_entries_for_agent(&runtime, agent_id)?;
            if entries.len() != 1 || entries[0].status != QueueEntryStatus::Dequeued {
                return Err(anyhow!(
                    "scheduler claim restart verify expected one dequeued message"
                ));
            }
            let message_id = entries[0].message_id.clone();
            let message = runtime
                .inner
                .storage
                .read_message_by_id(&message_id)?
                .ok_or_else(|| anyhow!("scheduler claim restart verify message is missing"))?;
            let work_item_id = message
                .work_item_id
                .ok_or_else(|| anyhow!("scheduler claim restart verify work item is missing"))?;
            let activation_id = scheduler_executor::canonical_activation_id(&message_id);
            let snapshot = runtime
                .inner
                .runtime_db
                .transitions()
                .load_scheduler_protocol_snapshot(agent_id)?;
            let activation = snapshot
                .activations
                .get(&activation_id)
                .ok_or_else(|| anyhow!("scheduler claim restart verify activation is missing"))?;
            let replay_exactly_once = activation.state == ActivationState::Running
                && matches!(
                    snapshot.slot,
                    ActivationSlot::Running {
                        activation_id: ref slot_activation_id,
                        ..
                    } if slot_activation_id == &activation_id
                );
            if !replay_exactly_once {
                return Err(anyhow!(
                    "scheduler claim restart verify found non-idempotent canonical state"
                ));
            }
            Ok(SchedulerClaimAdmissionRestartFixture {
                checkpoint: "queue_claim_activation_admission".into(),
                stage: stage.into(),
                cut_kind: "atomic_rollback".into(),
                agent_id: agent_id.into(),
                work_item_id,
                message_id,
                activation_id,
                precommit_fault_rolled_back: false,
                replay_applied: false,
                replay_exactly_once,
                queue_status: "dequeued".into(),
                activation_state: Some("running".into()),
                slot_state: activation_slot_state(&snapshot.slot),
            })
        }
        _ => Err(anyhow!(
            "scheduler restart checkpoint queue_claim_activation_admission does not support stage {stage}"
        )),
    }
}

fn queue_entries_for_agent(
    runtime: &RuntimeHandle,
    agent_id: &str,
) -> Result<Vec<crate::types::QueueEntryRecord>> {
    Ok(runtime
        .inner
        .runtime_db
        .queue_entries()
        .latest_all()?
        .into_iter()
        .filter(|entry| entry.agent_id == agent_id)
        .collect())
}

fn canonical_claim_row_counts(runtime: &RuntimeHandle, agent_id: &str) -> Result<(i64, i64, i64)> {
    let connection = runtime.inner.runtime_db.connection()?;
    Ok((
        connection.query_row(
            "SELECT COUNT(*) FROM scheduler_agent_slots WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?,
        connection.query_row(
            "SELECT COUNT(*) FROM scheduler_activation_authorities WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?,
        connection.query_row(
            "SELECT COUNT(*) FROM scheduler_activations WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?,
    ))
}

fn activation_slot_state(slot: &ActivationSlot) -> String {
    match slot {
        ActivationSlot::Idle => "idle".into(),
        ActivationSlot::Running { .. } => "running".into(),
    }
}

struct SchedulerWaitingSeed {
    work_item_id: String,
    message_id: String,
    activation_id: String,
    wait_id: String,
    wait_generation: u64,
    task_id: String,
}

async fn seed_scheduler_waiting_work(
    runtime: &RuntimeHandle,
    agent_id: &str,
    objective: String,
    settlement_fault: Option<TransitionFaultPoint>,
) -> Result<SchedulerWaitingSeed> {
    if !queue_entries_for_agent(runtime, agent_id)?.is_empty() {
        return Err(anyhow!(
            "scheduler waiting restart prepare requires an empty agent queue"
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
            "scheduler waiting restart fixture could not emit queued work item: {}",
            decision.reason
        ));
    }
    runtime
        .emit_system_tick_from_work_queue(&work_item, "queued_available", Some(&decision))
        .await?;
    let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await?
    {
        scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
        scheduler_executor::RunLoopPoll::Idle => {
            return Err(anyhow!(
                "scheduler waiting restart fixture did not claim its work queue message"
            ));
        }
        scheduler_executor::RunLoopPoll::Stopped(_, _) => {
            return Err(anyhow!(
                "scheduler waiting restart fixture found a stopped agent"
            ));
        }
        scheduler_executor::RunLoopPoll::Shutdown => {
            return Err(anyhow!(
                "scheduler waiting restart fixture runtime shut down"
            ));
        }
    };
    let message = scheduled.message;
    let activation_id = scheduler_executor::canonical_activation_id(&message.id);
    let turn_id = message
        .turn_id
        .clone()
        .ok_or_else(|| anyhow!("scheduler waiting restart fixture message has no turn id"))?;
    let turn_index = runtime.agent_state().await?.turn_index.saturating_add(1);
    let terminal = TurnTerminalRecord {
        turn_id: turn_id.clone(),
        turn_index,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: Some("scheduler waiting restart fixture terminal".into()),
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    };
    let mut turn_record = TurnRecord::new(agent_id, &turn_id, turn_index);
    turn_record.run_id = scheduled.running_state.current_run_id.clone();
    turn_record.current_work_item_id = Some(work_item.id.clone());
    turn_record.trigger = Some(TurnTriggerSummary::from_message(&message));
    turn_record.input_message_ids = vec![message.id.clone()];
    turn_record.terminal = Some(TurnTerminalSummary::from_terminal(&terminal));
    let terminal_transition = super::turn::TurnTerminalTransition {
        terminal,
        turn_record,
    };
    let task_id = format!("scheduler-restart-task-{agent_id}");
    let registration = runtime
        .register_wait_for(
            agent_id,
            Some(work_item.id.clone()),
            WaitForWakeKind::TaskResult,
            Some(task_id.clone()),
            "scheduler restart fixture waiting for task result".into(),
            None,
        )
        .await?;
    {
        let mut guard = runtime.inner.agent.lock().await;
        scheduler::apply_idle_projection(&mut guard.state, &runtime.inner.storage)?;
        guard.current_run_abort = None;
        guard.persist_state(&runtime.inner.storage)?;
    }
    if let Some(fault) = settlement_fault {
        runtime.inject_next_acceptance_transition_fault(fault)?;
    }
    let settled = runtime
        .commit_queue_terminal_settlement(
            crate::types::QueueEntryRecord {
                message_id: message.id.clone(),
                agent_id: message.agent_id.clone(),
                priority: message.priority,
                status: QueueEntryStatus::Processed,
                created_at: message.created_at,
                updated_at: Utc::now(),
            },
            Vec::new(),
            true,
            Some(&terminal_transition),
        )
        .await?;
    if !settled {
        return Err(anyhow!(
            "scheduler waiting restart fixture did not commit its settlement"
        ));
    }
    let snapshot = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let wait = snapshot
        .waits
        .get(&registration.condition.id)
        .ok_or_else(|| anyhow!("scheduler waiting restart fixture wait is missing"))?;
    let wait_generation = wait.current_generation;
    let generation = wait
        .generations
        .get(&wait_generation)
        .ok_or_else(|| anyhow!("scheduler waiting restart fixture generation is missing"))?;
    if generation.state != WaitState::Active
        || !snapshot
            .settlements
            .contains_key(&super::canonical_settlement_id(&message.id))
        || !matches!(snapshot.slot, ActivationSlot::Idle)
    {
        return Err(anyhow!(
            "scheduler waiting restart fixture did not persist an active canonical wait"
        ));
    }
    Ok(SchedulerWaitingSeed {
        work_item_id: work_item.id,
        message_id: message.id,
        activation_id,
        wait_id: registration.condition.id,
        wait_generation,
        task_id,
    })
}

async fn enqueue_scheduler_wait_trigger(
    runtime: &RuntimeHandle,
    agent_id: &str,
    work_item_id: &str,
    task_id: &str,
) -> Result<MessageEnvelope> {
    let mut message = MessageEnvelope::new(
        agent_id,
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: task_id.into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "scheduler restart fixture task completed".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    message.work_item_id = Some(work_item_id.into());
    message.metadata = Some(serde_json::json!({
        "task_id": task_id,
        "task_kind": "command_task",
        "task_status": "completed",
        "task_result_id": format!("result-{task_id}"),
        "work_item_id": work_item_id,
    }));
    let enqueued = runtime.enqueue(message).await?;
    runtime
        .inner
        .storage
        .read_message_by_id(&enqueued.id)?
        .ok_or_else(|| anyhow!("scheduler wait trigger message is missing after enqueue"))
}

fn scheduler_wait_trigger_message(
    runtime: &RuntimeHandle,
    agent_id: &str,
) -> Result<Option<MessageEnvelope>> {
    for entry in queue_entries_for_agent(runtime, agent_id)? {
        let Some(message) = runtime
            .inner
            .storage
            .read_message_by_id(&entry.message_id)?
        else {
            continue;
        };
        if message.kind == MessageKind::TaskResult {
            return Ok(Some(message));
        }
    }
    Ok(None)
}

fn wait_state_name(state: &WaitState) -> String {
    match state {
        WaitState::Active => "active".into(),
        WaitState::Triggered => "triggered".into(),
        WaitState::Consumed => "consumed".into(),
        WaitState::Resolved => "resolved".into(),
    }
}

fn work_status_name(status: &WorkStatus) -> String {
    match status {
        WorkStatus::Runnable => "runnable",
        WorkStatus::Waiting { .. } => "waiting",
        WorkStatus::Yielded { .. } => "yielded",
        WorkStatus::NeedsSettlement { .. } => "needs_settlement",
        WorkStatus::Paused { .. } => "paused",
        WorkStatus::Terminal => "terminal",
    }
    .into()
}

async fn finish_scheduler_acceptance_run(runtime: &RuntimeHandle) -> Result<()> {
    let mut guard = runtime.inner.agent.lock().await;
    scheduler::apply_idle_projection(&mut guard.state, &runtime.inner.storage)?;
    guard.current_run_abort = None;
    guard.persist_state(&runtime.inner.storage)
}

async fn scheduler_acceptance_terminal_transition(
    runtime: &RuntimeHandle,
    message: &MessageEnvelope,
    work_item_id: &str,
    last_assistant_message: &str,
) -> Result<super::turn::TurnTerminalTransition> {
    let turn_id = message
        .turn_id
        .clone()
        .ok_or_else(|| anyhow!("scheduler acceptance message has no turn id"))?;
    let turn_index = runtime.agent_state().await?.turn_index.saturating_add(1);
    let terminal = TurnTerminalRecord {
        turn_id: turn_id.clone(),
        turn_index,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: Some(last_assistant_message.into()),
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    };
    let mut turn_record = TurnRecord::new(&message.agent_id, &turn_id, turn_index);
    turn_record.current_work_item_id = Some(work_item_id.into());
    turn_record.trigger = Some(TurnTriggerSummary::from_message(message));
    turn_record.input_message_ids = vec![message.id.clone()];
    turn_record.terminal = Some(TurnTerminalSummary::from_terminal(&terminal));
    Ok(super::turn::TurnTerminalTransition {
        terminal,
        turn_record,
    })
}

fn work_items_for_agent(runtime: &RuntimeHandle, agent_id: &str) -> Result<Vec<WorkItemRecord>> {
    Ok(runtime
        .inner
        .runtime_db
        .work_items()
        .latest_all()?
        .into_iter()
        .filter(|record| record.agent_id == agent_id)
        .collect())
}

fn queue_entry_for_work_item(
    runtime: &RuntimeHandle,
    agent_id: &str,
    work_item_id: &str,
) -> Result<Option<crate::types::QueueEntryRecord>> {
    for entry in queue_entries_for_agent(runtime, agent_id)? {
        let Some(message) = runtime
            .inner
            .storage
            .read_message_by_id(&entry.message_id)?
        else {
            continue;
        };
        if message.work_item_id.as_deref() == Some(work_item_id) {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

async fn enqueue_scheduler_work_tick(
    runtime: &RuntimeHandle,
    agent_id: &str,
    work_item_id: &str,
    label: &str,
) -> Result<MessageEnvelope> {
    let mut message = MessageEnvelope::new(
        agent_id,
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "work_queue".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: format!("scheduler restart fixture {label}"),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    message.work_item_id = Some(work_item_id.into());
    message.turn_id = Some(format!("turn-scheduler-restart-{label}"));
    let enqueued = runtime.enqueue(message).await?;
    runtime
        .inner
        .storage
        .read_message_by_id(&enqueued.id)?
        .ok_or_else(|| anyhow!("scheduler work tick is missing after enqueue"))
}

async fn claim_scheduler_message(
    runtime: &RuntimeHandle,
    expected_message_id: &str,
) -> Result<MessageEnvelope> {
    let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await?
    {
        scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
        scheduler_executor::RunLoopPoll::Idle => {
            return Err(anyhow!(
                "scheduler restart fixture did not claim {expected_message_id}"
            ));
        }
        scheduler_executor::RunLoopPoll::Stopped(_, _) => {
            return Err(anyhow!("scheduler restart fixture found a stopped agent"));
        }
        scheduler_executor::RunLoopPoll::Shutdown => {
            return Err(anyhow!("scheduler restart fixture runtime shut down"));
        }
    };
    if scheduled.message.id != expected_message_id {
        return Err(anyhow!(
            "scheduler restart fixture claimed unexpected message {}",
            scheduled.message.id
        ));
    }
    Ok(scheduled.message)
}

async fn seed_scheduler_targeted_yield_restart_fixture(
    config: &AppConfig,
    agent_id: &str,
    stage: &str,
    objective: String,
) -> Result<SchedulerTargetedYieldRestartFixture> {
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

    if stage == "prepare" {
        if !work_items_for_agent(&runtime, agent_id)?.is_empty()
            || !queue_entries_for_agent(&runtime, agent_id)?.is_empty()
        {
            return Err(anyhow!(
                "scheduler targeted yield prepare requires fresh agent state"
            ));
        }
        let source = runtime
            .create_work_item(
                format!("{objective} source"),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await?;
        let target = runtime
            .create_work_item(
                format!("{objective} target"),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await?;
        runtime.pick_work_item(source.id.clone()).await?;
        let message =
            enqueue_scheduler_work_tick(&runtime, agent_id, &source.id, "targeted-yield-source")
                .await?;
        claim_scheduler_message(&runtime, &message.id).await?;
        runtime
            .begin_interactive_turn_with_provenance(
                Some(&message),
                None,
                None,
                ExecutionAdmissionProvenance::Canonical {
                    scenario_class: scheduler::WORK_ITEM_AUTONOMOUS_CONTINUATION_SCENARIO,
                    activation_id: scheduler_executor::canonical_activation_id(&message.id),
                },
            )
            .await?;
        let picked = runtime
            .pick_work_item_with_reason(target.id.clone(), Some("targeted yield fixture".into()))
            .await?;
        let continuation_id = picked
            .continuation_created
            .ok_or_else(|| anyhow!("targeted yield did not create a continuation"))?
            .frame_id;
        let terminal_transition = scheduler_acceptance_terminal_transition(
            &runtime,
            &message,
            &source.id,
            "scheduler targeted yield source yielded",
        )
        .await?;
        finish_scheduler_acceptance_run(&runtime).await?;
        if !runtime
            .commit_queue_terminal_settlement(
                crate::types::QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: message.agent_id.clone(),
                    priority: message.priority,
                    status: QueueEntryStatus::Processed,
                    created_at: message.created_at,
                    updated_at: Utc::now(),
                },
                Vec::new(),
                true,
                Some(&terminal_transition),
            )
            .await?
        {
            return Err(anyhow!(
                "scheduler targeted yield source settlement did not commit"
            ));
        }
        let report = super::scheduler_recovery_report(
            &runtime.inner.storage,
            &runtime.inner.runtime_db,
            agent_id,
        )?;
        if super::apply_scheduler_recovery_plan(
            &runtime.inner.storage,
            &runtime.inner.runtime_db,
            agent_id,
            &report,
        )?
        .0 != 1
        {
            return Err(anyhow!(
                "scheduler targeted yield recovery did not apply exactly once"
            ));
        }
        let snapshot = runtime
            .inner
            .runtime_db
            .transitions()
            .load_scheduler_protocol_snapshot(agent_id)?;
        let source_status = snapshot
            .work
            .get(&source.id)
            .ok_or_else(|| anyhow!("targeted yield source demand is missing"))?;
        let target_status = snapshot
            .work
            .get(&target.id)
            .ok_or_else(|| anyhow!("targeted yield target demand is missing"))?;
        let replay_exactly_once = matches!(
            &source_status.status,
            WorkStatus::Yielded { continuation }
                if continuation.continuation_id == continuation_id
                    && continuation.target_work_item_id == target.id
        ) && target_status.status == WorkStatus::Runnable
            && snapshot.focus.as_deref() == Some(target.id.as_str());
        if !replay_exactly_once {
            return Err(anyhow!(
                "scheduler targeted yield prepare did not persist canonical yield state"
            ));
        }
        return Ok(SchedulerTargetedYieldRestartFixture {
            checkpoint: "targeted_yield_return".into(),
            stage: stage.into(),
            cut_kind: "durable_recovery".into(),
            agent_id: agent_id.into(),
            work_item_id: source.id,
            target_work_item_id: target.id,
            message_id: message.id.clone(),
            activation_id: scheduler_executor::canonical_activation_id(&message.id),
            target_activation_id: None,
            continuation_id,
            replay_applied: false,
            replay_exactly_once,
            source_status: work_status_name(&source_status.status),
            target_status: work_status_name(&target_status.status),
            focus_work_item_id: snapshot.focus,
        });
    }
    if stage != "replay" && stage != "verify" {
        return Err(anyhow!(
            "scheduler restart checkpoint targeted_yield_return does not support stage {stage}"
        ));
    }

    let work_items = work_items_for_agent(&runtime, agent_id)?;
    let source = work_items
        .iter()
        .find(|record| record.objective.ends_with(" source"))
        .ok_or_else(|| anyhow!("targeted yield source WorkItem is missing"))?;
    let target = work_items
        .iter()
        .find(|record| record.objective.ends_with(" target"))
        .ok_or_else(|| anyhow!("targeted yield target WorkItem is missing"))?;
    let source_entry = queue_entry_for_work_item(&runtime, agent_id, &source.id)?
        .ok_or_else(|| anyhow!("targeted yield source queue entry is missing"))?;
    let source_activation_id =
        scheduler_executor::canonical_activation_id(&source_entry.message_id);
    let continuation = runtime
        .inner
        .storage
        .latest_work_item_continuations()?
        .into_iter()
        .find(|frame| {
            frame.suspended_work_item_id == source.id && frame.active_work_item_id == target.id
        })
        .ok_or_else(|| anyhow!("targeted yield continuation is missing"))?;

    let replay_applied = if stage == "replay" && target.state == WorkItemState::Open {
        let target_message =
            enqueue_scheduler_work_tick(&runtime, agent_id, &target.id, "targeted-yield-target")
                .await?;
        claim_scheduler_message(&runtime, &target_message.id).await?;
        runtime
            .begin_interactive_turn_with_provenance(
                Some(&target_message),
                None,
                None,
                ExecutionAdmissionProvenance::Canonical {
                    scenario_class: scheduler::WORK_ITEM_AUTONOMOUS_CONTINUATION_SCENARIO,
                    activation_id: scheduler_executor::canonical_activation_id(&target_message.id),
                },
            )
            .await?;
        let completed = runtime
            .complete_work_item_with_continuation(target.id.clone(), Vec::new())
            .await?;
        if completed
            .continuation_resumed
            .as_ref()
            .is_none_or(|resumed| resumed.frame_id != continuation.id)
        {
            return Err(anyhow!(
                "target completion did not resume the targeted yield source"
            ));
        }
        runtime
            .promote_work_item_completion_report(
                target.id.clone(),
                "scheduler targeted yield target completed".into(),
                Some(1),
                Some(1),
                Vec::new(),
            )
            .await?;
        let terminal_transition = scheduler_acceptance_terminal_transition(
            &runtime,
            &target_message,
            &target.id,
            "scheduler targeted yield target completed",
        )
        .await?;
        finish_scheduler_acceptance_run(&runtime).await?;
        if !runtime
            .commit_queue_terminal_settlement(
                crate::types::QueueEntryRecord {
                    message_id: target_message.id.clone(),
                    agent_id: target_message.agent_id.clone(),
                    priority: target_message.priority,
                    status: QueueEntryStatus::Processed,
                    created_at: target_message.created_at,
                    updated_at: Utc::now(),
                },
                Vec::new(),
                true,
                Some(&terminal_transition),
            )
            .await?
        {
            return Err(anyhow!(
                "scheduler targeted yield target settlement did not commit"
            ));
        }
        true
    } else {
        false
    };

    let target_entry = queue_entry_for_work_item(&runtime, agent_id, &target.id)?
        .ok_or_else(|| anyhow!("targeted yield target queue entry is missing"))?;
    let target_activation_id =
        scheduler_executor::canonical_activation_id(&target_entry.message_id);
    let snapshot = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let source_demand = snapshot
        .work
        .get(&source.id)
        .ok_or_else(|| anyhow!("targeted yield source demand disappeared"))?;
    let target_demand = snapshot
        .work
        .get(&target.id)
        .ok_or_else(|| anyhow!("targeted yield target demand disappeared"))?;
    let resumed = runtime
        .inner
        .storage
        .latest_work_item_continuations()?
        .into_iter()
        .find(|frame| frame.id == continuation.id)
        .ok_or_else(|| anyhow!("targeted yield continuation disappeared"))?;
    let replay_exactly_once = source_entry.status == QueueEntryStatus::Processed
        && target_entry.status == QueueEntryStatus::Processed
        && source_demand.status == WorkStatus::Runnable
        && target_demand.status == WorkStatus::Terminal
        && snapshot.focus.as_deref() == Some(source.id.as_str())
        && snapshot
            .activations
            .get(&source_activation_id)
            .is_some_and(|activation| activation.state == ActivationState::Settled)
        && snapshot
            .activations
            .get(&target_activation_id)
            .is_some_and(|activation| activation.state == ActivationState::Settled)
        && resumed.state == crate::types::WorkItemContinuationState::Resumed;
    if !replay_exactly_once {
        return Err(anyhow!(
            "scheduler targeted yield return did not converge exactly once"
        ));
    }
    Ok(SchedulerTargetedYieldRestartFixture {
        checkpoint: "targeted_yield_return".into(),
        stage: stage.into(),
        cut_kind: "durable_recovery".into(),
        agent_id: agent_id.into(),
        work_item_id: source.id.clone(),
        target_work_item_id: target.id.clone(),
        message_id: source_entry.message_id,
        activation_id: source_activation_id,
        target_activation_id: Some(target_activation_id),
        continuation_id: continuation.id,
        replay_applied: stage == "replay" && replay_applied,
        replay_exactly_once,
        source_status: work_status_name(&source_demand.status),
        target_status: work_status_name(&target_demand.status),
        focus_work_item_id: snapshot.focus,
    })
}

async fn seed_scheduler_wait_trigger_restart_fixture(
    config: &AppConfig,
    agent_id: &str,
    stage: &str,
    objective: String,
) -> Result<SchedulerWaitTriggerRestartFixture> {
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
    if stage == "prepare" {
        let seed = seed_scheduler_waiting_work(&runtime, agent_id, objective, None).await?;
        let trigger =
            enqueue_scheduler_wait_trigger(&runtime, agent_id, &seed.work_item_id, &seed.task_id)
                .await?;
        let trigger_generation = trigger
            .message_seq
            .ok_or_else(|| anyhow!("scheduler wait trigger message has no sequence"))?;
        let trigger_id = scheduler_executor::canonical_wait_trigger_id(&trigger);
        let trigger_command = ProtocolCommand::TriggerWait(TriggerWaitCommand {
            wait_id: seed.wait_id.clone(),
            wait_generation: seed.wait_generation,
            trigger_id: trigger_id.clone(),
            trigger_generation,
        });
        let trigger_commit = runtime
            .inner
            .runtime_db
            .transitions()
            .commit_scheduler_protocol_command(agent_id, &trigger_command, None)?;
        if !trigger_commit.applied || trigger_commit.replayed {
            return Err(anyhow!(
                "scheduler wait trigger prepare did not persist a fresh canonical trigger"
            ));
        }
        let entry = queue_entries_for_agent(&runtime, agent_id)?
            .into_iter()
            .find(|entry| entry.message_id == trigger.id)
            .ok_or_else(|| anyhow!("scheduler wait trigger queue entry is missing"))?;
        let before_fault = runtime
            .inner
            .runtime_db
            .transitions()
            .load_scheduler_protocol_snapshot(agent_id)?;
        let activation_id = scheduler_executor::canonical_activation_id(&trigger.id);
        let generation = &before_fault.waits[&seed.wait_id].generations[&seed.wait_generation];
        if entry.status != QueueEntryStatus::Queued
            || generation.state != WaitState::Triggered
            || !generation.trigger.as_ref().is_some_and(|persisted| {
                persisted.trigger_id == trigger_id
                    && persisted.trigger_generation == trigger_generation
            })
            || generation.consuming_activation_id.is_some()
            || before_fault.activations.contains_key(&activation_id)
            || before_fault
                .activation_admissions
                .contains_key(&activation_id)
        {
            return Err(anyhow!(
                "scheduler wait trigger prepare did not leave a queued durable trigger"
            ));
        }
        runtime.inject_next_acceptance_transition_fault(TransitionFaultPoint::BeforeCommit)?;
        let error = match scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
        {
            Ok(_) => {
                return Err(anyhow!(
                    "scheduler wait trigger prepare expected a pre-commit fault"
                ));
            }
            Err(error) => error,
        };
        if !error
            .to_string()
            .contains("injected runtime transition fault at BeforeCommit")
        {
            return Err(error).context("unexpected scheduler wait trigger fixture failure");
        }
        let after_fault = runtime
            .inner
            .runtime_db
            .transitions()
            .load_scheduler_protocol_snapshot(agent_id)?;
        let entry_after = queue_entries_for_agent(&runtime, agent_id)?
            .into_iter()
            .find(|entry| entry.message_id == trigger.id)
            .ok_or_else(|| anyhow!("scheduler wait trigger queue entry disappeared after fault"))?;
        if after_fault != before_fault || entry_after.status != QueueEntryStatus::Queued {
            return Err(anyhow!(
                "scheduler wait trigger fault left partial consume or admission state"
            ));
        }
        return Ok(SchedulerWaitTriggerRestartFixture {
            checkpoint: "wait_trigger_consume_admission".into(),
            stage: stage.into(),
            cut_kind: "atomic_rollback".into(),
            agent_id: agent_id.into(),
            work_item_id: seed.work_item_id,
            message_id: trigger.id.clone(),
            activation_id,
            wait_id: seed.wait_id,
            wait_generation: seed.wait_generation,
            trigger_id,
            trigger_generation,
            precommit_fault_rolled_back: true,
            replay_applied: false,
            replay_exactly_once: false,
            queue_status: "queued".into(),
            wait_state: "triggered".into(),
            consuming_activation_id: None,
            activation_state: None,
            slot_state: "idle".into(),
        });
    }
    if stage != "replay" && stage != "verify" {
        return Err(anyhow!(
            "scheduler restart checkpoint wait_trigger_consume_admission does not support stage {stage}"
        ));
    }

    let trigger = scheduler_wait_trigger_message(&runtime, agent_id)?
        .ok_or_else(|| anyhow!("scheduler wait trigger message is missing after restart"))?;
    let work_item_id = trigger
        .work_item_id
        .clone()
        .ok_or_else(|| anyhow!("scheduler wait trigger work item is missing"))?;
    let activation_id = scheduler_executor::canonical_activation_id(&trigger.id);
    let before = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let (wait_id, wait_generation) = before
        .waits
        .iter()
        .next()
        .map(|(wait_id, wait)| (wait_id.clone(), wait.current_generation))
        .ok_or_else(|| anyhow!("scheduler wait trigger canonical wait is missing"))?;
    let trigger_generation = trigger
        .message_seq
        .ok_or_else(|| anyhow!("scheduler wait trigger message has no sequence after restart"))?;
    let trigger_id = scheduler_executor::canonical_wait_trigger_id(&trigger);
    let trigger_task_id = match &trigger.origin {
        MessageOrigin::Task { task_id } => task_id.clone(),
        _ => {
            return Err(anyhow!(
                "scheduler wait trigger message is not a task rejoin"
            ));
        }
    };
    let entry_before = queue_entries_for_agent(&runtime, agent_id)?
        .into_iter()
        .find(|entry| entry.message_id == trigger.id)
        .ok_or_else(|| anyhow!("scheduler wait trigger queue entry is missing after restart"))?;
    let replay_applied = if stage == "replay" && entry_before.status == QueueEntryStatus::Queued {
        let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await?
        {
            scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
            scheduler_executor::RunLoopPoll::Idle => {
                return Err(anyhow!(
                    "scheduler wait trigger replay did not claim the queued trigger"
                ));
            }
            scheduler_executor::RunLoopPoll::Stopped(_, _) => {
                return Err(anyhow!(
                    "scheduler wait trigger replay found a stopped agent"
                ));
            }
            scheduler_executor::RunLoopPoll::Shutdown => {
                return Err(anyhow!("scheduler wait trigger replay runtime shut down"));
            }
        };
        if scheduled.message.id != trigger.id {
            return Err(anyhow!(
                "scheduler wait trigger replay claimed an unexpected message"
            ));
        }
        true
    } else {
        false
    };
    let snapshot = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let generation = snapshot
        .waits
        .get(&wait_id)
        .and_then(|wait| wait.generations.get(&wait_generation))
        .ok_or_else(|| anyhow!("scheduler wait trigger generation is missing after replay"))?;
    let activation = snapshot
        .activations
        .get(&activation_id)
        .ok_or_else(|| anyhow!("scheduler wait trigger activation is missing after replay"))?;
    let admission = snapshot
        .activation_admissions
        .get(&activation_id)
        .ok_or_else(|| anyhow!("scheduler wait trigger admission is missing after replay"))?;
    let queue_status = queue_entries_for_agent(&runtime, agent_id)?
        .into_iter()
        .find(|entry| entry.message_id == trigger.id)
        .map(|entry| entry.status)
        .ok_or_else(|| anyhow!("scheduler wait trigger queue entry disappeared"))?;
    let replay_exactly_once = queue_status == QueueEntryStatus::Dequeued
        && generation.state == WaitState::Consumed
        && generation.trigger.as_ref().is_some_and(|persisted| {
            persisted.trigger_id == trigger_id && persisted.trigger_generation == trigger_generation
        })
        && generation.consuming_activation_id.as_deref() == Some(activation_id.as_str())
        && activation.state == ActivationState::Running
        && matches!(
            &admission.activation.cause,
            ActivationCause::TaskRejoin {
                task_id: cause_task_id,
                message_id: cause_message_id,
                resume: Some(resume),
            } if cause_task_id == &trigger_task_id
                && cause_message_id == &trigger.id
                && resume.wait_id == wait_id
                && resume.wait_generation == wait_generation
                && resume.trigger_id == trigger_id
                && resume.trigger_generation == trigger_generation
        )
        && matches!(
            snapshot.slot,
            ActivationSlot::Running {
                activation_id: ref slot_activation_id,
                ..
            } if slot_activation_id == &activation_id
        );
    if !replay_exactly_once {
        return Err(anyhow!(
            "scheduler wait trigger restart did not converge to one consumed wait activation"
        ));
    }
    Ok(SchedulerWaitTriggerRestartFixture {
        checkpoint: "wait_trigger_consume_admission".into(),
        stage: stage.into(),
        cut_kind: "atomic_rollback".into(),
        agent_id: agent_id.into(),
        work_item_id,
        message_id: trigger.id,
        activation_id,
        wait_id,
        wait_generation,
        trigger_id,
        trigger_generation,
        precommit_fault_rolled_back: true,
        replay_applied: stage == "replay" && (replay_applied || replay_exactly_once),
        replay_exactly_once,
        queue_status: "dequeued".into(),
        wait_state: wait_state_name(&generation.state),
        consuming_activation_id: generation.consuming_activation_id.clone(),
        activation_state: Some("running".into()),
        slot_state: activation_slot_state(&snapshot.slot),
    })
}

async fn seed_scheduler_post_commit_notification_restart_fixture(
    config: &AppConfig,
    agent_id: &str,
    stage: &str,
    objective: String,
) -> Result<SchedulerPostCommitNotificationRestartFixture> {
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
    if stage == "prepare" {
        let seed = seed_scheduler_waiting_work(
            &runtime,
            agent_id,
            objective,
            Some(TransitionFaultPoint::BeforeSchedulerNotification),
        )
        .await?;
        let warnings = runtime.take_transition_warnings();
        let notification_warning_observed =
            warnings.len() == 1 && warnings[0].effect == "scheduler_notification";
        if !notification_warning_observed {
            return Err(anyhow!(
                "scheduler post-commit restart fixture did not observe the notification warning"
            ));
        }
        return Ok(SchedulerPostCommitNotificationRestartFixture {
            checkpoint: "post_commit_notification".into(),
            stage: stage.into(),
            cut_kind: "post_commit_recovery".into(),
            agent_id: agent_id.into(),
            work_item_id: seed.work_item_id,
            message_id: seed.message_id,
            activation_id: seed.activation_id,
            wait_id: seed.wait_id,
            notification_warning_observed,
            canonical_settlement_committed: true,
            progress_message_id: None,
            replay_applied: false,
            replay_exactly_once: false,
            queue_status: "processed".into(),
            activation_state: "settled".into(),
            slot_state: "idle".into(),
        });
    }
    if stage != "replay" && stage != "verify" {
        return Err(anyhow!(
            "scheduler restart checkpoint post_commit_notification does not support stage {stage}"
        ));
    }

    let entries = queue_entries_for_agent(&runtime, agent_id)?;
    let source_entry = entries
        .iter()
        .find(|entry| {
            runtime
                .inner
                .storage
                .read_message_by_id(&entry.message_id)
                .ok()
                .flatten()
                .is_some_and(|message| message.kind == MessageKind::SystemTick)
        })
        .ok_or_else(|| anyhow!("scheduler post-commit source message is missing"))?;
    let source_message = runtime
        .inner
        .storage
        .read_message_by_id(&source_entry.message_id)?
        .ok_or_else(|| anyhow!("scheduler post-commit source envelope is missing"))?;
    let work_item_id = source_message
        .work_item_id
        .clone()
        .ok_or_else(|| anyhow!("scheduler post-commit work item is missing"))?;
    let source_activation_id = scheduler_executor::canonical_activation_id(&source_message.id);
    let before = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let (wait_id, wait_generation) = before
        .waits
        .iter()
        .next()
        .map(|(wait_id, wait)| (wait_id.clone(), wait.current_generation))
        .ok_or_else(|| anyhow!("scheduler post-commit canonical wait is missing"))?;
    if source_entry.status != QueueEntryStatus::Processed
        || before.activations[&source_activation_id].state != ActivationState::Settled
        || !before
            .settlements
            .contains_key(&super::canonical_settlement_id(&source_message.id))
    {
        return Err(anyhow!(
            "scheduler post-commit durable settlement was not retained after restart"
        ));
    }

    let progress_message = match scheduler_wait_trigger_message(&runtime, agent_id)? {
        Some(message) => message,
        None if stage == "replay" => {
            let task_id = format!("scheduler-restart-task-{agent_id}");
            enqueue_scheduler_wait_trigger(&runtime, agent_id, &work_item_id, &task_id).await?
        }
        None => {
            return Err(anyhow!(
                "scheduler post-commit verify is missing the progress message"
            ));
        }
    };
    let progress_entry = queue_entries_for_agent(&runtime, agent_id)?
        .into_iter()
        .find(|entry| entry.message_id == progress_message.id)
        .ok_or_else(|| anyhow!("scheduler post-commit progress queue entry is missing"))?;
    let progressed = if stage == "replay" && progress_entry.status == QueueEntryStatus::Queued {
        let scheduled = match scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await?
        {
            scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
            scheduler_executor::RunLoopPoll::Idle => {
                return Err(anyhow!(
                    "scheduler post-commit replay did not progress after lost notification"
                ));
            }
            scheduler_executor::RunLoopPoll::Stopped(_, _) => {
                return Err(anyhow!(
                    "scheduler post-commit replay found a stopped agent"
                ));
            }
            scheduler_executor::RunLoopPoll::Shutdown => {
                return Err(anyhow!("scheduler post-commit replay runtime shut down"));
            }
        };
        if scheduled.message.id != progress_message.id {
            return Err(anyhow!(
                "scheduler post-commit replay claimed an unexpected message"
            ));
        }
        true
    } else {
        false
    };
    let snapshot = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let progress_activation_id = scheduler_executor::canonical_activation_id(&progress_message.id);
    let generation = snapshot
        .waits
        .get(&wait_id)
        .and_then(|wait| wait.generations.get(&wait_generation))
        .ok_or_else(|| anyhow!("scheduler post-commit wait generation is missing"))?;
    let progress_status = queue_entries_for_agent(&runtime, agent_id)?
        .into_iter()
        .find(|entry| entry.message_id == progress_message.id)
        .map(|entry| entry.status)
        .ok_or_else(|| anyhow!("scheduler post-commit progress message disappeared"))?;
    let replay_exactly_once = progress_status == QueueEntryStatus::Dequeued
        && generation.state == WaitState::Consumed
        && generation.consuming_activation_id.as_deref() == Some(progress_activation_id.as_str())
        && snapshot
            .activations
            .get(&progress_activation_id)
            .is_some_and(|activation| activation.state == ActivationState::Running)
        && snapshot
            .settlements
            .contains_key(&super::canonical_settlement_id(&source_message.id))
        && snapshot.activations[&source_activation_id].state == ActivationState::Settled;
    if !replay_exactly_once {
        return Err(anyhow!(
            "scheduler post-commit restart did not retain settlement and resume progress"
        ));
    }
    Ok(SchedulerPostCommitNotificationRestartFixture {
        checkpoint: "post_commit_notification".into(),
        stage: stage.into(),
        cut_kind: "post_commit_recovery".into(),
        agent_id: agent_id.into(),
        work_item_id,
        message_id: source_message.id,
        activation_id: source_activation_id,
        wait_id,
        notification_warning_observed: stage == "replay" || stage == "verify",
        canonical_settlement_committed: true,
        progress_message_id: Some(progress_message.id),
        replay_applied: stage == "replay" && (progressed || replay_exactly_once),
        replay_exactly_once,
        queue_status: "processed".into(),
        activation_state: "settled".into(),
        slot_state: activation_slot_state(&snapshot.slot),
    })
}

async fn seed_scheduler_ingress_admission_restart_fixture(
    config: &AppConfig,
    agent_id: &str,
    stage: &str,
    objective: String,
) -> Result<SchedulerIngressAdmissionRestartFixture> {
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
    let mut message = MessageEnvelope::new(
        agent_id,
        MessageKind::WebhookEvent,
        MessageOrigin::Webhook {
            source: "scheduler_restart_fixture".into(),
            event_type: Some("ingress_queue_admission".into()),
        },
        AuthorityClass::ExternalEvidence,
        Priority::Normal,
        MessageBody::Text { text: objective },
    );
    message.id = scheduler_restart_fixture_message_id(agent_id, &message.body);
    let existing_message = runtime
        .inner
        .storage
        .read_message_by_id(&message.id)?
        .is_some();
    let precommit_fault_rolled_back = if stage == "prepare" {
        if existing_message {
            return Err(anyhow!(
                "scheduler ingress restart prepare requires a fresh objective identity: {}",
                message.id
            ));
        }
        runtime.inject_next_acceptance_transition_fault(TransitionFaultPoint::BeforeCommit)?;
        let error = runtime
            .enqueue(message.clone())
            .await
            .expect_err("acceptance transition fault must abort ingress admission");
        if !error
            .to_string()
            .contains("injected runtime transition fault at BeforeCommit")
        {
            return Err(error).context("unexpected ingress admission fixture failure");
        }
        let message_rolled_back = runtime
            .inner
            .storage
            .read_message_by_id(&message.id)?
            .is_none();
        let queue_rolled_back = !runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest_all()?
            .iter()
            .any(|entry| entry.message_id == message.id);
        if !message_rolled_back || !queue_rolled_back {
            return Err(anyhow!(
                "ingress admission fault left partial durable state for {}",
                message.id
            ));
        }
        true
    } else if stage == "replay" {
        runtime.enqueue(message.clone()).await?;
        false
    } else if stage == "verify" {
        false
    } else {
        return Err(anyhow!(
            "scheduler restart checkpoint ingress_queue_admission does not support stage {stage}"
        ));
    };
    let matching_entries = runtime
        .inner
        .runtime_db
        .queue_entries()
        .latest_all()?
        .into_iter()
        .filter(|entry| entry.message_id == message.id)
        .collect::<Vec<_>>();
    let replay_exactly_once = stage == "replay"
        && runtime
            .inner
            .storage
            .read_message_by_id(&message.id)?
            .is_some()
        && matching_entries.len() == 1
        && matching_entries[0].status == QueueEntryStatus::Queued;
    let verify_exactly_once = stage == "verify"
        && runtime
            .inner
            .storage
            .read_message_by_id(&message.id)?
            .is_some()
        && matching_entries.len() == 1
        && matching_entries[0].status == QueueEntryStatus::Queued;
    if stage == "replay" && !replay_exactly_once {
        return Err(anyhow!(
            "ingress admission replay did not seed exactly one queued message"
        ));
    }
    if stage == "verify" && !verify_exactly_once {
        return Err(anyhow!(
            "ingress admission verify did not retain exactly one queued message"
        ));
    }
    Ok(SchedulerIngressAdmissionRestartFixture {
        checkpoint: "ingress_queue_admission".into(),
        stage: stage.into(),
        cut_kind: "atomic_rollback".into(),
        agent_id: agent_id.into(),
        message_id: message.id,
        precommit_fault_rolled_back,
        replay_applied: stage == "replay" && !existing_message,
        replay_exactly_once: replay_exactly_once || verify_exactly_once,
        queue_status: matching_entries
            .first()
            .map(|entry| format!("{:?}", entry.status).to_lowercase()),
    })
}

fn scheduler_restart_fixture_message_id(agent_id: &str, body: &MessageBody) -> String {
    let mut hasher = Sha256::new();
    hasher.update(agent_id.as_bytes());
    hasher.update(b"\0ingress_queue_admission\0");
    hasher.update(serde_json::to_vec(body).expect("message body serialization cannot fail"));
    format!("msg_restart_{:x}", hasher.finalize())
}

async fn seed_scheduler_legacy_adoption_restart_fixture(
    config: &AppConfig,
    agent_id: &str,
    stage: &str,
    objective: String,
) -> Result<SchedulerLegacyAdoptionRestartFixture> {
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

    if stage == "prepare" {
        if !work_items_for_agent(&runtime, agent_id)?.is_empty() {
            return Err(anyhow!(
                "scheduler legacy adoption prepare requires fresh agent state"
            ));
        }
        runtime
            .create_work_item(
                format!("{objective} first"),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await?;
        runtime
            .create_work_item(
                format!("{objective} second"),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await?;
        let report = super::scheduler_recovery_report(
            &runtime.inner.storage,
            &runtime.inner.runtime_db,
            agent_id,
        )?;
        let eligible = report
            .legacy_adoptions
            .iter()
            .filter(|candidate| candidate.eligible && candidate.proposed_command.is_some())
            .collect::<Vec<_>>();
        if eligible.len() != 2 {
            return Err(anyhow!(
                "scheduler legacy adoption expected two eligible candidates, found {}",
                eligible.len()
            ));
        }
        let stale_work_item_id = eligible[1].work_item_id.clone();
        let stale = runtime
            .inner
            .runtime_db
            .work_items()
            .latest(&stale_work_item_id)?
            .ok_or_else(|| anyhow!("legacy adoption stale WorkItem is missing"))?;
        runtime
            .update_work_item_fields(
                stale_work_item_id.clone(),
                Some(format!("{} stale", stale.objective)),
                None,
                None,
                None,
                None,
            )
            .await?;
        let error = super::apply_scheduler_recovery_plan(
            &runtime.inner.storage,
            &runtime.inner.runtime_db,
            agent_id,
            &report,
        )
        .expect_err("stale legacy adoption report must be rejected");
        if !error.to_string().contains("legacy adoption source changed") {
            return Err(error).context("unexpected legacy adoption rollback failure");
        }
        let partition_initialized = runtime
            .inner
            .runtime_db
            .transitions()
            .load_scheduler_protocol_snapshot_if_initialized(agent_id)?
            .is_some();
        if partition_initialized {
            return Err(anyhow!(
                "stale legacy adoption report left partial canonical state"
            ));
        }
        return Ok(SchedulerLegacyAdoptionRestartFixture {
            checkpoint: "legacy_adoption_atomicity".into(),
            stage: stage.into(),
            cut_kind: "atomic_rollback".into(),
            agent_id: agent_id.into(),
            work_item_id: stale_work_item_id,
            precommit_fault_rolled_back: true,
            replay_applied: false,
            replay_exactly_once: false,
            partition_initialized,
            work_status: None,
            focus_work_item_id: None,
        });
    }
    if stage != "replay" && stage != "verify" {
        return Err(anyhow!(
            "scheduler restart checkpoint legacy_adoption_atomicity does not support stage {stage}"
        ));
    }

    let work_items = work_items_for_agent(&runtime, agent_id)?;
    let stale = work_items
        .iter()
        .find(|record| record.objective.ends_with(" stale"))
        .ok_or_else(|| anyhow!("legacy adoption stale WorkItem is missing after restart"))?;
    let report = super::scheduler_recovery_report(
        &runtime.inner.storage,
        &runtime.inner.runtime_db,
        agent_id,
    )?;
    let replay_applied = if stage == "replay" {
        super::apply_scheduler_recovery_plan(
            &runtime.inner.storage,
            &runtime.inner.runtime_db,
            agent_id,
            &report,
        )?
        .0 == 1
    } else {
        false
    };
    let snapshot = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let stale_demand = snapshot
        .work
        .get(&stale.id)
        .ok_or_else(|| anyhow!("legacy adoption stale demand is missing"))?;
    let replay_exactly_once = snapshot.work.len() == work_items.len()
        && work_items.iter().all(|record| {
            snapshot.work.get(&record.id).is_some_and(|demand| {
                demand.metadata_revision == record.revision
                    && demand.scheduling_generation == record.revision.max(1)
                    && demand.status == WorkStatus::Runnable
            })
        });
    if !replay_exactly_once {
        return Err(anyhow!(
            "scheduler legacy adoption did not converge atomically"
        ));
    }
    if stage == "verify"
        && super::apply_scheduler_recovery_plan(
            &runtime.inner.storage,
            &runtime.inner.runtime_db,
            agent_id,
            &report,
        )?
        .0 != 0
    {
        return Err(anyhow!(
            "scheduler legacy adoption verify applied duplicate state"
        ));
    }
    Ok(SchedulerLegacyAdoptionRestartFixture {
        checkpoint: "legacy_adoption_atomicity".into(),
        stage: stage.into(),
        cut_kind: "atomic_rollback".into(),
        agent_id: agent_id.into(),
        work_item_id: stale.id.clone(),
        precommit_fault_rolled_back: stage == "verify",
        replay_applied: stage == "replay" && replay_applied,
        replay_exactly_once,
        partition_initialized: true,
        work_status: Some(work_status_name(&stale_demand.status)),
        focus_work_item_id: snapshot.focus,
    })
}

async fn seed_scheduler_terminal_settlement_restart_fixture(
    config: &AppConfig,
    agent_id: &str,
    stage: &str,
    objective: String,
) -> Result<SchedulerTerminalSettlementRestartFixture> {
    if stage == "prepare" {
        let fixture = seed_scheduler_terminal_recovery_fixture(config, agent_id, objective).await?;
        return Ok(SchedulerTerminalSettlementRestartFixture {
            checkpoint: "turn_terminal_settlement".into(),
            stage: stage.into(),
            cut_kind: "durable_recovery".into(),
            agent_id: fixture.agent_id,
            work_item_id: fixture.work_item_id,
            message_id: fixture.message_id,
            turn_id: fixture.turn_id,
            activation_id: fixture.activation_id,
            recovery_applied: false,
            replay_applied: false,
            replay_exactly_once: false,
            queue_status: fixture.queue_status,
            activation_state: fixture.activation_state,
            slot_state: fixture.slot_state,
            recovery_candidates: 1,
        });
    }
    if stage != "replay" && stage != "verify" {
        return Err(anyhow!(
            "scheduler restart checkpoint turn_terminal_settlement does not support stage {stage}"
        ));
    }

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
    let before = super::scheduler_recovery_report(
        &runtime.inner.storage,
        &runtime.inner.runtime_db,
        agent_id,
    )?;
    let recovery_candidates = before
        .candidates
        .iter()
        .filter(|candidate| candidate.eligible)
        .count();
    let recovered = if stage == "replay" {
        runtime.recover_scheduler_bootstrap_claims().await?
    } else {
        0
    };
    let after = super::scheduler_recovery_report(
        &runtime.inner.storage,
        &runtime.inner.runtime_db,
        agent_id,
    )?;
    let entries = queue_entries_for_agent(&runtime, agent_id)?;
    if entries.len() != 1 {
        return Err(anyhow!(
            "scheduler terminal settlement restart expected one queue entry"
        ));
    }
    let entry = &entries[0];
    let message = runtime
        .inner
        .storage
        .read_message_by_id(&entry.message_id)?
        .ok_or_else(|| anyhow!("scheduler terminal settlement message is missing"))?;
    let work_item_id = message
        .work_item_id
        .clone()
        .ok_or_else(|| anyhow!("scheduler terminal settlement work item is missing"))?;
    let turn_id = message
        .turn_id
        .clone()
        .ok_or_else(|| anyhow!("scheduler terminal settlement turn is missing"))?;
    let activation_id = scheduler_executor::canonical_activation_id(&entry.message_id);
    let snapshot = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let activation = snapshot
        .activations
        .get(&activation_id)
        .ok_or_else(|| anyhow!("scheduler terminal settlement activation is missing"))?;
    let replay_exactly_once = entry.status == QueueEntryStatus::Processed
        && activation.state == ActivationState::Settled
        && snapshot
            .settlements
            .contains_key(&super::canonical_settlement_id(&entry.message_id))
        && matches!(snapshot.slot, ActivationSlot::Idle)
        && after.candidates.is_empty();
    if stage == "replay" && (recovered != 1 || recovery_candidates != 1) {
        return Err(anyhow!(
            "scheduler terminal settlement replay did not apply exactly one recovery"
        ));
    }
    if stage == "verify" && (recovery_candidates != 0 || !replay_exactly_once) {
        return Err(anyhow!(
            "scheduler terminal settlement verify found residual recovery work"
        ));
    }
    if !replay_exactly_once {
        return Err(anyhow!(
            "scheduler terminal settlement restart did not converge exactly once"
        ));
    }
    Ok(SchedulerTerminalSettlementRestartFixture {
        checkpoint: "turn_terminal_settlement".into(),
        stage: stage.into(),
        cut_kind: "durable_recovery".into(),
        agent_id: agent_id.into(),
        work_item_id,
        message_id: entry.message_id.clone(),
        turn_id,
        activation_id,
        recovery_applied: recovered == 1,
        replay_applied: recovered == 1,
        replay_exactly_once,
        queue_status: "processed".into(),
        activation_state: "settled".into(),
        slot_state: "idle".into(),
        recovery_candidates,
    })
}

async fn seed_scheduler_settlement_delivery_restart_fixture(
    config: &AppConfig,
    agent_id: &str,
    stage: &str,
    objective: String,
) -> Result<SchedulerSettlementDeliveryRestartFixture> {
    if stage == "prepare" {
        let fixture = seed_scheduler_terminal_recovery_fixture(config, agent_id, objective).await?;
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())?;
        let agent_home = config.agent_root_dir().join(agent_id);
        let runtime = RuntimeHandle::new_offline_with_runtime_db(
            agent_id,
            agent_home,
            InitialWorkspaceBinding::Detached,
            runtime_db,
        )?;
        let report = super::scheduler_recovery_report(
            &runtime.inner.storage,
            &runtime.inner.runtime_db,
            agent_id,
        )?;
        let candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.message_id == fixture.message_id && candidate.eligible)
            .ok_or_else(|| {
                anyhow!("scheduler settlement delivery recovery candidate is missing")
            })?;
        let command = match candidate.proposed_commands.as_slice() {
            [crate::domain::scheduler_protocol::ProtocolCommand::SettleActivation(command)] => {
                crate::domain::scheduler_protocol::ProtocolCommand::SettleActivation(
                    command.clone(),
                )
            }
            commands => {
                return Err(anyhow!(
                    "scheduler settlement delivery expected one settlement command, found {}",
                    commands.len()
                ));
            }
        };
        let commit = runtime
            .inner
            .runtime_db
            .transitions()
            .commit_scheduler_protocol_command(agent_id, &command, None)?;
        if !commit.applied || commit.replayed {
            return Err(anyhow!(
                "scheduler settlement delivery did not commit a fresh canonical settlement"
            ));
        }
        let snapshot = runtime
            .inner
            .runtime_db
            .transitions()
            .load_scheduler_protocol_snapshot(agent_id)?;
        let activation = snapshot
            .activations
            .get(&fixture.activation_id)
            .ok_or_else(|| anyhow!("scheduler settlement delivery activation is missing"))?;
        let queue_status = runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest_all()?
            .into_iter()
            .find(|entry| entry.message_id == fixture.message_id)
            .map(|entry| entry.status)
            .ok_or_else(|| anyhow!("scheduler settlement delivery queue entry is missing"))?;
        if activation.state != ActivationState::Settled
            || !snapshot
                .settlements
                .contains_key(&super::canonical_settlement_id(&fixture.message_id))
            || !matches!(snapshot.slot, ActivationSlot::Idle)
            || queue_status != QueueEntryStatus::Dequeued
        {
            return Err(anyhow!(
                "scheduler settlement delivery prepare did not preserve the canonical/legacy split"
            ));
        }
        return Ok(SchedulerSettlementDeliveryRestartFixture {
            checkpoint: "settlement_delivery".into(),
            stage: stage.into(),
            cut_kind: "durable_recovery".into(),
            agent_id: fixture.agent_id,
            work_item_id: fixture.work_item_id,
            message_id: fixture.message_id,
            turn_id: fixture.turn_id,
            activation_id: fixture.activation_id,
            canonical_settlement_committed: true,
            recovery_applied: false,
            replay_applied: false,
            replay_exactly_once: false,
            queue_status: "dequeued".into(),
            activation_state: "settled".into(),
            slot_state: "idle".into(),
            recovery_candidates: 1,
        });
    }
    if stage != "replay" && stage != "verify" {
        return Err(anyhow!(
            "scheduler restart checkpoint settlement_delivery does not support stage {stage}"
        ));
    }

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
    let before = super::scheduler_recovery_report(
        &runtime.inner.storage,
        &runtime.inner.runtime_db,
        agent_id,
    )?;
    let recovery_candidates = before
        .candidates
        .iter()
        .filter(|candidate| candidate.eligible)
        .count();
    let recovered = if stage == "replay" {
        runtime.recover_scheduler_bootstrap_claims().await?
    } else {
        0
    };
    let after = super::scheduler_recovery_report(
        &runtime.inner.storage,
        &runtime.inner.runtime_db,
        agent_id,
    )?;
    let entries = queue_entries_for_agent(&runtime, agent_id)?;
    if entries.len() != 1 {
        return Err(anyhow!(
            "scheduler settlement delivery expected one queue entry"
        ));
    }
    let entry = &entries[0];
    let message = runtime
        .inner
        .storage
        .read_message_by_id(&entry.message_id)?
        .ok_or_else(|| anyhow!("scheduler settlement delivery message is missing"))?;
    let work_item_id = message
        .work_item_id
        .clone()
        .ok_or_else(|| anyhow!("scheduler settlement delivery work item is missing"))?;
    let turn_id = message
        .turn_id
        .clone()
        .ok_or_else(|| anyhow!("scheduler settlement delivery turn is missing"))?;
    let activation_id = scheduler_executor::canonical_activation_id(&entry.message_id);
    let snapshot = runtime
        .inner
        .runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot(agent_id)?;
    let activation = snapshot
        .activations
        .get(&activation_id)
        .ok_or_else(|| anyhow!("scheduler settlement delivery activation is missing"))?;
    let replay_exactly_once = entry.status == QueueEntryStatus::Processed
        && activation.state == ActivationState::Settled
        && snapshot
            .settlements
            .contains_key(&super::canonical_settlement_id(&entry.message_id))
        && matches!(snapshot.slot, ActivationSlot::Idle)
        && after.candidates.is_empty();
    if stage == "replay" && (recovered != 1 || recovery_candidates != 1) {
        return Err(anyhow!(
            "scheduler settlement delivery replay did not reconcile exactly one legacy claim"
        ));
    }
    if stage == "verify" && (recovery_candidates != 0 || !replay_exactly_once) {
        return Err(anyhow!(
            "scheduler settlement delivery verify found residual recovery work"
        ));
    }
    if !replay_exactly_once {
        return Err(anyhow!(
            "scheduler settlement delivery did not converge exactly once"
        ));
    }
    Ok(SchedulerSettlementDeliveryRestartFixture {
        checkpoint: "settlement_delivery".into(),
        stage: stage.into(),
        cut_kind: "durable_recovery".into(),
        agent_id: agent_id.into(),
        work_item_id,
        message_id: entry.message_id.clone(),
        turn_id,
        activation_id,
        canonical_settlement_committed: true,
        recovery_applied: recovered == 1,
        replay_applied: recovered == 1,
        replay_exactly_once,
        queue_status: "processed".into(),
        activation_state: "settled".into(),
        slot_state: "idle".into(),
        recovery_candidates,
    })
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
    runtime
        .emit_system_tick_from_work_queue(&work_item, "queued_available", Some(&decision))
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
