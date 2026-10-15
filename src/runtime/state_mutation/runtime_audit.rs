//! Normal Active Audit remains owned once its original Event is submitted.
use super::*;
impl RuntimeHandle {
    fn observe_original_runtime_audit(
        &self,
        event: &AuditEvent,
    ) -> Result<Arc<super::super::input_evidence_budget::BudgetViewGuard>> {
        let (encoded, nodes) = super::super::input_evidence_budget::borrowed_json_material(event)?;
        let bytes = encoded
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Runtime Audit native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(4096))
            .context("Runtime Audit envelope overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        anyhow::ensure!(
            bytes
                <= limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("Runtime Audit cap overflow")?
                    .min(64 * 1024 * 1024)
                && nodes.checked_add(32).is_some_and(
                    |n| n <= usize::try_from(limits.record_limit.min(65536)).unwrap_or(0)
                ),
            super::super::current_read_facts::CurrentReadError::ResourceExhausted
        );
        Ok(self.inner.input_evidence_budget.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?)
    }
    pub(in crate::runtime) async fn append_runtime_audit_event(
        &self,
        event: AuditEvent,
    ) -> Result<()> {
        // The original ID/time and actual input guard precede registration.
        // Caller cancellation cannot detach an accepted Audit continuation.
        let material = self.observe_original_runtime_audit(&event)?;
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let result = runtime.append_original_runtime_audit(event, material).await;
            if let Err(error) = &result {
                tracing::warn!(error=%error,"original Runtime Audit continuation failed");
            }
            let _ = send.send(result);
        })?;
        receive
            .await
            .context("original Runtime Audit worker ended before its ACK")?
    }
    /// The original Timer or dispatch worker already owns this continuation.
    /// It can finish while the owned-task registry is closing; no new admission.
    pub(in crate::runtime) async fn append_owned_runtime_audit_event(
        &self,
        event: AuditEvent,
    ) -> Result<()> {
        let material = self.observe_original_runtime_audit(&event)?;
        self.append_original_runtime_audit(event, material).await
    }
    pub(in crate::runtime) async fn append_owned_scheduler_decision(
        &self,
        decision: &super::super::scheduler::SchedulerDecision,
    ) -> Result<bool> {
        let (encoded, nodes) = super::super::input_evidence_budget::borrowed_json_material(&(
            decision.kind.as_str(),
            &decision.reason,
            decision.model_reentry,
            decision.liveness_only,
            &decision.message_id,
            &decision.work_item_id,
            &decision.task_id,
            &decision.boundary,
            &decision.evidence,
        ))?;
        let bytes = encoded
            .checked_add(
                u64::try_from(nodes)?
                    .checked_mul(256)
                    .context("Scheduler decision native bytes overflow")?,
            )
            .and_then(|n| n.checked_add(4096))
            .and_then(|n| n.checked_mul(4))
            .context("Scheduler decision copies overflow")?;
        let records = nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(4))
            .context("Scheduler decision copy nodes overflow")?;
        let limits = self.inner.input_evidence_budget.limits();
        let allowed = usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .context("Scheduler decision cap overflow")?,
        )?
        .min(64 * 1024 * 1024);
        let allowed_records = usize::try_from(limits.record_limit)?.min(65536);
        let prior = usize::try_from(bytes)?;
        anyhow::ensure!(
            prior < allowed && records < allowed_records,
            super::super::current_read_facts::CurrentReadError::ResourceExhausted
        );
        let _material = self.inner.input_evidence_budget.register_host_owned_bytes(
            super::super::input_evidence_budget::InputMemoryCategory::Candidate,
            super::super::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            },
        )?;
        let mut mutation = self.begin_mutation().await?;
        let root = mutation.capture_published_root()?;
        root.active_identity()?;
        let mut request = super::super::current_read_facts::CurrentReadRequest::current(
            allowed - prior,
            allowed_records - records,
        );
        request.include_pending_queue = false;
        request.terminal_queue_limit = 0;
        request.event_limit = 32;
        request.event_count_limit = 32;
        let current = root
            .current_reads
            .as_ref()
            .ok_or(super::super::current_read_facts::CurrentReadError::NeedsPreparation)?;
        current.validate_state(&root.state)?;
        current.validate_identity(root.active_identity()?)?;
        let selected = current.capture(&request)?;
        let events = super::super::scheduler::scheduler_decision_events(&root.state.id, decision)?;
        let duplicate = super::super::scheduler::recent_scheduler_decision_is_duplicate_iter(
            selected
                .events
                .iter()
                .filter_map(|record| match &record.fact {
                    super::super::current_read_facts::CurrentReadFact::Event(event) => Some(event),
                    _ => None,
                }),
            &events[1],
        );
        if duplicate {
            return Ok(false);
        }
        let expected = root.state.clone();
        // No selected32 temporary or Root survives into the actual SQL phase.
        drop((selected, root));
        let nonce = format!("runtime-scheduler-audit:{}", events[0].id);
        let prepared =
            crate::runtime_db::transitions::PreparedInputTransition::runtime_scheduler_audit(
                mutation.input_evidence_scope(),
                nonce,
                events,
                expected,
                None,
            )?;
        mutation.commit_prepared_input_transition(prepared).await?;
        Ok(true)
    }
    async fn append_original_runtime_audit(
        &self,
        event: AuditEvent,
        _material: Arc<super::super::input_evidence_budget::BudgetViewGuard>,
    ) -> Result<()> {
        let mut mutation = self.begin_mutation().await?;
        mutation.commit_original_runtime_audit(event).await
    }
}
impl RuntimeMutation {
    /// The original owner is already held and its durable State is unchanged.
    /// Keep run handles and the original reservation across the Audit ACK.
    pub(in crate::runtime) async fn append_held_runtime_audit_event(
        &mut self,
        event: AuditEvent,
    ) -> Result<()> {
        let _material = self.runtime.observe_original_runtime_audit(&event)?;
        self.commit_original_runtime_audit(event).await
    }
    async fn commit_original_runtime_audit(&mut self, event: AuditEvent) -> Result<()> {
        let root = self.capture_published_root()?;
        root.active_identity()?;
        let nonce = format!("runtime-audit:{}", event.id);
        let prepared = crate::runtime_db::transitions::PreparedInputTransition::runtime_audit(
            self.input_evidence_scope(),
            nonce,
            event,
            root.state.clone(),
            None,
        )?;
        self.commit_prepared_input_transition(prepared).await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "runtime_audit_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "runtime_scheduler_audit_tests.rs"]
mod scheduler_audit_tests;

#[cfg(test)]
#[path = "lifecycle_observation_audit_tests.rs"]
mod lifecycle_observation_audit_tests;
