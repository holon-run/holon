//! Single-flight agent deletion cleanup coordinator.
//!
//! Drives an [`AgentDeletionJob`] through its ordered phases from `Fence` to
//! `Finalize`. Each phase is idempotent: re-running a phase that has already
//! been completed is a safe no-op. On transient failure the job is marked
//! `RetryableFailed` with an actionable `last_error` and persisted retry
//! deadline; a subsequent retry resumes from the failed phase.
//!
//! The coordinator is triggered:
//! - by a coalesced wake after deletion admission;
//! - on daemon startup for crash recovery;
//! - by persisted retry deadlines and a periodic safety sweep.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::host::RuntimeHost;
use crate::types::*;

/// Interval between periodic deletion coordinator sweeps.
const DELETION_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const DELETION_BATCH_LIMIT: usize = 16;
const DELETION_RETRY_BASE: Duration = Duration::from_millis(250);
const DELETION_RETRY_CAP: Duration = Duration::from_secs(30);
const DELETION_COORDINATOR_ERROR_DELAY: Duration = Duration::from_secs(1);
pub(crate) const LEGACY_DELETION_REPAIR_BATCH_LIMIT: usize = 16;

#[derive(Debug, Default)]
pub(crate) struct LegacyDeletionRepairScanOutcome {
    pub(crate) scanned: usize,
    pub(crate) created: usize,
    pub(crate) ambiguous: usize,
    pub(crate) cursor: Option<String>,
}

impl RuntimeHost {
    /// Spawn the background deletion coordinator task.
    ///
    /// The coordinator periodically sweeps for actionable deletion jobs and
    /// drives them to completion. It is cancelled during graceful shutdown.
    pub fn spawn_daemon_deletion_coordinator(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            debug!("deletion coordinator not spawned: no Tokio runtime");
            return;
        }
        let mut coordinator = self.inner.daemon_deletion_handle.lock().unwrap();
        if coordinator.is_some() {
            return;
        }
        let host = self.clone();
        let handle = tokio::spawn(async move {
            host.run_daemon_deletion_coordinator().await;
        });
        *coordinator = Some(handle);
    }

    async fn run_daemon_deletion_coordinator(self) {
        let mut startup_recovery_complete = false;
        loop {
            let mut coordinator_failed = false;
            let mut legacy_scan_has_more = false;
            if !startup_recovery_complete {
                match self
                    .runtime_db()
                    .agent_deletions()
                    .recover_running_jobs(Utc::now())
                {
                    Ok(recovered) => {
                        startup_recovery_complete = true;
                        if recovered > 0 {
                            info!(recovered, "recovered running deletion jobs at startup");
                        }
                    }
                    Err(err) => {
                        coordinator_failed = true;
                        warn!(error = %err, "deletion coordinator startup recovery failed");
                    }
                }
            }
            if startup_recovery_complete {
                match self.scan_legacy_deletion_residue_batch().await {
                    Ok(outcome) => {
                        legacy_scan_has_more = outcome.cursor.is_some();
                        if outcome.created > 0 || outcome.ambiguous > 0 {
                            info!(
                                scanned = outcome.scanned,
                                created = outcome.created,
                                ambiguous = outcome.ambiguous,
                                cursor = outcome.cursor.as_deref().unwrap_or_default(),
                                "legacy deletion residue scan completed"
                            );
                        }
                    }
                    Err(err) => {
                        coordinator_failed = true;
                        warn!(error = %err, "legacy deletion residue scan failed");
                    }
                }
                if let Err(err) = self.drain_due_deletions().await {
                    coordinator_failed = true;
                    warn!(error = %err, "deletion coordinator sweep failed");
                }
            }
            let sleep_for = if coordinator_failed {
                DELETION_COORDINATOR_ERROR_DELAY
            } else if legacy_scan_has_more {
                Duration::ZERO
            } else {
                match self.next_deletion_coordinator_delay() {
                    Ok(delay) => delay.min(DELETION_SWEEP_INTERVAL),
                    Err(err) => {
                        warn!(error = %err, "reading deletion retry deadline failed");
                        DELETION_COORDINATOR_ERROR_DELAY
                    }
                }
            };
            tokio::select! {
                _ = self.inner.daemon_deletion_token.cancelled() => {
                    debug!("deletion coordinator cancelled");
                    break;
                }
                _ = self.inner.daemon_deletion_notify.notified() => {}
                _ = tokio::time::sleep(sleep_for) => {}
            }
        }
    }

    pub(crate) fn notify_deletion_coordinator(&self) {
        self.inner.daemon_deletion_notify.notify_one();
    }

    fn next_deletion_coordinator_delay(&self) -> Result<Duration> {
        let Some(next_attempt_at) = self.runtime_db().agent_deletions().earliest_retry_at()? else {
            return Ok(DELETION_SWEEP_INTERVAL);
        };
        Ok((next_attempt_at - Utc::now())
            .to_std()
            .unwrap_or(Duration::ZERO))
    }

    pub(crate) async fn scan_legacy_deletion_residue_batch(
        &self,
    ) -> Result<LegacyDeletionRepairScanOutcome> {
        let batch = self
            .runtime_db()
            .agent_identities()
            .next_legacy_deletion_scan_batch(LEGACY_DELETION_REPAIR_BATCH_LIMIT)?;
        let mut outcome = LegacyDeletionRepairScanOutcome {
            scanned: batch.identities.len(),
            cursor: batch.cursor.clone(),
            ..LegacyDeletionRepairScanOutcome::default()
        };
        for identity in &batch.identities {
            if identity.agent_id == self.config().default_agent_id {
                continue;
            }
            let existing_job = self
                .runtime_db()
                .agent_deletions()
                .latest_for_agent(&identity.agent_id)?;
            if existing_job.as_ref().is_some_and(|job| {
                matches!(
                    job.status,
                    AgentDeletionStatus::Pending
                        | AgentDeletionStatus::Running
                        | AgentDeletionStatus::RetryableFailed
                )
            }) {
                continue;
            }
            match identity.status {
                AgentRegistryStatus::Deleted => {
                    if existing_job.as_ref().is_some_and(|job| {
                        job.status == AgentDeletionStatus::Completed
                            && job.mode == AgentDeletionMode::CleanupRepair
                    }) {
                        continue;
                    }
                    let (updated_identity, _, created) = self
                        .runtime_db()
                        .agent_deletions()
                        .begin(
                            &identity.agent_id,
                            identity.revision,
                            "legacy_residue_scanner",
                            false,
                        )
                        .with_context(|| {
                            format!(
                                "admitting cleanup repair for deleted agent {}",
                                identity.agent_id
                            )
                        })?;
                    self.cache_agent_identity(&updated_identity)?;
                    outcome.created += usize::from(created);
                }
                AgentRegistryStatus::Deleting => {
                    if existing_job.is_none()
                        && self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "deleting_identity_without_job",
                            serde_json::json!({}),
                        )?
                    {
                        outcome.ambiguous += 1;
                    }
                }
                AgentRegistryStatus::Active => {
                    if identity.kind != AgentKind::Child
                        || identity.visibility != AgentVisibility::Private
                        || identity.ownership() != AgentOwnership::ParentSupervised
                    {
                        continue;
                    }
                    let Some(relations) = self
                        .runtime_db()
                        .agent_canonical_relations()
                        .latest(&identity.agent_id)?
                    else {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "canonical_relations_missing",
                            serde_json::json!({
                                "legacy_parent_agent_id": identity.parent_agent_id,
                                "legacy_delegated_from_task_id": identity.delegated_from_task_id,
                            }),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    };
                    if relations.resolution != AgentCanonicalResolution::Resolved {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "canonical_relations_unresolved",
                            serde_json::json!({
                                "resolution": relations.resolution,
                                "issues": relations.issues,
                            }),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    }
                    let Some(lineage) = relations.lineage.as_ref() else {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "canonical_lineage_missing",
                            serde_json::json!({}),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    };
                    let Some(supervision) = relations.supervision.as_ref() else {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "canonical_supervision_missing",
                            serde_json::json!({}),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    };
                    let Some(durability) = relations.durability.as_ref() else {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "canonical_durability_missing",
                            serde_json::json!({}),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    };
                    let Some(attachment) = relations.lifecycle_attachment.as_ref() else {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "canonical_lifecycle_attachment_missing",
                            serde_json::json!({}),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    };
                    if durability.durability != AgentCanonicalDurability::Ephemeral
                        || attachment.attachment != AgentLifecycleAttachment::SupervisionAttached
                    {
                        continue;
                    }
                    if lineage.parent_agent_id != supervision.supervisor_agent_id
                        || supervision.state != AgentSupervisionState::Active
                    {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "canonical_parent_supervision_conflict",
                            serde_json::json!({
                                "lineage_parent_agent_id": lineage.parent_agent_id,
                                "supervisor_agent_id": supervision.supervisor_agent_id,
                                "supervision_state": supervision.state,
                            }),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    }
                    let Some(task_id) = supervision.delegated_from_task_id.as_deref() else {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "delegated_task_missing",
                            serde_json::json!({
                                "supervisor_agent_id": supervision.supervisor_agent_id,
                            }),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    };
                    let Some(task) = self.runtime_db().tasks().latest(task_id)? else {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "delegated_task_record_missing",
                            serde_json::json!({
                                "task_id": task_id,
                                "supervisor_agent_id": supervision.supervisor_agent_id,
                            }),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    };
                    if task.agent_id != supervision.supervisor_agent_id {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "delegated_task_owner_conflict",
                            serde_json::json!({
                                "task_id": task.id,
                                "task_owner_agent_id": task.agent_id,
                                "supervisor_agent_id": supervision.supervisor_agent_id,
                            }),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    }
                    if task.kind == TaskKind::ActorInvocation {
                        continue;
                    }
                    if !task.kind.is_child_agent() {
                        if self.emit_legacy_deletion_ambiguity(
                            &identity,
                            "delegated_task_kind_conflict",
                            serde_json::json!({
                                "task_id": task.id,
                                "task_kind": task.kind,
                            }),
                        )? {
                            outcome.ambiguous += 1;
                        }
                        continue;
                    }
                    if !is_legacy_repair_terminal_task(&task) {
                        continue;
                    }
                    if !legacy_task_deletes_child_on_terminal(&task) {
                        continue;
                    }
                    match self
                        .runtime_db()
                        .agent_deletions()
                        .begin_terminal_ephemeral_child(
                            &identity.agent_id,
                            &supervision.supervisor_agent_id,
                            task_id,
                            "legacy_residue_scanner",
                        )
                    {
                        Ok((updated_identity, _, created)) => {
                            self.cache_agent_identity(&updated_identity)?;
                            if created {
                                self.unload_runtime(&identity.agent_id).await;
                                outcome.created += 1;
                            }
                        }
                        Err(error)
                            if crate::runtime_db::repositories::
                                is_terminal_child_deletion_admission_rejected(&error) =>
                        {
                            if self.emit_legacy_deletion_ambiguity(
                                &identity,
                                "terminal_child_admission_rejected",
                                serde_json::json!({
                                    "task_id": task_id,
                                    "error": error.to_string(),
                                }),
                            )? {
                                outcome.ambiguous += 1;
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        self.runtime_db()
            .agent_identities()
            .commit_legacy_deletion_scan_batch(&batch)?;
        Ok(outcome)
    }

    fn emit_legacy_deletion_ambiguity(
        &self,
        identity: &AgentIdentityRecord,
        reason_code: &str,
        details: serde_json::Value,
    ) -> Result<bool> {
        let data = serde_json::json!({
            "agent_id": identity.agent_id,
            "identity_status": identity.status,
            "identity_revision": identity.revision,
            "incarnation": identity.incarnation,
            "reason_code": reason_code,
            "details": details,
        });
        let mut hasher = Sha256::new();
        hasher.update(b"legacy_deletion_repair_ambiguity");
        hasher.update([0]);
        hasher.update(identity.agent_id.as_bytes());
        hasher.update([0]);
        hasher.update(identity.incarnation.to_le_bytes());
        hasher.update(identity.revision.to_le_bytes());
        hasher.update(reason_code.as_bytes());
        hasher.update(serde_json::to_vec(&data)?);
        let digest = format!("{:x}", hasher.finalize());
        let mut event = AuditEvent::legacy("legacy_deletion_repair_ambiguous", data);
        event.id = format!("event_{}", &digest[..15]);
        event.created_at = identity.updated_at;
        if self
            .runtime_db()
            .audit_events()
            .has_event_by_id(&event.id)?
        {
            return Ok(false);
        }
        self.runtime_db()
            .audit_events()
            .append(Some(&identity.agent_id), &event)?;
        Ok(true)
    }

    async fn drain_due_deletions(&self) -> Result<()> {
        loop {
            if self.inner.daemon_deletion_token.is_cancelled() {
                return Ok(());
            }
            let jobs = self
                .runtime_db()
                .agent_deletions()
                .due_jobs(Utc::now(), DELETION_BATCH_LIMIT)?;
            let batch_len = jobs.len();
            if batch_len == 0 {
                return Ok(());
            }
            let mut failed = 0;
            for job in jobs {
                if self.inner.daemon_deletion_token.is_cancelled() {
                    return Ok(());
                }
                if self.execute_deletion_job(job).await.is_err() {
                    failed += 1;
                }
            }
            if failed > 0 {
                warn!(
                    processed = batch_len,
                    failed, "deletion coordinator batch completed with failures"
                );
            }
            if batch_len < DELETION_BATCH_LIMIT {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
    }

    /// Drive a single deletion job through its remaining phases.
    pub(crate) async fn execute_deletion_job(&self, mut job: AgentDeletionJob) -> Result<()> {
        if let Some(fresh) = self
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&job.agent_id)?
        {
            let now = Utc::now();
            let actionable = fresh.status == AgentDeletionStatus::Pending
                || (fresh.status == AgentDeletionStatus::RetryableFailed
                    && fresh.next_attempt_at.is_none_or(|deadline| deadline <= now));
            if !actionable {
                debug!(
                    agent_id = %job.agent_id,
                    status = ?fresh.status,
                    "deletion job is not due"
                );
                return Ok(());
            }
            job = fresh;
        }
        let agent_id = job.agent_id.clone();
        info!(
            agent_id = %agent_id,
            deletion_id = %job.deletion_id,
            phase = ?job.phase,
            status = ?job.status,
            "executing deletion job"
        );

        job.status = AgentDeletionStatus::Running;
        job.attempts = job.attempts.saturating_add(1);
        job.last_error = None;
        job.next_attempt_at = None;
        let claim_at = Utc::now();
        job.updated_at = claim_at;
        if !self
            .runtime_db()
            .agent_deletions()
            .claim_due(&job, claim_at)?
        {
            debug!(agent_id = %job.agent_id, "deletion job claim lost");
            return Ok(());
        }

        // Execute each phase from the current one onward.
        let phases = AgentDeletionPhase::ALL;
        let start_idx = phases.iter().position(|p| *p == job.phase).unwrap_or(0);

        for phase in &phases[start_idx..] {
            // Skip if we've already advanced past this phase.
            if job.phase > *phase {
                continue;
            }

            let result = if *phase == AgentDeletionPhase::Finalize {
                self.deletion_phase_finalize(&job).await.map(Some)
            } else {
                self.execute_deletion_phase(&agent_id, *phase, &job)
                    .await
                    .map(|_| None)
            };
            match result {
                Ok(finalized_job) => {
                    if let Some(finalized_job) = finalized_job {
                        job = finalized_job;
                    }
                    // Advance to next phase.
                    if let Some(next) = phase.next() {
                        job.phase = next;
                        job.updated_at = Utc::now();
                        self.runtime_db().agent_deletions().update(&job)?;
                    }
                }
                Err(err) => {
                    let error_msg = format!("{err:#}");
                    warn!(
                        agent_id = %agent_id,
                        phase = ?phase,
                        error = %error_msg,
                        "deletion phase failed"
                    );
                    job.status = AgentDeletionStatus::RetryableFailed;
                    job.last_error = Some(error_msg);
                    let now = Utc::now();
                    let retry_delay = deletion_retry_delay(&job);
                    job.next_attempt_at = Some(now + chrono::Duration::from_std(retry_delay)?);
                    job.updated_at = now;
                    self.runtime_db().agent_deletions().update(&job)?;
                    return Err(anyhow!(
                        "deletion job for {agent_id} failed at phase {phase:?}: {err}"
                    ));
                }
            }
        }

        // All phases completed.
        if job.status != AgentDeletionStatus::Completed {
            job.status = AgentDeletionStatus::Completed;
            job.next_attempt_at = None;
            job.completed_at = Some(Utc::now());
            job.updated_at = Utc::now();
            self.runtime_db().agent_deletions().update(&job)?;
        }

        info!(
            agent_id = %agent_id,
            deletion_id = %job.deletion_id,
            "deletion job completed"
        );
        Ok(())
    }

    async fn execute_deletion_phase(
        &self,
        agent_id: &str,
        phase: AgentDeletionPhase,
        job: &AgentDeletionJob,
    ) -> Result<()> {
        match phase {
            AgentDeletionPhase::Fence => self.deletion_phase_fence(agent_id).await,
            AgentDeletionPhase::Quiesce => self.deletion_phase_quiesce(agent_id, job).await,
            AgentDeletionPhase::Ingress => self.deletion_phase_ingress(agent_id).await,
            AgentDeletionPhase::Scheduler => self.deletion_phase_scheduler(agent_id).await,
            AgentDeletionPhase::Workspace => self.deletion_phase_workspace(agent_id).await,
            AgentDeletionPhase::Index => self.deletion_phase_index(agent_id).await,
            AgentDeletionPhase::Home => self.deletion_phase_home(agent_id).await,
            AgentDeletionPhase::Finalize => {
                unreachable!("finalize uses the atomic repository path")
            }
        }
    }

    /// Fence: verify the identity is in Deleting state and the runtime is
    /// unloaded. Phase 0-1 already sets the identity and unloads; this is a
    /// safety check for crash recovery.
    async fn deletion_phase_fence(&self, agent_id: &str) -> Result<()> {
        let identity = self
            .agent_identity_record(agent_id)?
            .ok_or_else(|| anyhow!("agent {agent_id} identity not found"))?;
        if identity.status != AgentRegistryStatus::Deleting {
            return Err(anyhow!(
                "agent {agent_id} identity is {:?}, expected Deleting",
                identity.status
            ));
        }
        // Ensure runtime is unloaded.
        self.unload_runtime(agent_id).await;
        Ok(())
    }

    /// Quiesce: terminalize active tasks, cancel wait conditions and timers.
    /// If cascade_private_children is set, drive private children through
    /// deletion first.
    async fn deletion_phase_quiesce(&self, agent_id: &str, job: &AgentDeletionJob) -> Result<()> {
        // Cascade private children first.
        if job.cascade_private_children {
            self.cascade_private_children_deletion(agent_id, job)
                .await?;
        }

        let storage = if self.agent_data_dir(agent_id).exists() {
            self.agent_storage(agent_id).ok()
        } else {
            None
        };

        let now = Utc::now();

        // Terminalize active tasks.
        let active_tasks = self
            .runtime_db()
            .tasks()
            .active_for_agent(agent_id, usize::MAX)?;
        let tasks_count = active_tasks.len();
        for mut task in active_tasks {
            task.status = TaskStatus::Cancelled;
            task.updated_at = now;
            self.runtime_db().tasks().upsert(&task)?;
        }
        if tasks_count > 0 {
            debug!(agent_id, count = tasks_count, "terminalized active tasks");
        }

        // Cancel active wait conditions.
        let active_waits = self
            .runtime_db()
            .wait_conditions()
            .active_for_agent(agent_id)?;
        let waits_count = active_waits.len();
        for mut wait in active_waits {
            wait.status = WaitConditionStatus::Cancelled;
            wait.updated_at = now;
            wait.cancelled_at = Some(now);
            self.runtime_db().wait_conditions().upsert(&wait)?;
        }
        if waits_count > 0 {
            debug!(
                agent_id,
                count = waits_count,
                "cancelled active wait conditions"
            );
        }

        // Cancel active timers.
        let timers = self
            .runtime_db()
            .timers()
            .recent_for_agent(agent_id, usize::MAX)?;
        for mut timer in timers {
            if timer.status == TimerStatus::Active {
                timer.status = TimerStatus::Cancelled;

                self.runtime_db().timers().upsert(&timer)?;
            }
        }

        // Abort pending queue entries to prevent orphaned queued messages.
        let queue_aborted = self
            .runtime_db()
            .queue_entries()
            .abort_pending_for_agent(agent_id)
            .unwrap_or(0);
        if queue_aborted > 0 {
            debug!(
                agent_id,
                count = queue_aborted,
                "aborted pending queue entries"
            );
        }

        // Emit audit event via storage if available.
        if let Some(storage) = storage {
            let _ = storage.append_event(&AuditEvent::legacy(
                "deletion_quiesce",
                serde_json::json!({
                    "agent_id": agent_id,
                    "tasks_cancelled": tasks_count,
                    "waits_cancelled": waits_count,
                    "queue_entries_aborted": queue_aborted,
                }),
            ));
        }

        Ok(())
    }

    /// Ingress: revoke all active external triggers for the agent.
    async fn deletion_phase_ingress(&self, agent_id: &str) -> Result<()> {
        let triggers = self
            .runtime_db()
            .external_triggers()
            .latest_for_agent(agent_id)?;
        let now = Utc::now();
        let mut revoked_count = 0;
        for mut trigger in triggers {
            if trigger.status == ExternalTriggerStatus::Active {
                trigger.status = ExternalTriggerStatus::Revoked;
                trigger.revoked_at = Some(now);
                self.runtime_db().external_triggers().upsert(&trigger)?;
                revoked_count += 1;
            }
        }
        if revoked_count > 0 {
            debug!(agent_id, count = revoked_count, "revoked external triggers");
        }
        Ok(())
    }

    /// Scheduler: terminalize durable work that could participate in future
    /// scheduler promotion or dispatch.
    async fn deletion_phase_scheduler(&self, agent_id: &str) -> Result<()> {
        let storage = self.agent_storage(&self.config().default_agent_id)?;
        let terminalizing_work_items = self
            .runtime_db()
            .work_items()
            .latest_for_agent(agent_id, usize::MAX)?
            .into_iter()
            .filter(|work_item| work_item.state != WorkItemState::Completed)
            .collect::<Vec<_>>();
        let now = Utc::now();
        for existing in &terminalizing_work_items {
            let completing = if existing.state == WorkItemState::Open {
                let mut completing = existing.clone();
                completing.revision = existing.revision.saturating_add(1);
                completing.state = WorkItemState::Completing;
                completing.blocked_by = None;
                completing.recheck_at = None;
                completing.recheck_consumed_at = None;
                completing.completion_intent = Some(WorkItemCompletionIntent {
                    work_item_id: existing.id.clone(),
                    source_activation_id: None,
                    source_message_id: None,
                    source_turn_id: None,
                    expected_work_revision: existing.revision,
                    report_requirement: CompletionReportRequirement::Required,
                    report_state: CompletionReportState::Pending,
                    result_brief_id: None,
                    created_at: now,
                    updated_at: now,
                });
                completing.updated_at = now;
                self.runtime_db().transitions().commit_work_item(
                    &crate::runtime_db::transitions::WorkItemTransitionCommand {
                        agent_id: agent_id.to_string(),
                        mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                            record: completing.clone(),
                            expected_revision: existing.revision,
                        },
                        agent_state: None,
                        brief_evidence: Vec::new(),
                        audit_events: vec![AuditEvent::legacy(
                            "work_item_completion_intent_recorded_for_agent_deletion",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "work_item_id": completing.id,
                                "revision": completing.revision,
                                "reason": "agent_deleted",
                            }),
                        )],
                        index_changes: storage.index_changes_for_work_item(&completing)?,
                        notify_scheduler: true,
                        fault: None,
                    },
                )?;
                completing
            } else {
                existing.clone()
            };
            let mut completion_intent = completing.completion_intent.clone().ok_or_else(|| {
                anyhow!(
                    "completing work item {} is missing completion intent",
                    completing.id
                )
            })?;
            let mut brief = BriefRecord::new(
                agent_id,
                BriefKind::Result,
                "Agent deleted before work completed",
                completion_intent.source_message_id.clone(),
                None,
            );
            brief.work_item_id = Some(completing.id.clone());
            brief.workspace_id = completing.workspace_id.clone();
            brief.turn_id = completion_intent.source_turn_id.clone();
            completion_intent.report_state = CompletionReportState::Bound;
            completion_intent.result_brief_id = Some(brief.id.clone());
            completion_intent.updated_at = now;
            let mut completed = completing.clone();
            completed.revision = completing.revision.saturating_add(1);
            completed.state = WorkItemState::Completed;
            completed.blocked_by = None;
            completed.recheck_at = None;
            completed.recheck_consumed_at = None;
            completed.result_brief_id = Some(brief.id.clone());
            completed.result_summary = Some("Agent deleted before work completed".into());
            completed.completion_intent = Some(completion_intent);
            completed.updated_at = now;
            self.runtime_db().transitions().commit_work_item(
                &crate::runtime_db::transitions::WorkItemTransitionCommand {
                    agent_id: agent_id.to_string(),
                    mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                        record: completed.clone(),
                        expected_revision: completing.revision,
                    },
                    agent_state: None,
                    brief_evidence: vec![brief],
                    audit_events: vec![AuditEvent::legacy(
                        "work_item_completed_for_agent_deletion",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "work_item_id": completed.id,
                            "revision": completed.revision,
                            "reason": "agent_deleted",
                        }),
                    )],
                    index_changes: storage.index_changes_for_work_item(&completed)?,
                    notify_scheduler: true,
                    fault: None,
                },
            )?;
        }
        debug!(
            agent_id,
            work_items_completed = terminalizing_work_items.len(),
            "scheduler phase terminalized incomplete work items"
        );
        Ok(())
    }

    /// Workspace: release all workspace occupancies held by this agent and
    /// remove owned clean managed worktrees.
    async fn deletion_phase_workspace(&self, agent_id: &str) -> Result<()> {
        // Release all active occupancies held by this agent.
        let all_occupancies = self.runtime_db().workspace_occupancies().latest_all()?;
        let now = Utc::now();
        let mut released_count = 0;
        for mut occupancy in all_occupancies {
            if occupancy.holder_agent_id == agent_id && occupancy.released_at.is_none() {
                occupancy.released_at = Some(now);
                self.runtime_db()
                    .workspace_occupancies()
                    .upsert(&occupancy)?;
                released_count += 1;
            }
        }
        if released_count > 0 {
            debug!(
                agent_id,
                count = released_count,
                "released workspace occupancies"
            );
        }

        // Remove owned managed worktrees that are clean.
        let all_roots = self.runtime_db().execution_root_entries().latest_all()?;
        for root in all_roots {
            if root.removed_at.is_some() {
                continue;
            }
            let Some(worktree) = root.worktree.as_ref() else {
                continue;
            };
            // Only remove worktrees registered by this agent.
            let dominated_by_agent = worktree.registered_by_agent_id.as_deref() == Some(agent_id)
                || worktree
                    .authorized_agent_ids
                    .iter()
                    .all(|id| id == agent_id);
            if !dominated_by_agent {
                continue;
            }
            let worktree_path = &root.filesystem_path;
            if !worktree_path.exists() {
                // Already gone; just mark removed.
                self.runtime_db()
                    .execution_root_entries()
                    .mark_removed(&root.execution_root_id)?;
                continue;
            }
            // Check if the worktree is clean (no uncommitted changes).
            if self.worktree_is_dirty(worktree_path) {
                return Err(anyhow!(
                    "worktree {} at {} has uncommitted changes; resolve before deletion can proceed",
                    root.execution_root_id,
                    worktree_path.display()
                ));
            }
            // Safe to remove.
            self.remove_worktree_directory(worktree_path)?;
            self.runtime_db()
                .execution_root_entries()
                .mark_removed(&root.execution_root_id)?;
            debug!(
                agent_id,
                execution_root_id = %root.execution_root_id,
                "removed managed worktree"
            );
        }

        Ok(())
    }

    /// Index: remove the agent's documents from the shared memory index.
    async fn deletion_phase_index(&self, agent_id: &str) -> Result<()> {
        self.runtime_db()
            .runtime_index_outbox()
            .delete_all_for_agent(agent_id)?;
        // The memory index is a shared SQLite database. Remove all rows
        // belonging to this agent.
        let storage = match self.agent_storage(&self.config().default_agent_id) {
            Ok(s) => s,
            Err(_) => {
                debug!(
                    agent_id,
                    "shared index storage unavailable during index cleanup; skipping"
                );
                return Ok(());
            }
        };
        crate::memory::index::delete_agent_memory_index_projection(
            &storage.shared_indexes_dir(),
            agent_id,
        )?;
        debug!(agent_id, "removed agent from memory index");
        Ok(())
    }

    /// Home: rename agent home to trash then delete.
    async fn deletion_phase_home(&self, agent_id: &str) -> Result<()> {
        let data_dir = self.agent_data_dir(agent_id);
        if !data_dir.exists() {
            return Ok(());
        }
        // Refuse to delete anything that is not a runtime-managed directory:
        // symlinked homes or paths resolving outside the agents root fail the
        // job instead of removing unintended files.
        let agents_root = self.config().data_dir.join("agents");
        ensure_deletable_agent_home(&data_dir, &agents_root)?;
        // Rename to a trash name first to avoid partial-state visibility.
        let trash_dir = data_dir.with_extension("deleting_trash");
        if trash_dir.exists() {
            // Previous attempt left trash; remove it.
            std::fs::remove_dir_all(&trash_dir).with_context(|| {
                format!("removing leftover trash directory {}", trash_dir.display())
            })?;
        }
        std::fs::rename(&data_dir, &trash_dir).with_context(|| {
            format!(
                "renaming agent home {} to trash {}",
                data_dir.display(),
                trash_dir.display()
            )
        })?;
        std::fs::remove_dir_all(&trash_dir)
            .with_context(|| format!("removing agent home trash {}", trash_dir.display()))?;
        info!(agent_id, "removed agent home directory");
        Ok(())
    }

    /// Finalize: set identity to Deleted and emit audit event.
    async fn deletion_phase_finalize(&self, job: &AgentDeletionJob) -> Result<AgentDeletionJob> {
        let (identity, completed_job) = self.runtime_db().agent_deletions().finalize(job)?;
        self.cache_agent_identity(&identity)?;
        info!(agent_id = %job.agent_id, "agent identity finalized as Deleted");
        Ok(completed_job)
    }

    /// Cascade deletion to private children of the given agent.
    async fn cascade_private_children_deletion(
        &self,
        parent_agent_id: &str,
        parent_job: &AgentDeletionJob,
    ) -> Result<()> {
        let identities = self.agent_identity_records()?;
        let children: Vec<_> = identities
            .into_iter()
            .filter(|id| {
                id.visibility == AgentVisibility::Private
                    && id.ownership() == AgentOwnership::ParentSupervised
                    && id.parent_agent_id.as_deref() == Some(parent_agent_id)
            })
            .collect();

        for child in children {
            let child_id = &child.agent_id;
            let bootstrap_lock = self.agent_bootstrap_lock(child_id);
            let _bootstrap_guard = bootstrap_lock.lock().await;
            let (updated_identity, child_job, _) = self.runtime_db().agent_deletions().begin(
                child_id,
                child.revision,
                &parent_job.requested_by,
                false, // Don't recurse further
            )?;
            self.cache_agent_identity(&updated_identity)?;
            Box::pin(self.execute_deletion_job(child_job))
                .await
                .with_context(|| format!("cascading deletion to private child {child_id}"))?;
        }
        Ok(())
    }

    /// Check if a git worktree has uncommitted changes.
    fn worktree_is_dirty(&self, path: &Path) -> bool {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(path)
            .output();
        match output {
            Ok(output) => !output.stdout.is_empty(),
            Err(_) => true, // If we can't check, treat as dirty for safety.
        }
    }

    /// Remove a worktree directory (git worktree remove or fallback to rm).
    fn remove_worktree_directory(&self, path: &Path) -> Result<()> {
        // Try git worktree remove first for clean git state.
        let output = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(path)
            .output();
        match output {
            Ok(output) if output.status.success() => return Ok(()),
            _ => {}
        }
        // Fallback: remove directory directly.
        std::fs::remove_dir_all(path)
            .with_context(|| format!("removing worktree directory {}", path.display()))
    }
}

fn is_legacy_repair_terminal_task(task: &TaskRecord) -> bool {
    matches!(
        task.status,
        TaskStatus::Completed
            | TaskStatus::Failed
            | TaskStatus::Cancelled
            | TaskStatus::Interrupted
    )
}

fn legacy_task_deletes_child_on_terminal(task: &TaskRecord) -> bool {
    match task.recovery.as_ref() {
        Some(TaskRecoverySpec::ChildAgentTask {
            lifecycle_disposition,
            ..
        }) => *lifecycle_disposition == AgentLifecycleDisposition::DeleteOnTerminal,
        Some(TaskRecoverySpec::SubagentTask { .. })
        | Some(TaskRecoverySpec::WorktreeSubagentTask { .. })
        | None
            if task.kind.is_child_agent() =>
        {
            true
        }
        _ => false,
    }
}

fn deletion_retry_delay(job: &AgentDeletionJob) -> Duration {
    let exponent = job.attempts.saturating_sub(1).min(16);
    let base_millis = DELETION_RETRY_BASE
        .as_millis()
        .saturating_mul(1_u128 << exponent)
        .min(DELETION_RETRY_CAP.as_millis());
    let jitter_headroom = DELETION_RETRY_CAP.as_millis().saturating_sub(base_millis);
    let jitter_bound = (base_millis / 4).min(jitter_headroom);
    let stable_hash = job
        .deletion_id
        .bytes()
        .chain(format!("{:?}", job.phase).bytes())
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    let jitter = if jitter_bound == 0 {
        0
    } else {
        u128::from(stable_hash) % (jitter_bound + 1)
    };
    Duration::from_millis(u64::try_from(base_millis + jitter).unwrap_or(u64::MAX))
}

/// Validate that an agent home path is safe to delete: it must be a real
/// directory (not a symlink) that resolves inside the runtime agents root.
/// Deletion fails closed so a tampered home cannot remove files outside the
/// runtime data directory.
fn ensure_deletable_agent_home(data_dir: &Path, agents_root: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(data_dir)
        .with_context(|| format!("inspecting agent home {}", data_dir.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "agent home {} is a symlink; refusing deletion",
            data_dir.display()
        );
    }
    if !metadata.is_dir() {
        anyhow::bail!(
            "agent home {} is not a directory; refusing deletion",
            data_dir.display()
        );
    }
    let canonical_home = std::fs::canonicalize(data_dir)
        .with_context(|| format!("canonicalizing agent home {}", data_dir.display()))?;
    let canonical_root = std::fs::canonicalize(agents_root)
        .with_context(|| format!("canonicalizing agents root {}", agents_root.display()))?;
    if !canonical_home.starts_with(&canonical_root) {
        anyhow::bail!(
            "agent home {} resolves outside agents root {}; refusing deletion",
            canonical_home.display(),
            canonical_root.display()
        );
    }
    Ok(())
}

#[cfg(test)]
fn delete_agent_memory_index_projection(
    tx: &rusqlite::Transaction<'_>,
    agent_id: &str,
) -> Result<()> {
    let table_exists = |name: &str| -> Result<bool> {
        tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
             )",
            [name],
            |row| row.get::<_, bool>(0),
        )
        .map_err(Into::into)
    };
    if table_exists("memory_documents_fts")? {
        tx.execute(
            "DELETE FROM memory_documents_fts
             WHERE document_key IN (
                SELECT document_key FROM memory_documents WHERE agent_id = ?1
             )",
            [agent_id],
        )?;
    }
    if table_exists("memory_documents_fts_rows")? {
        tx.execute(
            "DELETE FROM memory_documents_fts_rows
             WHERE document_key IN (
                SELECT document_key FROM memory_documents WHERE agent_id = ?1
             )",
            [agent_id],
        )?;
    }
    tx.execute(
        "DELETE FROM memory_documents WHERE agent_id = ?1",
        [agent_id],
    )?;
    tx.execute(
        "DELETE FROM memory_index_source_state WHERE agent_id = ?1",
        [agent_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retry_job(attempts: u32) -> AgentDeletionJob {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-19T07:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        AgentDeletionJob {
            deletion_id: "delete_retry_test".into(),
            agent_id: "retry-agent".into(),
            mode: AgentDeletionMode::Delete,
            status: AgentDeletionStatus::RetryableFailed,
            phase: AgentDeletionPhase::Index,
            requested_by: "test".into(),
            expected_identity_revision: 1,
            cascade_private_children: false,
            attempts,
            last_error: Some("database is locked".into()),
            next_attempt_at: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
        }
    }

    #[test]
    fn deletion_retry_delay_is_deterministic_exponential_and_capped() {
        let first = deletion_retry_delay(&retry_job(1));
        assert!(first >= DELETION_RETRY_BASE);
        assert!(first <= Duration::from_millis(312));
        assert_eq!(first, deletion_retry_delay(&retry_job(1)));

        let second = deletion_retry_delay(&retry_job(2));
        assert!(second >= Duration::from_millis(500));
        assert!(second <= Duration::from_millis(625));
        assert!(second > first);

        assert_eq!(deletion_retry_delay(&retry_job(32)), DELETION_RETRY_CAP);
    }

    #[test]
    fn index_cleanup_removes_row_map_before_fts_rowid_reuse() -> Result<()> {
        let connection = rusqlite::Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE memory_documents (
                 document_key TEXT PRIMARY KEY,
                 agent_id TEXT NOT NULL
             );
             CREATE VIRTUAL TABLE memory_documents_fts USING fts5(
                 document_key UNINDEXED,
                 body
             );
             CREATE TABLE memory_documents_fts_rows (
                 document_key TEXT PRIMARY KEY,
                 fts_rowid INTEGER NOT NULL UNIQUE
             );
             CREATE TABLE memory_index_source_state (
                 document_key TEXT PRIMARY KEY,
                 agent_id TEXT NOT NULL
             );",
        )?;
        let deleted_key = "deleted-agent:message:old";
        connection.execute(
            "INSERT INTO memory_documents (document_key, agent_id) VALUES (?1, 'deleted-agent')",
            [deleted_key],
        )?;
        connection.execute(
            "INSERT INTO memory_documents_fts (document_key, body) VALUES (?1, 'old')",
            [deleted_key],
        )?;
        let deleted_rowid = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO memory_documents_fts_rows (document_key, fts_rowid) VALUES (?1, ?2)",
            rusqlite::params![deleted_key, deleted_rowid],
        )?;
        connection.execute(
            "INSERT INTO memory_index_source_state (document_key, agent_id)
             VALUES (?1, 'deleted-agent')",
            [deleted_key],
        )?;

        let tx = connection.unchecked_transaction()?;
        delete_agent_memory_index_projection(&tx, "deleted-agent")?;
        tx.commit()?;

        let replacement_key = "replacement-agent:message:new";
        connection.execute(
            "INSERT INTO memory_documents_fts (rowid, document_key, body)
             VALUES (?1, ?2, 'new')",
            rusqlite::params![deleted_rowid, replacement_key],
        )?;
        connection.execute(
            "INSERT INTO memory_documents_fts_rows (document_key, fts_rowid) VALUES (?1, ?2)",
            rusqlite::params![replacement_key, deleted_rowid],
        )?;

        let mapped_key: String = connection.query_row(
            "SELECT document_key FROM memory_documents_fts_rows WHERE fts_rowid = ?1",
            [deleted_rowid],
            |row| row.get(0),
        )?;
        assert_eq!(mapped_key, replacement_key);
        Ok(())
    }

    #[test]
    fn deletable_home_accepts_plain_directory_inside_root() {
        let root = tempfile::tempdir().unwrap();
        let agents = root.path().join("agents");
        std::fs::create_dir_all(agents.join("alpha")).unwrap();
        ensure_deletable_agent_home(&agents.join("alpha"), &agents).unwrap();
    }

    #[test]
    fn deletable_home_rejects_missing_directory() {
        let root = tempfile::tempdir().unwrap();
        let agents = root.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let err = ensure_deletable_agent_home(&agents.join("missing"), &agents).unwrap_err();
        assert!(err.to_string().contains("inspecting agent home"));
    }

    #[test]
    fn deletable_home_rejects_plain_file() {
        let root = tempfile::tempdir().unwrap();
        let agents = root.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("alpha"), "not a dir").unwrap();
        let err = ensure_deletable_agent_home(&agents.join("alpha"), &agents).unwrap_err();
        assert!(err.to_string().contains("is not a directory"));
    }

    #[cfg(unix)]
    #[test]
    fn deletable_home_rejects_symlink_pointing_inside_root() {
        let root = tempfile::tempdir().unwrap();
        let agents = root.path().join("agents");
        let real = root.path().join("real-alpha");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&agents).unwrap();
        std::os::unix::fs::symlink(&real, agents.join("alpha")).unwrap();
        let err = ensure_deletable_agent_home(&agents.join("alpha"), &agents).unwrap_err();
        assert!(err.to_string().contains("is a symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn deletable_home_rejects_symlink_pointing_outside_root() {
        let root = tempfile::tempdir().unwrap();
        let agents = root.path().join("agents");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&agents).unwrap();
        std::os::unix::fs::symlink(&outside, agents.join("alpha")).unwrap();
        let err = ensure_deletable_agent_home(&agents.join("alpha"), &agents).unwrap_err();
        assert!(err.to_string().contains("is a symlink"));
    }
}
