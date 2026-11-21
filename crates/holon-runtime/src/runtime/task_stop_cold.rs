//! A Host-owned continuation has no in-process monitor or signal authority.
use crate::{
    host::RuntimeHostBridge,
    runtime::input_evidence_budget::BudgetViewGuard,
    runtime_db::{
        task_snapshot::TaskDomainLoadRequest,
        task_stop::{
            TaskStopChildState, TaskStopPhysicalState, TaskStopProgressAction, TaskStopRecord,
            TaskStopResultState, TaskStopWorktreeState,
        },
        transitions::PreparedInputTransition,
    },
};
use anyhow::{Context, Result};
use chrono::Utc;
use std::sync::Arc;

fn failed(
    error: anyhow::Error,
    original: &TaskStopRecord,
    phase: &'static str,
    any_failed: &mut bool,
) {
    tracing::warn!(effect_seq=original.effect_seq,phase,error=%error,
        "original cold TaskStop facet remains durable");
    *any_failed = true;
}

/// Caller registered owned work before awaiting this function. Each facet uses
/// its original whole ACK; no new Runtime, latest handle or fabricated exit.
pub(crate) async fn continue_original_task_stop_metadata(
    bridge: &RuntimeHostBridge,
    original: Arc<TaskStopRecord>,
    source_material: Arc<BudgetViewGuard>,
    request: TaskDomainLoadRequest,
) -> Result<()> {
    let _source_material = source_material;
    let ledger = bridge.input_evidence_budget()?;
    let mut any_failed = false;
    // Result is attempted first and never depends on physical/child/worktree.
    match bridge
        .read_original_task_stop_metadata(&original, &request)
        .await
    {
        Ok(current) if current.record().progress.result == TaskStopResultState::Pending => {
            let result = async {
                let prepared =
                    PreparedInputTransition::task_stop_result(current.record(), &request, &ledger)?;
                let read = bridge
                    .commit_task_stop_phase_metadata(prepared, request.clone())
                    .await?;
                anyhow::ensure!(
                    read.ack.original_nonce == original.prepared.command_nonce()
                        && read.ack.binding == original.receipt_binding()
                        && read.ack.result.is_some(),
                    "cold TaskStop Result ACK differs from original"
                );
                let plan = original
                    .intent
                    .result
                    .as_ref()
                    .context("cold original Result plan missing")?;
                let ack = read
                    .ack
                    .result
                    .as_ref()
                    .context("cold Result ACK missing")?;
                anyhow::ensure!(
                    ack.phase_nonce == plan.phase_nonce,
                    "cold Result phase address changed"
                );
                if let crate::runtime_db::task_stop::TaskStopResultOutcome::Delivered {
                    message_id,
                    ..
                } = &ack.outcome
                {
                    anyhow::ensure!(
                        message_id == &plan.message.id,
                        "cold Result feedback identity changed"
                    );
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                failed(error, &original, "result", &mut any_failed);
            }
        }
        Ok(_) => {}
        Err(error) => failed(error, &original, "result-observation", &mut any_failed),
    }
    match bridge
        .read_original_task_stop_metadata(&original, &request)
        .await
    {
        Ok(current) if current.record().progress.physical == TaskStopPhysicalState::Pending => {
            let result = async {
                let prepared = PreparedInputTransition::task_stop_safe_progress(
                    current.record(),
                    crate::ids::audit_event_id(),
                    Utc::now(),
                    TaskStopProgressAction::PhysicalAttention {
                        reason: "original in-process monitor unavailable after Host takeover"
                            .into(),
                    },
                    &request,
                    &ledger,
                )?;
                let read = bridge
                    .commit_task_stop_phase_metadata(prepared, request.clone())
                    .await?;
                anyhow::ensure!(
                    read.ack.original_nonce == original.prepared.command_nonce()
                        && read.ack.binding == original.receipt_binding()
                        && read.ack.progress.physical == TaskStopPhysicalState::NeedsAttention,
                    "cold physical observation ACK differs from original"
                );
                Ok(())
            }
            .await;
            if let Err(error) = result {
                failed(error, &original, "physical", &mut any_failed);
            }
        }
        Ok(_) => {}
        Err(error) => failed(error, &original, "physical-observation", &mut any_failed),
    }
    let mut producer_exit = None;
    match bridge
        .read_original_task_stop_metadata(&original, &request)
        .await
    {
        Ok(current) if current.record().progress.child == TaskStopChildState::Pending => {
            let result = async {
                // No parent owner is retained across the sorted child group.
                let retired = Arc::new(
                    bridge
                        .stop_private_agent_for_owned_task_stop(&current)
                        .await?,
                );
                producer_exit = retired.producer_exit();
                let fresh = bridge
                    .read_original_task_stop_metadata(&original, &request)
                    .await?;
                if fresh.record().progress.child == TaskStopChildState::Pending {
                    let prepared = PreparedInputTransition::task_stop_child_retired(
                        fresh.record(),
                        crate::ids::audit_event_id(),
                        Utc::now(),
                        retired,
                        &request,
                        &ledger,
                    )?;
                    let read = bridge
                        .commit_task_stop_phase_metadata(prepared, request.clone())
                        .await?;
                    anyhow::ensure!(
                        read.ack.original_nonce == original.prepared.command_nonce()
                            && read.ack.binding == original.receipt_binding()
                            && read.ack.progress.child == TaskStopChildState::RetiredJoined,
                        "cold child retirement ACK differs from original"
                    );
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                failed(error, &original, "child", &mut any_failed);
            }
        }
        Ok(_) => {}
        Err(error) => failed(error, &original, "child-observation", &mut any_failed),
    }
    match bridge
        .read_original_task_stop_metadata(&original, &request)
        .await
    {
        Ok(current) if current.record().progress.worktree == TaskStopWorktreeState::Pending => {
            let result=async {
                if let Some(exit)=producer_exit {
                    anyhow::ensure!(exit.matches(current.record()),"cold worktree exit differs from original");
                    // Root's actual FS/policy/typed phase entrypoint is required;
                    // this leaf never substitutes a successful cleanup DTO.
                    bridge.finish_original_task_stop_worktree(current.shared_record(),exit,request.clone()).await?;
                    let actual=bridge.read_original_task_stop_metadata(&original,&request).await?;
                    anyhow::ensure!(actual.record().progress.worktree!=TaskStopWorktreeState::Pending,
                        "original worktree cleanup returned without a terminal phase ACK");
                } else {
                    let prepared=PreparedInputTransition::task_stop_safe_progress(
                        current.record(),crate::ids::audit_event_id(),Utc::now(),
                        TaskStopProgressAction::WorktreeRetained{
                            reason:"complete original producer exit proof unavailable after Host takeover".into()},
                        &request,&ledger,
                    )?;
                    let read=bridge.commit_task_stop_phase_metadata(prepared,request.clone()).await?;
                    anyhow::ensure!(read.ack.original_nonce==original.prepared.command_nonce()
                        && read.ack.binding==original.receipt_binding()
                        && read.ack.progress.worktree==TaskStopWorktreeState::Retained,
                        "cold retained Worktree ACK differs from original");
                }
                Ok(())
            }.await;
            if let Err(error) = result {
                failed(error, &original, "worktree", &mut any_failed);
            }
        }
        Ok(_) => {}
        Err(error) => failed(error, &original, "worktree-observation", &mut any_failed),
    }
    anyhow::ensure!(
        !any_failed,
        "original cold TaskStop facet failed; durable responsibility retained"
    );
    Ok(())
}
