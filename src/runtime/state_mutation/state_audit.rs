//! The lightweight State notification joins its original State/Queue ACK.
use super::*;

/// A material carrier, not a business or metadata authority capability.
/// It retains no Root, Runtime or database reference.
pub(in crate::runtime) struct OriginalStateChangedAudit {
    event: AuditEvent,
    _material: Arc<super::super::input_evidence_budget::BudgetViewGuard>,
}
impl OriginalStateChangedAudit {
    pub(in crate::runtime) fn event(&self) -> &AuditEvent {
        &self.event
    }
}
impl RuntimeHandle {
    pub(in crate::runtime) fn prepare_original_state_changed_audit(
        &self,
        state: &AgentState,
        original_audits: &[AuditEvent],
    ) -> Result<OriginalStateChangedAudit> {
        let (material, _) = self.original_state_notification_material(state, &original_audits)?;
        // This input-copy guard precedes even a new scope/address allocation.
        // Business authorization remains the original held State/Queue actor.
        let guard = self.inner.input_evidence_budget.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: material.0,
                metadata_estimate_bytes: 0,
            },
        )?;
        // The original ID/time/payload are now frozen before business SQL.
        let payload = AgentStateChangedEvent::from_state(state);
        let event = AuditEvent::typed(RuntimeEventKind::AgentStateChanged, &payload)?;
        Ok(OriginalStateChangedAudit {
            event,
            _material: guard,
        })
    }
    pub(in crate::runtime) fn original_state_notification_material<T: serde::Serialize>(
        &self,
        state: &AgentState,
        extra: &T,
    ) -> Result<((u64, usize), (u64, usize))> {
        anyhow::ensure!(
            state.id == self.inner.mutation_owner.agent_id(),
            "State notification belongs to another original Runtime"
        );
        let active = state.active_workspace_entry.as_ref();
        let terminal = state.last_turn_terminal.as_ref().map(|record| {
            (
                record.kind,
                &record.reason,
                &record.no_brief_reason,
                record.completed_at,
                record.duration_ms,
            )
        });
        // Borrow only the actual lightweight wire fields. WorkingMemory and
        // other State collections are not copied into this notification.
        let projection = (
            &state.id,
            &state.status,
            state.sleeping_until,
            &state.current_run_id,
            state.pending,
            &state.last_wake_reason,
            state.turn_index,
            &state.current_turn_id,
            &state.current_turn_work_item_id,
            &state.current_work_item_id,
            terminal,
            &state.attached_workspaces,
            active.map(|entry| entry.workspace_id.as_str()),
            active.map(|entry| entry.execution_root_id.as_str()),
            active.and_then(|entry| entry.occupancy_id.as_deref()),
            state.worktree_session.is_some(),
        );
        let (encoded, nodes) =
            super::super::input_evidence_budget::borrowed_json_material(&projection)?;
        let (prior_bytes, prior_nodes) =
            super::super::input_evidence_budget::borrowed_json_material(extra)?;
        let cost = |bytes: u64, records: usize| -> Result<(u64, usize)> {
            Ok((
                bytes
                    .checked_add(
                        u64::try_from(records)?
                            .checked_mul(256)
                            .context("State notification native bytes overflow")?,
                    )
                    .and_then(|n| n.checked_add(4096))
                    .and_then(|n| n.checked_mul(4))
                    .context("State notification copies overflow")?,
                records
                    .checked_add(128)
                    .and_then(|n| n.checked_mul(4))
                    .context("State notification native records overflow")?,
            ))
        };
        let material = cost(encoded, nodes)?;
        let prior = cost(prior_bytes, prior_nodes)?;
        let limits = self.inner.input_evidence_budget.limits();
        let byte_limit = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .context("State notification allowance overflow")?
            .min(64 * 1024 * 1024);
        let record_limit = usize::try_from(limits.record_limit)?.min(65536);
        anyhow::ensure!(
            material
                .0
                .checked_add(prior.0)
                .is_some_and(|n| n <= byte_limit)
                && material
                    .1
                    .checked_add(prior.1)
                    .is_some_and(|n| n <= record_limit),
            super::super::current_read_facts::CurrentReadError::ResourceExhausted
        );
        Ok((material, prior))
    }

    pub(in crate::runtime) fn prepare_original_dispatch_audits(
        &self,
        state: &AgentState,
        message: Option<&MessageEnvelope>,
        replay: Option<(&str, Option<&TurnRecord>)>,
    ) -> Result<OriginalDispatchAudits> {
        let source = message.map(|message| {
            (
                &message.agent_id,
                &message.id,
                &state.current_turn_id,
                &message.kind,
                &state.current_run_id,
                state.turn_index,
            )
        });
        let original_replay = replay.map(|(source_id, source_turn)| {
            (
                source_id,
                &state.current_turn_id,
                source_turn.and_then(|turn| turn.current_work_item_id.as_ref()),
                source_turn.and_then(|turn| turn.terminal.as_ref()),
            )
        });
        let (body, extra) =
            self.original_state_notification_material(state, &(source, original_replay))?;
        // Both the State DTO and original dispatch annotations are new owned
        // copies in this phase, so the same guard includes them before Value.
        let material = self.inner.input_evidence_budget.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: body
                    .0
                    .checked_add(extra.0)
                    .context("dispatch notification material overflow")?,
                metadata_estimate_bytes: 0,
            },
        )?;
        let mut events = vec![AuditEvent::typed(
            RuntimeEventKind::AgentStateChanged,
            &AgentStateChangedEvent::from_state(state),
        )?];
        if let Some(message) = message {
            events.push(AuditEvent::legacy(
                "turn_started",
                serde_json::json!({
                    "agent_id": message.agent_id,
                    "message_id": message.id,
                    "turn_id": state.current_turn_id,
                    "message_kind": message.kind,
                    "run_id": state.current_run_id,
                    "turn_index": state.turn_index,
                }),
            ));
            if let Some((source_turn_id, source_turn)) = replay {
                events.push(AuditEvent::legacy("turn_replay_started",serde_json::json!({
                    "agent_id": message.agent_id,
                    "message_id": message.id,
                    "source_turn_id": source_turn_id,
                    "replay_turn_id": state.current_turn_id,
                    "reason": "interrupted_queue_claim_reentry",
                    "source_work_item_id": source_turn.and_then(|turn|turn.current_work_item_id.as_ref()),
                    "prior_terminal": source_turn.and_then(|turn|turn.terminal.as_ref()),
                })));
            }
        }
        Ok(OriginalDispatchAudits {
            events,
            _material: material,
        })
    }
}

/// Original dispatch notifications only; preserves their guard through ACK.
pub(in crate::runtime) struct OriginalDispatchAudits {
    events: Vec<AuditEvent>,
    _material: Arc<super::super::input_evidence_budget::BudgetViewGuard>,
}
impl OriginalDispatchAudits {
    pub(in crate::runtime) fn events(&self) -> &[AuditEvent] {
        &self.events
    }
}
