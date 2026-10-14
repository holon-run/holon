//! Pure operator Stop planning. SQL ACK and physical actions remain separate.
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};

use super::{
    command_task::ManagedTaskHandle,
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_task::{CanonicalTaskFact, QualifiedTaskFact},
};
use crate::{
    runtime_db::task_stop::{
        PreparedTaskStop, TaskStopChildTarget, TaskStopResultPlan, TaskStopSignal,
        TaskStopWorktreeTarget,
    },
    runtime_error::{RuntimeError, RuntimeErrorDomain},
    types::{
        AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageEnvelope,
        MessageKind, MessageOrigin, Priority, TaskKind, TaskRecord, TaskStatus,
    },
};

pub(super) struct FrozenTaskStopIntent {
    pub(super) task: TaskRecord,
    pub(super) intent: PreparedTaskStop,
    _material: Arc<BudgetViewGuard>,
}
impl FrozenTaskStopIntent {
    /// The caller holds the original mutation reservation through handle lookup.
    /// A late handle registration uses that same owner and must see this ACK.
    pub(super) fn prepare(
        qualified: &QualifiedTaskFact,
        handle: Option<&ManagedTaskHandle>,
        owner_epoch: u64,
        child: Option<&TaskStopChildTarget>,
        at: DateTime<Utc>,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<Self> {
        let CanonicalTaskFact::Task(original) = &qualified.fact else {
            anyhow::bail!("TaskStop requires the original qualified Task");
        };
        ensure!(
            qualified.locator.scope.agent_id == original.agent_id
                && qualified.locator.key.family == super::resident_task::TaskFamily::Task
                && qualified.locator.key.id == original.id
                && owner_epoch > 0,
            "TaskStop original namespace/owner differs"
        );
        let bytes = serialized_bytes(&(qualified, child, at, owner_epoch))?
            .checked_mul(8)
            .and_then(|n| n.checked_add(4096))
            .context("TaskStop frozen intent material overflow")?;
        let limits = ledger.limits();
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("TaskStop frozen intent limit overflow")?
            .min(64 * 1024 * 1024);
        ensure!(
            bytes <= byte_limit && limits.record_limit >= 8,
            "TaskStop frozen intent ResourceExhausted before clone"
        );
        let material = ledger.register_owned_bytes(
            &qualified.locator.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let lease = handle.and_then(ManagedTaskHandle::control_lease);
        ensure!(
            super::task_generation::TaskRejoinGeneration::of(original).is_some()
                && lease
                    .as_ref()
                    .is_none_or(|lease| lease.matches_task_generation(original)),
            "TaskStop handle belongs to another original generation"
        );
        let signal = match (original.kind, handle, lease.as_ref()) {
            (_, None, None) => TaskStopSignal::NoHandle,
            (TaskKind::CommandTask, Some(ManagedTaskHandle::Command(_)), Some(_)) => {
                if original
                    .detail
                    .as_ref()
                    .and_then(|d| d.get("cancel_requested"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    TaskStopSignal::Force
                } else {
                    TaskStopSignal::Cancel
                }
            }
            (kind, Some(ManagedTaskHandle::Async(_)), Some(_)) if kind != TaskKind::CommandTask => {
                TaskStopSignal::Abort
            }
            _ => {
                return Err(RuntimeError::new(
                    RuntimeErrorDomain::Task,
                    "task_handle_type_mismatch",
                    format!(
                        "task {} has an unexpected handle type or original lease",
                        original.id
                    ),
                )
                .with_safe_context("task_id", &original.id)
                .into())
            }
        };
        let status = if original.kind == TaskKind::CommandTask && signal != TaskStopSignal::NoHandle
        {
            TaskStatus::Cancelling
        } else {
            TaskStatus::Cancelled
        };
        let status_text = if status == TaskStatus::Cancelling {
            "cancelling"
        } else {
            "cancelled"
        };
        let mut task = original.clone();
        task.status = status;
        task.updated_at = at;
        task.parent_message_id = None;
        task.summary
            .get_or_insert_with(|| format!("task {status_text}"));
        let detail = task.detail.get_or_insert_with(|| serde_json::json!({}));
        let fields = detail
            .as_object_mut()
            .context("TaskStop original detail is not an object")?;
        fields.insert("task_status".into(), serde_json::json!(status_text));
        if original.kind == TaskKind::CommandTask {
            fields.insert("cancel_requested".into(), serde_json::json!(true));
            fields.insert("accepts_input".into(), serde_json::json!(false));
            fields.insert("input_target".into(), serde_json::Value::Null);
            if matches!(signal, TaskStopSignal::Force | TaskStopSignal::NoHandle) {
                fields.insert("force_stop_requested".into(), serde_json::json!(true));
            }
        }
        let needs_child = original.is_child_agent_task()
            && original
                .detail
                .as_ref()
                .and_then(|d| d.get("child_agent_id"))
                .and_then(serde_json::Value::as_str)
                .is_some();
        ensure!(
            child.is_none() || needs_child,
            "TaskStop unexpected child target"
        );
        let child_unavailable = (needs_child && child.is_none())
            .then(|| "original child identity unavailable".to_owned());
        let worktree = if original.is_worktree_child_agent_task() && child.is_some() {
            let artifact = original.detail.as_ref().and_then(|d| d.get("worktree"));
            artifact.and_then(|artifact| {
                let path = artifact.get("worktree_path")?.as_str()?;
                let branch = artifact.get("worktree_branch")?.as_str()?;
                if path.is_empty() || branch.is_empty() {
                    return None;
                }
                Some(TaskStopWorktreeTarget {
                    path: path.to_owned(),
                    branch: branch.to_owned(),
                })
            })
        } else {
            None
        };
        let worktree_unavailable = (original.is_worktree_child_agent_task() && worktree.is_none())
            .then(|| "original worktree artifact or child identity unavailable".to_owned());
        let result = if original.kind == TaskKind::CommandTask {
            None
        } else {
            let mut message = MessageEnvelope::new(
                &task.agent_id,
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: task.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Next,
                MessageBody::Text {
                    text: "task cancelled by operator".into(),
                },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            );
            message.task_id = Some(task.id.clone());
            message.work_item_id = task.work_item_id.clone();
            message.created_at = at;
            message.metadata = Some(serde_json::json!({
                "task_id":&task.id,"task_kind":task.kind,"task_status":"cancelled",
                "task_summary":&task.summary,"task_detail":&task.detail,"task_recovery":&task.recovery,
            }));
            Some(TaskStopResultPlan {
                phase_nonce: crate::ids::audit_event_id(),
                message,
            })
        };
        let intent = PreparedTaskStop {
            task: qualified.locator.clone(),
            kind: original.kind,
            created_at: original.created_at,
            rejoin: original.rejoin_fence().ok(),
            owner_epoch,
            signal,
            lease_id: lease.as_ref().map(|lease| lease.lease_id().to_owned()),
            child: child.cloned(),
            child_unavailable,
            worktree,
            worktree_unavailable,
            result,
        };
        intent.validate_original(original)?;
        Ok(Self {
            task,
            intent,
            _material: material,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::StateMemoryLimits,
        runtime::{
            command_task::CommandTaskHandle,
            control_cancellation::{ControlCancellationRegistry, SharedControlSignal},
            resident_input_evidence::InputEvidenceScope,
            resident_task::{TaskFactLocator, TaskFamily, TaskKey},
        },
    };
    use tokio::sync::{mpsc, oneshot};

    fn fact(kind: TaskKind) -> QualifiedTaskFact {
        let at = "2026-10-01T00:00:00.123456789Z".parse().unwrap();
        QualifiedTaskFact {
            locator: TaskFactLocator {
                scope: InputEvidenceScope {
                    agent_id: "stop-planner".into(),
                    identity_incarnation: 1,
                },
                key: TaskKey::new(TaskFamily::Task, "original-task"),
                revision: 1,
                content_digest: "a".repeat(64),
                projection_digest: "b".repeat(64),
            },
            fact: CanonicalTaskFact::Task(TaskRecord {
                id: "original-task".into(),
                agent_id: "stop-planner".into(),
                kind,
                status: TaskStatus::Running,
                created_at: at,
                updated_at: at,
                parent_message_id: Some("original-parent-message".into()),
                work_item_id: None,
                summary: Some("original task".into()),
                detail: Some(serde_json::json!({"cancel_requested":false})),
                recovery: None,
            }),
            resident_bytes: 1024,
        }
    }
    #[tokio::test]
    async fn new_force_comes_from_canonical_cancel_request_without_sending_either_signal(
    ) -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let mut original = fact(TaskKind::CommandTask);
        let CanonicalTaskFact::Task(task) = &original.fact else {
            unreachable!()
        };
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        let (force_tx, mut force_rx) = oneshot::channel();
        let cancel_tx = SharedControlSignal::new(cancel_tx);
        let force_tx = SharedControlSignal::new(force_tx);
        let lease = ControlCancellationRegistry::default().command(
            task,
            cancel_tx.clone(),
            force_tx.clone(),
        );
        let (input_tx, _) = mpsc::channel(1);
        let handle = ManagedTaskHandle::Command(CommandTaskHandle {
            cancel_tx: Some(cancel_tx),
            force_stop_tx: Some(force_tx),
            input_tx,
            control_lease: Some(lease),
        });
        let at = task.updated_at;
        let first = FrozenTaskStopIntent::prepare(&original, Some(&handle), 1, None, at, &ledger)?;
        assert_eq!(first.intent.signal, TaskStopSignal::Cancel);
        assert_eq!(first.task.status, TaskStatus::Cancelling);
        assert!(first.intent.result.is_none());
        let first_intent = first.intent.clone();
        if let CanonicalTaskFact::Task(task) = &mut original.fact {
            task.detail.as_mut().unwrap()["cancel_requested"] = serde_json::json!(true);
        }
        let force = FrozenTaskStopIntent::prepare(&original, Some(&handle), 1, None, at, &ledger)?;
        assert_eq!(force.intent.signal, TaskStopSignal::Force);
        assert_eq!(
            first.intent, first_intent,
            "later API planning cannot alter original Cancel"
        );
        assert!(matches!(
            cancel_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            force_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        drop(first);
        drop(force);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn absent_monitor_keeps_frozen_result_and_missing_artifact_responsibilities() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let mut original = fact(TaskKind::WorktreeSubagentTask);
        let CanonicalTaskFact::Task(task) = &mut original.fact else {
            unreachable!()
        };
        task.detail.as_mut().unwrap()["child_agent_id"] = serde_json::json!("original-child");
        let at = task.updated_at;
        let frozen = FrozenTaskStopIntent::prepare(&original, None, 1, None, at, &ledger)?;
        assert_eq!(frozen.intent.signal, TaskStopSignal::NoHandle);
        assert_eq!(frozen.task.status, TaskStatus::Cancelled);
        assert!(frozen.intent.child.is_none() && frozen.intent.child_unavailable.is_some());
        assert!(frozen.intent.worktree.is_none() && frozen.intent.worktree_unavailable.is_some());
        let result = frozen
            .intent
            .result
            .as_ref()
            .context("NoHandle must not discard Result obligation")?;
        assert_eq!(result.message.created_at, at);
        assert_eq!(result.message.task_id.as_deref(), Some("original-task"));
        assert_eq!(
            result.message.metadata.as_ref().unwrap()["task_status"],
            "cancelled"
        );
        assert!(!result.phase_nonce.is_empty() && !result.message.id.is_empty());
        drop(frozen);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn frozen_result_preserves_original_work_binding_for_pending_rejoin() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        for work in [None, Some("original-work")] {
            let mut original = fact(TaskKind::AgentMessageWait);
            let CanonicalTaskFact::Task(task) = &mut original.fact else {
                unreachable!()
            };
            task.work_item_id = work.map(str::to_owned);
            let detail = task.detail.as_mut().unwrap().as_object_mut().unwrap();
            detail.insert("rejoin_obligation_id".into(), serde_json::json!(&task.id));
            detail.insert("rejoin_generation".into(), serde_json::json!(7));
            detail.insert(
                "parent_turn_id".into(),
                serde_json::json!("original-parent-turn"),
            );
            // Legacy detail is retained; the canonical Result contract uses the raw field.
            detail.insert(
                "work_item_id".into(),
                serde_json::json!("legacy-detail-work"),
            );
            let at = task.updated_at;
            let frozen = FrozenTaskStopIntent::prepare(&original, None, 1, None, at, &ledger)?;
            let result = frozen
                .intent
                .result
                .as_ref()
                .context("missing frozen Result")?;
            let pending =
                crate::runtime_db::task_result_settlement::TaskResultSettlementRecord::pending(
                    &frozen.task,
                    &result.message,
                    at,
                )?
                .context("original Work binding must produce pending Result")?;
            assert_eq!(result.message.work_item_id.as_deref(), work);
            assert_eq!(pending.work_item_id.as_deref(), work);
            assert_eq!(pending.rejoin_generation, 7);
            assert_eq!(pending.parent_turn_id, "original-parent-turn");
            assert_eq!(pending.message_id, result.message.id);
            assert_eq!(pending.task_id, frozen.task.id);
            drop(frozen);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }
    #[test]
    fn command_without_handle_preserves_force_request_without_fabricating_a_signal() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let original = fact(TaskKind::CommandTask);
        let frozen = FrozenTaskStopIntent::prepare(
            &original,
            None,
            1,
            None,
            "2026-10-01T00:00:00Z".parse()?,
            &ledger,
        )?;
        assert_eq!(frozen.intent.signal, TaskStopSignal::NoHandle);
        assert!(frozen.intent.lease_id.is_none());
        assert_eq!(frozen.task.status, TaskStatus::Cancelled);
        assert_eq!(
            frozen.task.detail.as_ref().unwrap()["force_stop_requested"],
            true
        );
        assert!(frozen.intent.result.is_none());
        Ok(())
    }

    #[test]
    fn oversized_frozen_task_is_rejected_before_owned_plan_material() -> Result<()> {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let mut original = fact(TaskKind::AgentMessageWait);
        let CanonicalTaskFact::Task(task) = &mut original.fact else {
            unreachable!()
        };
        task.summary = Some("large task".repeat(1024 * 1024));
        let at = task.updated_at;
        let error = FrozenTaskStopIntent::prepare(&original, None, 1, None, at, &ledger)
            .err()
            .context("large original Task unexpectedly copied into plan")?;
        assert!(error.to_string().contains("ResourceExhausted before clone"));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
}
