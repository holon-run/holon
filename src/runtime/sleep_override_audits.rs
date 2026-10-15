//! The original lifecycle override retains its pre-tick light observation.
use super::input_evidence_budget::{
    borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
    OwnedBytesEstimate,
};
use super::*;
#[derive(Debug)]
pub(crate) struct OriginalSleepOverrideMaterial {
    scope: resident_input_evidence::InputEvidenceScope,
    owner_epoch: u64,
    ledger: InputEvidenceBudgetLedger,
    light: Arc<BudgetViewGuard>,
    light_records: usize,
    work: Option<(Arc<BudgetViewGuard>, usize)>,
}
impl OriginalSleepOverrideMaterial {
    pub(crate) fn validate_binding(
        &self,
        scope: &resident_input_evidence::InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        anyhow::ensure!(
            self.scope == *scope
                && self.owner_epoch == epoch
                && self.ledger.same_ledger(ledger)
                && self.light.matches_resource_ledger(ledger)
                && self
                    .work
                    .as_ref()
                    .is_none_or(|(g, _)| g.matches_resource_ledger(ledger)),
            "Sleep override material differs from its original scope/epoch/ledger"
        );
        Ok(())
    }
    pub(crate) fn owner_epoch(&self) -> u64 {
        self.owner_epoch
    }
    pub(crate) fn material_estimate(&self) -> Result<(usize, usize)> {
        let mut cost = (
            usize::try_from(self.light.owned_material_bytes()?)?,
            self.light_records,
        );
        if let Some((guard, records)) = &self.work {
            cost.0 = cost
                .0
                .checked_add(usize::try_from(guard.owned_material_bytes()?)?)
                .context("Sleep Work bytes overflow")?;
            cost.1 = cost
                .1
                .checked_add(*records)
                .context("Sleep Work nodes overflow")?;
        }
        Ok(cost)
    }
    fn light_only(&self) -> Arc<Self> {
        Arc::new(Self {
            scope: self.scope.clone(),
            owner_epoch: self.owner_epoch,
            ledger: self.ledger.clone(),
            light: self.light.clone(),
            light_records: self.light_records,
            work: None,
        })
    }
}
pub(super) struct OriginalSleepOverrideObservation {
    pub(super) work_item: WorkItemRecord,
    pub(super) generation: Option<u64>,
    pub(super) reason: &'static str,
    projection: AgentStateChangedEvent,
    work_id: String,
    pub(super) material: Arc<OriginalSleepOverrideMaterial>,
}
impl RuntimeHandle {
    pub(super) fn prepare_original_sleep_override_observation(
        &self,
        read: &state_mutation::PreparedSchedulerRead,
        work: &WorkItemRecord,
        generation: Option<u64>,
        reason: &'static str,
    ) -> Result<OriginalSleepOverrideObservation> {
        let prior = read.material_estimate()?;
        let (body, extra) = self.original_state_notification_material(
            &read.state,
            &(
                &read.state.status,
                &read.state.current_run_id,
                &work.id,
                reason,
            ),
        )?;
        let light = (
            body.0
                .checked_add(extra.0)
                .context("Sleep light bytes overflow")?,
            body.1
                .checked_add(extra.1)
                .context("Sleep light nodes overflow")?,
        );
        let (encoded, nodes) = borrowed_json_material(work)?;
        let work_cost = (
            encoded
                .checked_add(
                    u64::try_from(nodes)?
                        .checked_mul(256)
                        .context("Sleep Work native overflow")?,
                )
                .and_then(|n| n.checked_add(4096))
                .and_then(|n| n.checked_mul(8))
                .context("Sleep Work copies overflow")?,
            nodes
                .checked_add(128)
                .and_then(|n| n.checked_mul(8))
                .context("Sleep Work nodes overflow")?,
        );
        let limits = self.inner.input_evidence_budget.limits();
        anyhow::ensure!(
            prior
                .0
                .checked_add(light.0)
                .and_then(|n| n.checked_add(work_cost.0))
                .is_some_and(|n| n
                    <= limits
                        .history_per_agent_bytes
                        .saturating_add(limits.pending_per_agent_bytes)
                        .min(64 * 1024 * 1024))
                && prior
                    .1
                    .checked_add(light.1)
                    .and_then(|n| n.checked_add(work_cost.1))
                    .is_some_and(
                        |n| n <= usize::try_from(limits.record_limit.min(65536)).unwrap_or(0)
                    ),
            current_read_facts::CurrentReadError::ResourceExhausted
        );
        let ledger = &self.inner.input_evidence_budget;
        let guard = |bytes| {
            ledger.register_host_owned_bytes(
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: bytes,
                    metadata_estimate_bytes: 0,
                },
            )
        };
        let light_guard = guard(light.0)?;
        let work_guard = guard(work_cost.0)?;
        let material = Arc::new(OriginalSleepOverrideMaterial {
            scope: read.work.scope.clone(),
            owner_epoch: self.instance_owner_epoch(),
            ledger: ledger.clone(),
            light: light_guard,
            light_records: light.1,
            work: Some((work_guard, work_cost.1)),
        });
        material.validate_binding(&read.work.scope, self.instance_owner_epoch(), ledger)?;
        Ok(OriginalSleepOverrideObservation {
            work_item: work.clone(),
            generation,
            reason,
            projection: AgentStateChangedEvent::from_state(&read.state),
            work_id: work.id.clone(),
            material,
        })
    }
    pub(super) async fn start_owned_sleep_override(
        &self,
        observation: OriginalSleepOverrideObservation,
        decision: super::scheduler::SchedulerDecision,
    ) -> Result<()> {
        let runtime = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.spawn_owned_mutation(async move {
            let result = async {
                runtime
                    .emit_system_tick_from_original_sleep_override(&observation, Some(&decision))
                    .await?;
                runtime
                    .finish_owned_sleep_override_observation(observation)
                    .await
            }
            .await;
            let _ = send.send(result);
        })?;
        receive
            .await
            .context("original Sleep override worker ended before its ACK")?
    }
    fn freeze_original_sleep_override_audits(
        &self,
        observation: OriginalSleepOverrideObservation,
    ) -> Result<([AuditEvent; 2], Arc<OriginalSleepOverrideMaterial>)> {
        // Called only after the actual original Tick ACK succeeded. The Work
        // DTO and its guard end before this independent observation phase.
        let OriginalSleepOverrideObservation {
            work_item,
            generation: _,
            reason,
            projection,
            work_id,
            material,
        } = observation;
        let light = material.light_only();
        drop((work_item, material));
        light.validate_binding(
            &light.scope,
            self.instance_owner_epoch(),
            &self.inner.input_evidence_budget,
        )?;
        let events = [
            AuditEvent::legacy(
                "scheduler_posture_decision",
                serde_json::json!({
                    "boundary":"lifecycle_sleep","reason":"sleep_overridden_runnable_work",
                    "previous_status":projection.status,"next_status":projection.status,
                    "evidence":[format!("previous_run_id={:?}",projection.current_run_id),"sleeping_until=None".to_string(),format!("work_item_id={work_id}"),format!("work_queue_reason={reason}"),"work_item_scheduling_state=Runnable".to_string()]
                }),
            ),
            AuditEvent::typed(RuntimeEventKind::AgentStateChanged, &projection)?,
        ];
        Ok((events, light))
    }
    pub(super) async fn finish_owned_sleep_override_observation(
        &self,
        observation: OriginalSleepOverrideObservation,
    ) -> Result<()> {
        let (events, light) = self.freeze_original_sleep_override_audits(observation)?;
        let mut owner = self.begin_mutation().await?;
        let root = owner.capture_published_root()?;
        root.active_identity()?;
        let p =
            crate::runtime_db::transitions::PreparedInputTransition::runtime_sleep_override_audits(
                owner.input_evidence_scope(),
                events,
                root.state.clone(),
                light,
                self.instance_owner_epoch(),
                &self.inner.input_evidence_budget,
                None,
            )?;
        drop(root);
        owner.commit_prepared_input_transition(p).await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "sleep_override_audit_tests.rs"]
mod tests;
