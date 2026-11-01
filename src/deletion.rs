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
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::host::RuntimeHost;
use crate::types::*;

#[path = "deletion_archive.rs"]
mod archive;

mod scheduling_maintenance;
mod task_maintenance;
/// Authorization selects a policy, independent of the request's tool/CLI surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentDeletionPolicy {
    PreserveProtectedResources,
    DeleteAgentData,
}

impl From<AgentDeletionMode> for AgentDeletionPolicy {
    fn from(mode: AgentDeletionMode) -> Self {
        match mode {
            AgentDeletionMode::ParentCleanup => Self::PreserveProtectedResources,
            AgentDeletionMode::Delete | AgentDeletionMode::CleanupRepair => Self::DeleteAgentData,
        }
    }
}

/// Interval between periodic deletion coordinator safety/observation sweeps.
/// Deletion admission wakes the coordinator directly and retries use persisted
/// deadlines, so this cadence only bounds low-priority scans.
const DELETION_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
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

// A metadata ACK can outlive the phase caller. Do not overwrite its durable
// job checkpoint while publication or the transaction outcome is unresolved.
#[derive(Debug)]
struct DeletionMetadataPending;

impl std::fmt::Display for DeletionMetadataPending {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("deletion metadata requires reconciliation")
    }
}

impl std::error::Error for DeletionMetadataPending {}

impl RuntimeHost {
    async fn commit_deletion_identity_metadata<T: Send + 'static>(
        &self,
        agent_id: String,
        expected_incarnation: Option<u64>,
        scopes: Vec<String>,
        persist: impl FnOnce(&RuntimeHost) -> Result<(T, AgentIdentityRecord)> + Send + 'static,
    ) -> Result<T> {
        self.commit_deletion_identity_metadata_checked(
            agent_id,
            expected_incarnation,
            scopes,
            |_| Ok(()),
            persist,
        )
        .await
    }

    async fn commit_deletion_identity_metadata_checked<T: Send + 'static>(
        &self,
        agent_id: String,
        expected_incarnation: Option<u64>,
        mut scopes: Vec<String>,
        preflight: impl FnOnce(&[crate::runtime::mutation_owner::MutationReservation]) -> Result<()>
            + Send
            + 'static,
        persist: impl FnOnce(&RuntimeHost) -> Result<(T, AgentIdentityRecord)> + Send + 'static,
    ) -> Result<T> {
        scopes.push(agent_id.clone());
        scopes.sort();
        scopes.dedup();
        let host = self.clone();
        self.run_owned_external_mutation(async move {
            let bootstrap_lock = host.agent_bootstrap_lock(&agent_id);
            let _bootstrap = bootstrap_lock.lock().await;
            let mut reservations =
                host.reserve_agent_mutations(&scopes)
                    .await
                    .map_err(|error| {
                        if error
                            .downcast_ref::<crate::runtime::mutation_owner::MutationOwnerError>()
                            .is_some_and(|error| {
                                matches!(error,
                        crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_)
                        | crate::runtime::mutation_owner::MutationOwnerError::Replaced)
                            })
                            || crate::runtime_db::persistence_certainty(&error)
                                == Some(crate::runtime_db::PersistenceCertainty::Indeterminate)
                        {
                            error.context(DeletionMetadataPending)
                        } else {
                            error
                        }
                    })?;
            let _home_guard = host.inner.agent_home_cleanup_lock.lock().await;
            let mut facts = host.external_identity_facts(&reservations)?;
            let target = reservations
                .iter()
                .position(|reservation| reservation.agent_id() == agent_id)
                .context("deletion target owner is missing")?;
            if let Some(expected_incarnation) = expected_incarnation {
                anyhow::ensure!(
                    facts[target]
                        .identity
                        .as_ref()
                        .is_some_and(|identity| { identity.incarnation == expected_incarnation }),
                    "deletion target identity incarnation changed"
                );
            }
            preflight(&reservations)?;
            let mut committed_value = None;
            let result = host
                .commit_external_metadata(&mut reservations, crate::ids::audit_event_id(), || {
                    let (value, identity) = persist(&host)?;
                    facts[target].identity = Some(Arc::new(identity));
                    committed_value = Some(value);
                    Ok(((), facts))
                })
                .await;
            match result {
                Ok(()) => Ok(committed_value.expect("metadata ACK must retain its result")),
                Err(error) if committed_value.is_some() => {
                    // Install the exact ACK in this scope; never repeat the phase SQL.
                    if host
                        .reconcile_external_mutations(&mut reservations)
                        .await
                        .is_ok()
                    {
                        Ok(committed_value.expect("known metadata ACK must retain its result"))
                    } else {
                        Err(error.context(DeletionMetadataPending))
                    }
                }
                Err(error)
                    if crate::runtime_db::persistence_certainty(&error)
                        != Some(
                            crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted,
                        ) =>
                {
                    Err(error.context(DeletionMetadataPending))
                }
                Err(error) => Err(error),
            }
        })
        .await
    }

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
        let mut sweep = tokio::time::interval(DELETION_SWEEP_INTERVAL);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut deletion: Option<tokio::task::JoinHandle<Result<()>>> = None;
        let mut retirement: Option<tokio::task::JoinHandle<Result<()>>> = None;
        let mut idle_cursor = None;
        let mut cleanup_cursor = None;
        let mut reminder_cursor = None;
        let mut reminders: Option<tokio::task::JoinHandle<Result<Option<String>>>> = None;
        let mut trash_sweep: Option<tokio::task::JoinHandle<()>> = None;
        loop {
            let retry_delay = self
                .next_deletion_coordinator_delay()
                .unwrap_or(DELETION_COORDINATOR_ERROR_DELAY)
                .max(Duration::from_millis(1));
            tokio::select! {
                _ = self.inner.daemon_deletion_token.cancelled() => break,
                _ = sweep.tick() => {
                    if !startup_recovery_complete {
                        match self.runtime_db().agent_deletions().recover_running_jobs(Utc::now()) {
                            Ok(recovered) => { startup_recovery_complete = true; info!(recovered, "deletion startup recovery complete"); }
                            Err(error) => { warn!(%error, "deletion startup recovery failed"); sweep.reset_after(DELETION_COORDINATOR_ERROR_DELAY); continue; }
                        }
                    }
                    match self.scan_legacy_deletion_residue_batch().await {
                        Ok(outcome) => {
                            if outcome.created > 0 || outcome.ambiguous > 0 { info!(scanned = outcome.scanned, created = outcome.created, ambiguous = outcome.ambiguous, "legacy deletion scan completed"); }
                            if outcome.cursor.is_some() { sweep.reset_after(Duration::from_millis(1)); }
                        },
                        Err(error) => warn!(%error, "legacy deletion scan failed"),
                    }
                    if trash_sweep.is_none() {
                        let host = self.clone();
                        trash_sweep = Some(tokio::spawn(async move {
                            // Removals run off the coordinator so a large
                            // orphan tree cannot stall shutdown admission.
                            match host.sweep_orphan_agent_home_trash().await {
                                Ok(removed) => {
                                    if removed > 0 {
                                        info!(removed, "orphan agent home trash sweep complete");
                                    }
                                }
                                Err(error) => warn!(%error, "orphan agent home trash sweep failed"),
                            }
                        }));
                    }
                    let config = self.config();
                    let enabled = config.stored_config.runtime.reclamation.reminders_enabled.unwrap_or(true);
                    let grace = chrono::Duration::from_std(Duration::from_secs(config.stored_config.runtime.reclamation.idle_grace_seconds.unwrap_or(600).max(1))).unwrap_or(chrono::Duration::MAX);
                    match self.runtime_db().scan_subagent_cleanup(cleanup_cursor.as_deref(), DELETION_BATCH_LIMIT, Utc::now(), grace, enabled) {
                        Ok(outcome) => { cleanup_cursor = outcome.cursor; if outcome.changed > 0 { debug!(observed=outcome.observed, changed=outcome.changed, "subagent responsibility observations changed"); } }
                        Err(error) => warn!(%error, "subagent responsibility scan failed"),
                    }
                    if enabled && reminders.is_none() {
                        let host=self.clone(); let cursor=reminder_cursor.take();
                        reminders=Some(tokio::spawn(async move { host.process_subagent_reminder_batch(cursor).await }));
                    }
                    if retirement.is_none() {
                        let grace = Duration::from_secs(config.stored_config.runtime.reclamation.idle_grace_seconds.unwrap_or(600).max(1));
                        match self.scan_idle_runtime_candidates(&mut idle_cursor, DELETION_BATCH_LIMIT, grace, config.stored_config.runtime.reclamation.idle_retirement_enabled.unwrap_or(false)).await {
                            Ok(candidates) => if let Some((agent_id, generation)) = candidates.into_iter().next() {
                                let host = self.clone();
                                retirement = Some(tokio::spawn(async move { host.try_retire_idle_runtime(&agent_id, generation).await }));
                            },
                            Err(error) => warn!(%error, "idle runtime observation failed"),
                        }
                    }
                    if deletion.is_none() { deletion = Some(self.spawn_deletion_batch()); }
                }
                _ = self.inner.daemon_deletion_notify.notified(), if startup_recovery_complete => {
                    if deletion.is_none() { deletion = Some(self.spawn_deletion_batch()); }
                }
                _ = tokio::time::sleep(retry_delay), if startup_recovery_complete && deletion.is_none() => {
                    deletion = Some(self.spawn_deletion_batch());
                }
                result = async { deletion.as_mut().unwrap().await }, if deletion.is_some() => {
                    deletion = None;
                    if !matches!(result, Ok(Ok(()))) { warn!(?result, "deletion batch failed"); }
                    if self.runtime_db().agent_deletions().due_jobs(Utc::now(), 1).is_ok_and(|jobs| !jobs.is_empty()) { deletion = Some(self.spawn_deletion_batch()); }
                }
                result = async { reminders.as_mut().unwrap().await }, if reminders.is_some() => {
                    reminders=None;
                    match result { Ok(Ok(cursor)) => reminder_cursor=cursor, other => warn!(?other, "subagent reminder batch deferred") }
                }
                result = async { retirement.as_mut().unwrap().await }, if retirement.is_some() => {
                    retirement = None;
                    if !matches!(result, Ok(Ok(()))) { debug!(?result, "idle retirement deferred"); }
                }
                _ = async { trash_sweep.as_mut().unwrap().await }, if trash_sweep.is_some() => {
                    trash_sweep = None;
                }
            }
        }
        // Do not detach phase work or drop a closed runtime gate on shutdown.
        if let Some(job) = deletion {
            let _ = job.await;
        }
        if let Some(job) = reminders {
            let _ = job.await;
        }
        if let Some(job) = retirement {
            let _ = job.await;
        }
        if let Some(job) = trash_sweep {
            let _ = job.await;
        }
    }

    async fn process_subagent_reminder_batch(
        &self,
        cursor: Option<String>,
    ) -> Result<Option<String>> {
        if !self
            .config()
            .stored_config
            .runtime
            .reclamation
            .reminders_enabled
            .unwrap_or(true)
        {
            return Ok(cursor);
        }
        let operator = self.config().default_agent_id.clone();
        if let Some(brief) = self
            .runtime_db()
            .orphan_cleanup_notice(&operator, DELETION_BATCH_LIMIT)?
        {
            self.persist_cold_brief_evidence(brief.clone()).await?;
            self.runtime_db().acknowledge_subagent_reminder(&brief.id)?;
        }
        let messages = self
            .runtime_db()
            .pending_subagent_reminders(cursor.as_deref(), DELETION_BATCH_LIMIT)?;
        let next = if messages.len() == DELETION_BATCH_LIMIT {
            messages.last().map(|m| m.agent_id.clone())
        } else {
            None
        };
        for message in messages {
            if self.inner.daemon_deletion_token.is_cancelled()
                || !self
                    .config()
                    .stored_config
                    .runtime
                    .reclamation
                    .reminders_enabled
                    .unwrap_or(true)
            {
                break;
            }
            if !self.runtime_db().subagent_reminder_is_current(&message)? {
                continue;
            }
            // Reuse the durable ID when activation/enqueue crosses idle retirement.
            for _ in 0..3 {
                let runtime = self.get_or_create_agent(&message.agent_id).await?;
                match runtime.enqueue(message.clone()).await {
                    Ok(_) => {
                        self.runtime_db()
                            .acknowledge_subagent_reminder(&message.id)?;
                        break;
                    }
                    Err(error)
                        if crate::runtime_error::describe_runtime_error(&error).code
                            == "runtime_instance_retiring" =>
                    {
                        continue
                    }
                    Err(error) => {
                        debug!(parent=%message.agent_id,%error,"subagent reminder deferred");
                        break;
                    }
                }
            }
        }
        Ok(next)
    }

    fn spawn_deletion_batch(&self) -> tokio::task::JoinHandle<Result<()>> {
        let host = self.clone();
        let executor = tokio::runtime::Handle::current();
        // Existing phases include synchronous filesystem/SQLite operations.
        // Keep their owned worker from stalling scans and shutdown admission.
        tokio::task::spawn_blocking(move || executor.block_on(host.process_due_deletion_batch()))
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
                    }) && self
                        .runtime_db()
                        .with_read(|connection| {
                            let tx = connection.transaction()?;
                            crate::runtime_db::agent_deletion_cleanup::assert_private_history_cleared_tx(
                                &tx,
                                &identity.agent_id,
                            )
                        })
                        .is_ok()
                    {
                        continue;
                    }
                    let target = identity.clone();
                    let (_, _, created) = self
                        .commit_deletion_identity_metadata(
                            target.agent_id.clone(),
                            Some(target.incarnation),
                            Vec::new(),
                            move |host| {
                                let result = host.runtime_db().agent_deletions().begin(
                                    &target.agent_id,
                                    target.revision,
                                    "legacy_residue_scanner",
                                    false,
                                )?;
                                Ok((result.clone(), result.0))
                            },
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "admitting cleanup repair for deleted agent {}",
                                identity.agent_id
                            )
                        })?;
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
                    let target = identity.clone();
                    let parent_agent_id = supervision.supervisor_agent_id.clone();
                    let command_task_id = task_id.to_string();
                    match self.commit_deletion_identity_metadata(
                        target.agent_id.clone(),
                        Some(target.incarnation),
                        vec![parent_agent_id.clone()],
                        move |host| {
                            let result = host.runtime_db().agent_deletions()
                                .begin_terminal_ephemeral_child(
                                    &target.agent_id,
                                    &parent_agent_id,
                                    &command_task_id,
                                    "legacy_residue_scanner",
                                )?;
                            Ok((result.clone(), result.0))
                        },
                    ).await {
                        Ok((_, _, created)) => {
                            if created {
                                self.drain_retired_runtime(&identity.agent_id, identity.incarnation).await?;
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

    /// Remove orphaned `*.deleting_trash` directories under the agents root.
    ///
    /// A trash directory is created only by [`RuntimeHost::deletion_phase_home`]
    /// renaming an agent home right before removal. When trash removal fails
    /// and the deletion job finalizes anyway (older builds), or an unexpected
    /// crash leaves trash behind, no database record points at the leftover
    /// directory anymore, so this filesystem-level sweep is the only recovery
    /// path. Names are matched by suffix only: agent ids may contain `.`, so a
    /// trash name cannot be reversed into a reliable agent id. A candidate
    /// whose exact name belongs to a registered non-deleted identity is a
    /// live home whose legacy id collides with the reserved suffix, not
    /// trash, and is skipped.
    pub(crate) async fn sweep_orphan_agent_home_trash(&self) -> Result<usize> {
        let agents_root = self.config().data_dir.join("agents");
        let entries = std::fs::read_dir(&agents_root)
            .with_context(|| format!("reading agents root {}", agents_root.display()))?;
        let mut removed = 0usize;
        for entry in entries {
            let entry =
                entry.with_context(|| format!("reading agents root {}", agents_root.display()))?;
            let file_name = entry.file_name();
            if !file_name.to_string_lossy().ends_with(".deleting_trash") {
                continue;
            }
            if file_name.to_str().is_none() {
                // Runtime-created trash names are UTF-8; leave anything
                // else for an operator instead of deleting silently.
                warn!(entry = %entry.path().display(), "skipping non-UTF-8 agent home trash name");
                continue;
            }
            let trash_dir = entry.path();
            match self.remove_orphan_agent_home_trash(&trash_dir).await {
                Ok(true) => {
                    removed += 1;
                    info!(trash = %trash_dir.display(), "removed orphan agent home trash");
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(trash = %trash_dir.display(), %error, "failed to remove orphan agent home trash")
                }
            }
        }
        Ok(removed)
    }

    pub(crate) async fn remove_orphan_agent_home_trash(&self, trash_dir: &Path) -> Result<bool> {
        // Enumeration is not ownership evidence. Recheck under the same fence
        // as deletion admission and Home rename/removal, including legacy aliases.
        let _home_guard = self.inner.agent_home_cleanup_lock.lock().await;
        let agents_root = self.config().data_dir.join("agents");
        let Some(name) = trash_dir.file_name().and_then(|name| name.to_str()) else {
            return Ok(false);
        };
        if !name.ends_with(".deleting_trash")
            || self.registered_home_claimant(name)?.is_some()
            || self
                .registered_trash_alias_claimant(trash_dir, None)?
                .is_some()
        {
            return Ok(false);
        }
        ensure_deletable_agent_home(trash_dir, &agents_root)?;
        match std::fs::remove_dir_all(trash_dir) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| {
                format!("removing orphan agent home trash {}", trash_dir.display())
            }),
        }
    }

    /// Whether `name` belongs to a registered non-deleted agent identity.
    ///
    /// `*.deleting_trash` names can collide with legacy agent ids created
    /// before the suffix was reserved; such a directory is a live home, not
    /// trash, and must never be removed as trash.
    fn registered_home_claimant(&self, name: &str) -> Result<Option<AgentIdentityRecord>> {
        Ok(match self.runtime_db().agent_identities().latest(name)? {
            Some(identity) if identity.status != AgentRegistryStatus::Deleted => Some(identity),
            _ => None,
        })
    }

    /// Home removal and orphan sweeping use the same full/legacy ownership test.
    fn registered_trash_alias_claimant(
        &self,
        trash_dir: &Path,
        excluded_agent_id: Option<&str>,
    ) -> Result<Option<AgentIdentityRecord>> {
        let agents_root = self.config().data_dir.join("agents");
        Ok(self
            .runtime_db()
            .agent_identities()
            .latest_all()?
            .into_iter()
            .find(|identity| {
                identity.status != AgentRegistryStatus::Deleted
                    && Some(identity.agent_id.as_str()) != excluded_agent_id
                    && (agents_root.join(format!("{}.deleting_trash", identity.agent_id))
                        == trash_dir
                        || self
                            .agent_data_dir(&identity.agent_id)
                            .with_extension("deleting_trash")
                            == trash_dir)
            }))
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

    async fn process_due_deletion_batch(&self) -> Result<()> {
        let jobs = self
            .runtime_db()
            .agent_deletions()
            .due_jobs(Utc::now(), DELETION_BATCH_LIMIT)?;
        for job in jobs {
            if self.inner.daemon_deletion_token.is_cancelled() {
                break;
            }
            let _ = self.execute_deletion_job(job).await;
        }
        Ok(())
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

        // Legacy jobs can have advanced past Quiesce without ever settling or
        // purging private history. Persist a restart at the cancellation gate,
        // rather than skipping the new contract. A completed purge must not
        // replay writers (including the Quiesce audit) and recreate residue.
        if job.phase > AgentDeletionPhase::Quiesce {
            let ready = if job.phase > AgentDeletionPhase::Index {
                crate::runtime_db::agent_deletion_cleanup::assert_private_history_cleared(
                    self.runtime_db(),
                    &agent_id,
                )
            } else {
                crate::runtime_db::agent_deletion_cleanup::assert_execution_settled(
                    self.runtime_db(),
                    &agent_id,
                )
            };
            if let Err(error) = ready {
                debug!(agent_id, %error, "restarting deletion without current cleanup proof");
                job.phase = if job.mode == AgentDeletionMode::CleanupRepair {
                    AgentDeletionPhase::Quiesce
                } else {
                    AgentDeletionPhase::Fence
                };
            }
        }
        // A cleanup repair whose incarnation already settled and purged
        // private history must not replay settlement writers: only residue
        // re-purge can remain, so resume at the purge phase instead of the
        // Quiesce gate, where the settlement short-circuit fails forever.
        if job.mode == AgentDeletionMode::CleanupRepair
            && job.phase == AgentDeletionPhase::Quiesce
            && self
                .agent_identity_record(&agent_id)?
                .is_some_and(|identity| {
                    crate::runtime_db::agent_deletion_cleanup::private_history_purge_proof_is_current(
                        self.runtime_db(),
                        &agent_id,
                        identity.incarnation,
                    )
                })
        {
            debug!(
                agent_id = %agent_id,
                "cleanup repair resumes at the private history purge phase"
            );
            job.phase = AgentDeletionPhase::Index;
        }
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
                    if err.downcast_ref::<DeletionMetadataPending>().is_some() {
                        // Requeue only this exact Running checkpoint. Finalize
                        // may already have ACKed Completed; never overwrite it or
                        // treat a job read as an identity mutation receipt.
                        let now = Utc::now();
                        let deadline =
                            now + chrono::Duration::from_std(deletion_retry_delay(&job))?;
                        if let Err(checkpoint_error) = self
                            .runtime_db()
                            .agent_deletions()
                            .requeue_metadata_pending(&job, &format!("{err:#}"), now, deadline)
                        {
                            return Err(err.context(format!(
                                "could not requeue pending deletion metadata checkpoint: {checkpoint_error:#}"
                            )));
                        }
                        return Err(err);
                    }
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
        let policy = AgentDeletionPolicy::from(job.mode);
        match phase {
            AgentDeletionPhase::Fence => self.deletion_phase_fence(agent_id, policy).await,
            AgentDeletionPhase::Quiesce => self.deletion_phase_quiesce(agent_id, job, policy).await,
            AgentDeletionPhase::Ingress => self.deletion_phase_ingress(agent_id, job).await,
            AgentDeletionPhase::Scheduler => self.deletion_phase_scheduler(agent_id).await,
            AgentDeletionPhase::Workspace => self.deletion_phase_workspace(agent_id).await,
            AgentDeletionPhase::Archive => self.deletion_phase_archive(agent_id, job).await,
            AgentDeletionPhase::Index => self.deletion_phase_index(agent_id).await,
            AgentDeletionPhase::Home => self.deletion_phase_home(agent_id, policy).await,
            AgentDeletionPhase::Finalize => {
                unreachable!("finalize uses the atomic repository path")
            }
        }
    }

    /// Fence: verify the identity is in Deleting state and the runtime is
    /// unloaded. Phase 0-1 already sets the identity and unloads; this is a
    /// safety check for crash recovery.
    async fn deletion_phase_fence(
        &self,
        agent_id: &str,
        policy: AgentDeletionPolicy,
    ) -> Result<()> {
        let identity = self
            .agent_identity_record(agent_id)?
            .ok_or_else(|| anyhow!("agent {agent_id} identity not found"))?;
        if identity.status != AgentRegistryStatus::Deleting {
            return Err(anyhow!(
                "agent {agent_id} identity is {:?}, expected Deleting",
                identity.status
            ));
        }
        if policy == AgentDeletionPolicy::PreserveProtectedResources {
            self.finish_idle_runtime_exit(agent_id, None).await
        } else {
            // Later cleanup has identity-fenced, unloaded job authority. A failed
            // drain must not advance into those phases while a commit is unresolved.
            self.drain_retired_runtime(agent_id, identity.incarnation)
                .await
        }
    }

    /// Quiesce: terminalize active tasks, cancel wait conditions and timers.
    /// If cascade_private_children is set, drive private children through
    /// deletion first.
    async fn deletion_phase_quiesce(
        &self,
        agent_id: &str,
        job: &AgentDeletionJob,
        policy: AgentDeletionPolicy,
    ) -> Result<()> {
        if policy == AgentDeletionPolicy::PreserveProtectedResources {
            self.runtime_db().transaction(|tx| {
                if let Some(blocker) =
                    crate::runtime_db::reclamation::parent_cleanup_blocker_tx(tx, agent_id)?
                {
                    anyhow::bail!("agent_cleanup_blocked: {blocker}");
                }
                Ok(())
            })?;
        }
        // Retention timers and pending wakes cannot outlive either policy.
        self.cancel_deletion_timers(agent_id, job, Utc::now())
            .await?;
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

        // Safe cleanup must not enter the destructive per-Task cancellation path.
        let tasks_count = match policy {
            AgentDeletionPolicy::PreserveProtectedResources => 0,
            AgentDeletionPolicy::DeleteAgentData => {
                self.terminalize_deletion_tasks(agent_id, job, now).await?
            }
        };
        if tasks_count > 0 {
            debug!(agent_id, count = tasks_count, "terminalized active tasks");
        }

        // Settlement cancels active and triggered waits atomically with its proof.
        let waits_count = self
            .runtime_db()
            .wait_conditions()
            .unresolved_for_agent(agent_id)?
            .len();

        let identity = self
            .agent_identity_record(agent_id)?
            .ok_or_else(|| anyhow!("deletion identity not found"))?;
        crate::runtime_db::agent_deletion_cleanup::settle_execution(
            self.runtime_db(),
            agent_id,
            identity.incarnation,
            now,
        )?;

        // Emit audit event via storage if available.
        if let Some(storage) = storage {
            let _ = storage.append_event(&AuditEvent::legacy(
                "deletion_quiesce",
                serde_json::json!({
                    "agent_id": agent_id,
                    "tasks_cancelled": tasks_count,
                    "waits_cancelled": waits_count,
                    "execution_settled": true,
                }),
            ));
        }

        Ok(())
    }

    /// Ingress: revoke all active external triggers for the agent.
    async fn deletion_phase_ingress(&self, agent_id: &str, job: &AgentDeletionJob) -> Result<()> {
        let revoked_count = self
            .revoke_deletion_triggers(agent_id, job, Utc::now())
            .await?;
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
                self.commit_deletion_work_item_transition(
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
            self.commit_deletion_work_item_transition(
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
        if terminalizing_work_items.is_empty() {
            let command = self.deletion_focus_transition(agent_id)?;
            self.runtime_db()
                .transitions()
                .commit_agent_deletion_work_item(&command)?;
        }
        debug!(
            agent_id,
            work_items_completed = terminalizing_work_items.len(),
            "scheduler phase terminalized incomplete work items"
        );
        Ok(())
    }

    fn deletion_focus_transition(
        &self,
        agent_id: &str,
    ) -> Result<crate::runtime_db::transitions::WorkItemFocusTransitionCommand> {
        use crate::runtime_db::transitions::{AgentStateMutation, WorkItemFocusTransitionCommand};
        let expected = self.runtime_db().agent_states().latest(agent_id)?;
        let mut state = expected
            .clone()
            .unwrap_or_else(|| AgentState::new(agent_id));
        state.status = AgentStatus::Stopped;
        state.pending = 0;
        state.sleeping_until = None;
        state.pending_wake_hint = None;
        state.current_work_item_id = None;
        state.current_turn_work_item_id = None;
        state.current_execution_binding = None;
        state.current_run_id = None;
        state.current_turn_id = None;
        state.current_turn_operator_binding_id = None;
        state.current_turn_operator_reply_route_id = None;
        let active = self
            .runtime_db()
            .work_item_continuations()
            .active_for_agent(agent_id)?;
        let now = Utc::now();
        let continuations = active
            .iter()
            .cloned()
            .map(|mut frame| {
                frame.state = WorkItemContinuationState::Cancelled;
                frame.updated_at = now;
                frame.cancelled_at = Some(now);
                frame.resolution_reason = Some("agent_deleted".into());
                frame
            })
            .collect();
        Ok(WorkItemFocusTransitionCommand {
            agent_id: agent_id.into(),
            work_items: Vec::new(),
            wait_conditions: Vec::new(),
            active_continuation_expectations: Some(active.clone()),
            continuation_expectations: active,
            continuations,
            agent_state: AgentStateMutation {
                expected: expected.map(Box::new),
                record: Box::new(state),
            },
            brief_evidence: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: true,
            fault: None,
        })
    }

    fn commit_deletion_work_item_transition(
        &self,
        work_item: &crate::runtime_db::transitions::WorkItemTransitionCommand,
    ) -> Result<()> {
        let mut command = self.deletion_focus_transition(&work_item.agent_id)?;
        command.work_items.push(work_item.mutation.clone());
        command.brief_evidence = work_item.brief_evidence.clone();
        command.audit_events = work_item.audit_events.clone();
        command.index_changes = work_item.index_changes.clone();
        self.runtime_db()
            .transitions()
            .commit_agent_deletion_work_item(&command)?;
        Ok(())
    }

    /// Validate or remove exclusively owned clean runtime-created worktrees.
    pub(crate) async fn check_agent_deletion_workspaces(
        &self,
        agent_id: &str,
        remove: bool,
    ) -> Result<()> {
        // No force-removal or directory fallback. Lease prevents concurrent use.
        let roots = self.runtime_db().execution_root_entries().latest_all()?;
        for root in roots {
            if root.removed_at.is_some() {
                continue;
            }
            let Some(worktree) = &root.worktree else {
                continue;
            };
            if worktree.registered_by_agent_id.as_deref() != Some(agent_id)
                && !worktree
                    .authorized_agent_ids
                    .iter()
                    .any(|id| id == agent_id)
            {
                continue;
            }
            anyhow::ensure!(
                worktree.registered_by_agent_id.as_deref() == Some(agent_id)
                    && worktree.provenance == WorktreeProvenance::RuntimeCreated,
                "agent_cleanup_blocked: foreign_or_unowned_worktree"
            );
            anyhow::ensure!(
                !worktree
                    .authorized_agent_ids
                    .iter()
                    .any(|id| id != agent_id),
                "agent_cleanup_blocked: shared_worktree"
            );
            let _lease = self.acquire_workspace_cleanup_lease(&root.execution_root_id)?;
            if root.filesystem_path.exists() {
                let path = root.filesystem_path.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let status = std::process::Command::new("git")
                        .args(["status", "--porcelain", "--untracked-files=all"])
                        .current_dir(&path)
                        .output()?;
                    anyhow::ensure!(
                        status.status.success() && status.stdout.is_empty(),
                        "agent_cleanup_blocked: dirty_or_unknown_worktree"
                    );
                    if !remove {
                        return Ok(());
                    }
                    let removed = std::process::Command::new("git")
                        .args(["worktree", "remove"])
                        .arg(&path)
                        .current_dir(&path)
                        .output()?;
                    anyhow::ensure!(
                        removed.status.success(),
                        "git refused safe worktree removal: {}",
                        String::from_utf8_lossy(&removed.stderr)
                    );
                    Ok(())
                })
                .await??;
            }
            if !remove {
                continue;
            }
            self.runtime_db()
                .execution_root_entries()
                .mark_removed(&root.execution_root_id)?;
        }
        Ok(())
    }

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

        self.check_agent_deletion_workspaces(agent_id, true).await
    }

    async fn deletion_phase_archive(&self, agent_id: &str, job: &AgentDeletionJob) -> Result<()> {
        let Some(destination) = &job.archive_path else {
            return Ok(());
        };
        anyhow::ensure!(
            destination
                == &self
                    .config()
                    .data_dir
                    .join("archives")
                    .join(format!("{}.tar.gz", job.deletion_id)),
            "agent_archive_destination_mismatch"
        );
        let identity = self
            .agent_identity_record(agent_id)?
            .ok_or_else(|| anyhow!("archive identity not found"))?;
        anyhow::ensure!(
            identity.status != AgentRegistryStatus::Active,
            "agent_archive_requires_deletion_fence"
        );
        let binding = archive::ArchiveBinding {
            agent_id: agent_id.into(),
            incarnation: identity.incarnation,
            deletion_id: job.deletion_id.clone(),
        };
        let destination = destination.clone();
        let home = self.agent_data_dir(agent_id);
        let db = self.runtime_db().clone();
        let _home_guard = self.inner.agent_home_cleanup_lock.lock().await;
        tokio::task::spawn_blocking(move || {
            let history = crate::runtime_db::agent_deletion_cleanup::export_private_history(
                &db,
                &binding.agent_id,
                binding.incarnation,
            )?;
            archive::write_archive(&destination, &home, binding, &history)
        })
        .await??;
        Ok(())
    }

    /// Index: clear private history only after any requested archive is sealed.
    async fn deletion_phase_index(&self, agent_id: &str) -> Result<()> {
        self.runtime_db()
            .runtime_index_outbox()
            .delete_all_for_agent(agent_id)?;
        // The memory index is a shared SQLite database. Remove all rows
        // belonging to this agent.
        let storage = self.agent_storage(&self.config().default_agent_id)?;
        crate::memory::index::delete_agent_memory_index_projection(
            &storage.shared_indexes_dir(),
            agent_id,
        )?;
        let identity = self
            .agent_identity_record(agent_id)?
            .ok_or_else(|| anyhow!("deletion identity not found"))?;
        crate::runtime_db::agent_deletion_cleanup::purge_private_history(
            self.runtime_db(),
            agent_id,
            identity.incarnation,
            Utc::now(),
        )?;
        debug!(agent_id, "removed agent from memory index");
        Ok(())
    }

    /// Home: rename agent home to trash then delete.
    async fn deletion_phase_home(&self, agent_id: &str, policy: AgentDeletionPolicy) -> Result<()> {
        let _home_guard = self.inner.agent_home_cleanup_lock.lock().await;
        let data_dir = self.agent_data_dir(agent_id);
        // Refuse to delete anything that is not a runtime-managed directory:
        // symlinked homes or paths resolving outside the agents root fail the
        // job instead of removing unintended files.
        let agents_root = self.config().data_dir.join("agents");
        // Rename to a trash name first to avoid partial-state visibility.
        let trash_dir = self.resolve_agent_home_trash_dir(agent_id, policy)?;
        if !agent_home_present(&data_dir)? {
            // The home is already gone, but a previous attempt may have
            // renamed it to trash and failed before removing the trash.
            // Finish removing the leftover trash so a retry cannot complete
            // the job while silently leaking the directory.
            if agent_home_present(&trash_dir)? {
                ensure_agent_home_deletion_safe(&trash_dir, &agents_root, policy)?;
                self.ensure_trash_dir_unclaimed(&trash_dir, agent_id)?;
                info!(agent_id, trash = %trash_dir.display(), "removing leftover agent home trash");
                std::fs::remove_dir_all(&trash_dir).with_context(|| {
                    format!("removing agent home trash {}", trash_dir.display())
                })?;
            }
            return Ok(());
        }
        ensure_agent_home_deletion_safe(&data_dir, &agents_root, policy)?;
        if agent_home_present(&trash_dir)? {
            // Previous attempt left trash; remove it.
            self.ensure_trash_dir_unclaimed(&trash_dir, agent_id)?;
            ensure_agent_home_deletion_safe(&trash_dir, &agents_root, policy)?;
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

    /// Full IDs own distinct trash names. Recover a legacy extension-replaced
    /// name only when no other registered identity can own that alias.
    fn resolve_agent_home_trash_dir(
        &self,
        agent_id: &str,
        policy: AgentDeletionPolicy,
    ) -> Result<std::path::PathBuf> {
        let agents_root = self.config().data_dir.join("agents");
        let home = self.agent_data_dir(agent_id);
        let trash = agents_root.join(format!("{agent_id}.deleting_trash"));
        self.ensure_trash_dir_unclaimed(&trash, agent_id)?;
        let legacy = home.with_extension("deleting_trash");
        if legacy == trash || !agent_home_present(&legacy)? {
            return Ok(trash);
        }
        self.ensure_trash_dir_unclaimed(&legacy, agent_id)?;
        anyhow::ensure!(
            !self
                .runtime_db()
                .agent_identities()
                .latest_all()?
                .iter()
                .any(|identity| identity.agent_id != agent_id
                    && self
                        .agent_data_dir(&identity.agent_id)
                        .with_extension("deleting_trash")
                        == legacy),
            "ambiguous legacy agent home trash {}; refusing deletion",
            legacy.display()
        );
        anyhow::ensure!(
            !agent_home_present(&trash)?,
            "both legacy and current agent home trash exist; resolve before deletion"
        );
        ensure_agent_home_deletion_safe(&legacy, &agents_root, policy)?;
        std::fs::rename(&legacy, &trash)
            .with_context(|| format!("migrating legacy agent home trash {}", legacy.display()))?;
        Ok(trash)
    }

    /// Refuse another identity's home or either version of its trash path.
    fn ensure_trash_dir_unclaimed(&self, trash_dir: &Path, agent_id: &str) -> Result<()> {
        if let Some(name) = trash_dir.file_name().and_then(|n| n.to_str()) {
            if let Some(claimant) = self.registered_home_claimant(name)? {
                anyhow::bail!(
                    "{} is a live home of agent {} colliding with the trash suffix",
                    trash_dir.display(),
                    claimant.agent_id
                );
            }
        }
        if let Some(claimant) = self.registered_trash_alias_claimant(trash_dir, Some(agent_id))? {
            anyhow::bail!(
                "ambiguous legacy agent home trash {}; aliases trash owned by agent {}; refusing deletion",
                trash_dir.display(),
                claimant.agent_id
            );
        }
        Ok(())
    }

    /// Finalize: set identity to Deleted and emit audit event.
    async fn deletion_phase_finalize(&self, job: &AgentDeletionJob) -> Result<AgentDeletionJob> {
        let command = job.clone();
        let completed_job = self
            .commit_deletion_identity_metadata(
                job.agent_id.clone(),
                None,
                Vec::new(),
                move |host| {
                    let (identity, completed_job) =
                        host.runtime_db().agent_deletions().finalize(&command)?;
                    Ok((completed_job, identity))
                },
            )
            .await?;
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
            let child_id = child.agent_id.clone();
            let requested_by = parent_job.requested_by.clone();
            let (_, child_job, _) = self
                .commit_deletion_identity_metadata(
                    child_id.clone(),
                    Some(child.incarnation),
                    vec![parent_agent_id.to_string()],
                    move |host| {
                        let result = host.runtime_db().agent_deletions().begin(
                            &child.agent_id,
                            child.revision,
                            &requested_by,
                            false, // Don't recurse further
                        )?;
                        Ok((result.clone(), result.0))
                    },
                )
                .await?;
            // The short worker released both owners and bootstrap before the
            // child job enters unload, recursive phases or filesystem cleanup.
            Box::pin(self.execute_deletion_job(child_job))
                .await
                .with_context(|| format!("cascading deletion to private child {child_id}"))?;
        }
        Ok(())
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
/// Runtime-owned state/output is disposable; user deliverables require the
/// destructive deletion policy. The documented-disposable `tmp/` scratch
/// directory is removable like runtime output. Both policies enforce the same
/// path safety boundary.
pub(crate) fn ensure_agent_home_deletion_safe(
    data_dir: &Path,
    agents_root: &Path,
    policy: AgentDeletionPolicy,
) -> Result<()> {
    if !agent_home_present(data_dir)? {
        return Ok(());
    }
    ensure_deletable_agent_home(data_dir, agents_root)?;
    if policy == AgentDeletionPolicy::DeleteAgentData {
        return Ok(());
    }
    for entry in std::fs::read_dir(data_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        let runtime_owned = (name == "AGENTS.md" && kind.is_file())
            || (kind.is_dir()
                && matches!(
                    name.to_str(),
                    Some(
                        ".holon"
                            | "memory"
                            | "skills"
                            | "work-items"
                            | "tool-artifacts"
                            | "task-output"
                    )
                ));
        if runtime_owned {
            continue;
        }
        // `tmp/` is documented disposable scratch space, so any real
        // directory content may go. Other user directories are protected
        // deliverable locations unless they are empty scaffolds.
        let disposable_tmp = name == "tmp" && kind.is_dir();
        let empty_scaffold = matches!(name.to_str(), Some("notes" | "work" | "media" | "apps"))
            && kind.is_dir()
            && std::fs::read_dir(entry.path())?.next().is_none();
        anyhow::ensure!(
            disposable_tmp || empty_scaffold,
            "agent_cleanup_blocked: unregistered_home_artifact {}",
            entry.path().display()
        );
    }
    Ok(())
}

fn agent_home_present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("inspecting agent home {}", path.display()))
        }
    }
}

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
    if canonical_home == canonical_root || !canonical_home.starts_with(&canonical_root) {
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

    pub(super) fn metadata_test_host() -> (tempfile::TempDir, RuntimeHost) {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let config =
            crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let host = RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new("unused")),
        )
        .unwrap();
        (home, host)
    }

    pub(super) fn metadata_identity(host: &RuntimeHost, agent_id: &str) -> AgentIdentityRecord {
        let identity = AgentIdentityRecord::new(
            agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        host.runtime_db()
            .agent_identities()
            .upsert(&identity)
            .unwrap();
        let mut state = AgentState::new(agent_id);
        state.status = AgentStatus::Stopped;
        state.total_input_tokens = 37;
        host.runtime_db()
            .agent_states()
            .upsert_for_fixture(&state)
            .unwrap();
        identity
    }

    async fn metadata_finalization_fixture(
        host: &RuntimeHost,
        identity: &AgentIdentityRecord,
    ) -> (
        AgentDeletionJob,
        crate::runtime::mutation_owner::MutationOwner,
    ) {
        // Real cleanup leaves an explicit absent-State ACK for the cold owner.
        let owner = host
            .bridge()
            .mutation_owner_for(&identity.agent_id, identity.incarnation)
            .unwrap();
        let agent_id = identity.agent_id.clone();
        let revision = identity.revision;
        let (_, job, _) = host
            .commit_deletion_identity_metadata(
                agent_id.clone(),
                Some(identity.incarnation),
                Vec::new(),
                move |host| {
                    let result = host
                        .runtime_db()
                        .agent_deletions()
                        .begin(&agent_id, revision, "fixture", false)?;
                    Ok((result.clone(), result.0))
                },
            )
            .await
            .unwrap();
        crate::runtime_db::agent_deletion_cleanup::settle_execution(
            host.runtime_db(),
            &identity.agent_id,
            identity.incarnation,
            Utc::now(),
        )
        .unwrap();
        crate::runtime_db::agent_deletion_cleanup::purge_private_history(
            host.runtime_db(),
            &identity.agent_id,
            identity.incarnation,
            Utc::now(),
        )
        .unwrap();
        crate::runtime_db::agent_deletion_cleanup::assert_private_history_cleared(
            host.runtime_db(),
            &identity.agent_id,
        )
        .unwrap();
        (job, owner)
    }

    #[tokio::test]
    async fn deletion_finalize_known_publication_fault_restores_exact_ack() {
        let (_home, host) = metadata_test_host();
        let identity = metadata_identity(&host, "finalize-known");
        let (mut job, owner) = metadata_finalization_fixture(&host, &identity).await;
        assert!(
            host.runtime_db()
                .agent_states()
                .latest(&identity.agent_id)
                .unwrap()
                .is_none(),
            "real purge must clear State before finalization"
        );
        job.phase = AgentDeletionPhase::Finalize;
        job.status = AgentDeletionStatus::Running;
        host.runtime_db().agent_deletions().update(&job).unwrap();
        host.fail_next_external_mutation_publication();
        let completed = host.deletion_phase_finalize(&job).await.unwrap();
        assert_eq!(completed.status, AgentDeletionStatus::Completed);
        assert_eq!(
            host.runtime_db()
                .agent_deletions()
                .latest_for_agent(&identity.agent_id)
                .unwrap()
                .unwrap(),
            completed
        );
        let durable = host
            .runtime_db()
            .agent_identities()
            .latest(&identity.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(durable.status, AgentRegistryStatus::Deleted);
        assert_eq!(durable.revision, identity.revision + 2);
        assert_eq!(
            host.agent_identity_record(&identity.agent_id)
                .unwrap()
                .unwrap(),
            durable
        );
        assert!(
            host.runtime_db()
                .agent_states()
                .latest(&identity.agent_id)
                .unwrap()
                .is_none(),
            "finalization must not recreate purged State"
        );
        assert!(
            owner.capture().unwrap().is_none(),
            "exact metadata ACK must retain State absence after publication recovery"
        );
        assert!(
            owner.try_reserve().is_ok(),
            "finalize must release its short reservation"
        );
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn deletion_metadata_known_fault_does_not_replay_original_sql() {
        let (_home, host) = metadata_test_host();
        let identity = metadata_identity(&host, "known-once");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let persisted_calls = calls.clone();
        host.fail_next_external_mutation_publication();
        let (_, job, created) = host
            .commit_deletion_identity_metadata(
                identity.agent_id.clone(),
                Some(identity.incarnation),
                Vec::new(),
                move |host| {
                    persisted_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let result = host.runtime_db().agent_deletions().begin(
                        &identity.agent_id,
                        identity.revision,
                        "fixture",
                        false,
                    )?;
                    Ok((result.clone(), result.0))
                },
            )
            .await
            .unwrap();
        assert!(created);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            host.runtime_db()
                .agent_deletions()
                .latest_for_agent("known-once")
                .unwrap()
                .unwrap(),
            job
        );
        let owner = host.bridge().mutation_owner_for("known-once", 1).unwrap();
        assert!(owner.capture().is_ok());
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn deletion_metadata_unknown_fences_whole_group_without_replay() {
        let (_home, host) = metadata_test_host();
        metadata_identity(&host, "a-parent");
        let child = metadata_identity(&host, "z-child");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let persisted_calls = calls.clone();
        let result: Result<()> = host
            .commit_deletion_identity_metadata(
                child.agent_id.clone(),
                Some(child.incarnation),
                vec!["a-parent".into()],
                move |host| {
                    persisted_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    host.runtime_db().agent_deletions().begin(
                        &child.agent_id,
                        child.revision,
                        "fixture",
                        false,
                    )?;
                    Err(crate::runtime_db::persistence::mark_persistence_failure(
                        anyhow!("fixture lost SQL acknowledgment"),
                        crate::runtime_db::PersistenceCertainty::Indeterminate,
                        crate::runtime_db::persistence::PersistenceFailurePhase::Commit,
                        None,
                    ))
                },
            )
            .await;
        let error = result.unwrap_err();
        assert!(error.downcast_ref::<DeletionMetadataPending>().is_some());
        assert_eq!(
            crate::runtime_db::persistence_certainty(&error),
            Some(crate::runtime_db::PersistenceCertainty::Indeterminate)
        );
        let mut operation_id = None;
        for agent_id in ["a-parent", "z-child"] {
            let owner = host.bridge().mutation_owner_for(agent_id, 1).unwrap();
            let key = match owner.capture().unwrap_err() {
                crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(key) => key,
                other => panic!("unknown metadata must stay fenced: {other:?}"),
            };
            if let Some(expected) = &operation_id {
                assert_eq!(&key.operation_id, expected);
            } else {
                operation_id = Some(key.operation_id);
            }
            let reservation = owner.reserve().await.unwrap();
            assert!(reservation.recovery().unwrap().unwrap().known.is_none());
        }
        let replayed_calls = calls.clone();
        let result = host
            .commit_deletion_identity_metadata(
                "z-child".into(),
                Some(1),
                vec!["a-parent".into()],
                move |host| {
                    replayed_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok((
                        (),
                        host.runtime_db()
                            .agent_identities()
                            .latest("z-child")?
                            .unwrap(),
                    ))
                },
            )
            .await;
        assert!(result.is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            host.runtime_db()
                .agent_deletions()
                .latest_for_agent("z-child")
                .unwrap()
                .unwrap()
                .status,
            AgentDeletionStatus::Pending
        );
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn deletion_metadata_pending_requeues_exact_running_checkpoint_without_ack() {
        let (_home, host) = metadata_test_host();
        let identity = metadata_identity(&host, "pending-running-checkpoint");
        let (mut job, owner) = metadata_finalization_fixture(&host, &identity).await;
        job.phase = AgentDeletionPhase::Finalize;
        host.runtime_db().agent_deletions().update(&job).unwrap();
        let bootstrap = host.agent_bootstrap_lock(&identity.agent_id);
        let blocked = bootstrap.lock().await;
        let mut execution = Box::pin(host.execute_deletion_job(job));
        assert!(futures_util::poll!(execution.as_mut()).is_pending());
        let running = host
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&identity.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(running.status, AgentDeletionStatus::Running);
        let mut reservation = owner.reserve().await.unwrap();
        let key = reservation
            .begin("fixture-unacknowledged-finalize".into())
            .unwrap();
        drop(reservation);
        drop(blocked);
        let error = execution.await.unwrap_err();
        assert!(error.downcast_ref::<DeletionMetadataPending>().is_some());
        let retry = host
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&identity.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(retry.status, AgentDeletionStatus::RetryableFailed);
        assert_eq!(retry.deletion_id, running.deletion_id);
        assert_eq!(retry.phase, running.phase);
        assert_eq!(retry.attempts, running.attempts);
        assert!(retry.next_attempt_at.is_some());
        assert!(matches!(owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(current)) if current == key));
        assert!(owner
            .reserve()
            .await
            .unwrap()
            .recovery()
            .unwrap()
            .unwrap()
            .known
            .is_none());
        assert_eq!(
            host.runtime_db()
                .agent_identities()
                .latest(&identity.agent_id)
                .unwrap()
                .unwrap()
                .status,
            AgentRegistryStatus::Deleting
        );
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn deletion_requeued_known_metadata_checkpoint_can_resume_finalization() {
        let (_home, host) = metadata_test_host();
        let identity = metadata_identity(&host, "known-requeued-checkpoint");
        let (mut job, _owner) = metadata_finalization_fixture(&host, &identity).await;
        job.phase = AgentDeletionPhase::Finalize;
        job.status = AgentDeletionStatus::Running;
        host.runtime_db().agent_deletions().update(&job).unwrap();
        let mut reservations = host
            .reserve_agent_mutations(&[identity.agent_id.clone()])
            .await
            .unwrap();
        let facts = host.external_identity_facts(&reservations).unwrap();
        // A real acknowledged metadata transaction is retained before install.
        host.acknowledge_external_metadata(
            &mut reservations,
            "fixture-known-checkpoint".into(),
            || {
                host.runtime_db().transaction(|_| Ok(()))?;
                Ok(((), facts))
            },
        )
        .unwrap();
        let now = Utc::now();
        assert!(host
            .runtime_db()
            .agent_deletions()
            .requeue_metadata_pending(&job, "known metadata pending", now, now)
            .unwrap());
        drop(reservations);
        let retry = host
            .runtime_db()
            .agent_deletions()
            .due_jobs(Utc::now(), 10)
            .unwrap()
            .into_iter()
            .find(|retry| retry.deletion_id == job.deletion_id)
            .expect("requeued checkpoint is discoverable");
        host.execute_deletion_job(retry).await.unwrap();
        let completed = host
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&identity.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, AgentDeletionStatus::Completed);
        assert_eq!(completed.attempts, job.attempts + 1);
        let durable = host
            .runtime_db()
            .agent_identities()
            .latest(&identity.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(durable.status, AgentRegistryStatus::Deleted);
        assert_eq!(durable.revision, identity.revision + 2);
        assert!(host
            .bridge()
            .mutation_owner_for(&identity.agent_id, identity.incarnation)
            .unwrap()
            .capture()
            .is_ok());
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn deletion_metadata_pending_does_not_requeue_an_advanced_checkpoint() {
        let (_home, host) = metadata_test_host();
        let identity = metadata_identity(&host, "advanced-checkpoint");
        let (_, mut expected, _) = host
            .runtime_db()
            .agent_deletions()
            .begin(&identity.agent_id, identity.revision, "fixture", false)
            .unwrap();
        expected.status = AgentDeletionStatus::Running;
        host.runtime_db()
            .agent_deletions()
            .update(&expected)
            .unwrap();
        let mut advanced = expected.clone();
        advanced.phase = AgentDeletionPhase::Quiesce;
        advanced.updated_at += chrono::Duration::nanoseconds(1);
        host.runtime_db()
            .agent_deletions()
            .update(&advanced)
            .unwrap();
        let now = Utc::now();
        assert!(!host
            .runtime_db()
            .agent_deletions()
            .requeue_metadata_pending(&expected, "stale phase failure", now, now)
            .unwrap());
        assert_eq!(
            host.runtime_db()
                .agent_deletions()
                .latest_for_agent(&identity.agent_id)
                .unwrap()
                .unwrap(),
            advanced
        );
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn deletion_finalize_existing_unknown_preserves_completed_checkpoint() {
        let (_home, host) = metadata_test_host();
        let identity = metadata_identity(&host, "finalize-existing-unknown");
        let (mut job, owner) = metadata_finalization_fixture(&host, &identity).await;
        job.phase = AgentDeletionPhase::Finalize;
        host.runtime_db().agent_deletions().update(&job).unwrap();
        let bootstrap = host.agent_bootstrap_lock(&identity.agent_id);
        let blocked = bootstrap.lock().await;
        let mut execution = Box::pin(host.execute_deletion_job(job));
        // Claim the due job, then stop its short metadata worker before reserve.
        assert!(futures_util::poll!(execution.as_mut()).is_pending());
        let running = host
            .runtime_db()
            .agent_deletions()
            .latest_for_agent(&identity.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(running.status, AgentDeletionStatus::Running);
        let mut reservation = owner.reserve().await.unwrap();
        let key = reservation
            .begin("fixture-unknown-finalize".into())
            .unwrap();
        // The prior finalize did commit, but its ACK is unavailable to the owner.
        let (_, completed) = host
            .runtime_db()
            .agent_deletions()
            .finalize(&running)
            .unwrap();
        drop(reservation);
        drop(blocked);
        let error = execution.await.unwrap_err();
        assert!(error.downcast_ref::<DeletionMetadataPending>().is_some());
        assert!(matches!(owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(current))
                if current == key));
        assert_eq!(
            host.runtime_db()
                .agent_deletions()
                .latest_for_agent(&identity.agent_id)
                .unwrap()
                .unwrap(),
            completed
        );
        assert_eq!(completed.status, AgentDeletionStatus::Completed);
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn deletion_metadata_cancelled_caller_keeps_owned_short_commit() {
        let (_home, host) = metadata_test_host();
        let identity = metadata_identity(&host, "cancelled-metadata");
        let bootstrap = host.agent_bootstrap_lock(&identity.agent_id);
        let blocked = bootstrap.lock().await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let persisted_calls = calls.clone();
        let mut request = Box::pin(host.commit_deletion_identity_metadata(
            identity.agent_id.clone(),
            Some(identity.incarnation),
            Vec::new(),
            move |host| {
                persisted_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let result = host.runtime_db().agent_deletions().begin(
                    &identity.agent_id,
                    identity.revision,
                    "fixture",
                    false,
                )?;
                Ok((result.clone(), result.0))
            },
        ));
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        drop(request);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(blocked);
        tokio::time::timeout(Duration::from_secs(5), host.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            host.runtime_db()
                .agent_identities()
                .latest("cancelled-metadata")
                .unwrap()
                .unwrap()
                .status,
            AgentRegistryStatus::Deleting
        );
        assert!(host
            .bridge()
            .mutation_owner_for("cancelled-metadata", 1)
            .unwrap()
            .capture()
            .is_ok());
    }

    #[tokio::test]
    async fn deletion_metadata_opposite_scope_orders_release_all_owners() {
        let (_home, host) = metadata_test_host();
        let parent = metadata_identity(&host, "a-parent");
        let child = metadata_identity(&host, "z-child");
        let parent_host = host.clone();
        let child_host = host.clone();
        let parent_id = parent.agent_id.clone();
        let child_id = child.agent_id.clone();
        let operations = async {
            tokio::try_join!(
                parent_host.commit_deletion_identity_metadata(
                    parent_id.clone(),
                    Some(1),
                    vec![child_id.clone(), parent_id.clone()],
                    move |host| {
                        host.runtime_db().transaction(|_| Ok(()))?;
                        Ok(((), parent))
                    },
                ),
                child_host.commit_deletion_identity_metadata(
                    child_id.clone(),
                    Some(1),
                    vec![parent_id.clone(), child_id.clone()],
                    move |host| {
                        host.runtime_db().transaction(|_| Ok(()))?;
                        Ok(((), child))
                    },
                ),
            )
        };
        tokio::time::timeout(Duration::from_secs(5), operations)
            .await
            .unwrap()
            .unwrap();
        for agent_id in ["a-parent", "z-child"] {
            let owner = host.bridge().mutation_owner_for(agent_id, 1).unwrap();
            assert!(owner.capture().is_ok());
            assert!(owner.try_reserve().is_ok());
            assert!(host.agent_bootstrap_lock(agent_id).try_lock().is_ok());
        }
        host.shutdown().await.unwrap();
    }

    #[test]
    fn home_resource_policy_classifies_real_runtime_output_and_user_files() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("child");
        for name in [
            ".holon",
            "memory",
            "skills",
            "work-items",
            "tool-artifacts",
            "task-output",
        ] {
            std::fs::create_dir_all(home.join(name)).unwrap();
            std::fs::write(home.join(name).join("state"), "runtime-owned").unwrap();
        }
        std::fs::write(home.join("AGENTS.md"), "configuration").unwrap();
        for policy in [
            AgentDeletionPolicy::PreserveProtectedResources,
            AgentDeletionPolicy::DeleteAgentData,
        ] {
            ensure_agent_home_deletion_safe(&home, root.path(), policy).unwrap();
        }
        for name in ["work", "notes", "tmp", "media", "apps", "report.md"] {
            let artifact = home.join(name);
            std::fs::write(&artifact, "user result").unwrap();
            assert!(ensure_agent_home_deletion_safe(
                &home,
                root.path(),
                AgentDeletionPolicy::PreserveProtectedResources,
            )
            .unwrap_err()
            .to_string()
            .contains("unregistered_home_artifact"));
            ensure_agent_home_deletion_safe(
                &home,
                root.path(),
                AgentDeletionPolicy::DeleteAgentData,
            )
            .unwrap();
            assert_eq!(std::fs::read_to_string(&artifact).unwrap(), "user result");
            std::fs::remove_file(artifact).unwrap();
        }
    }

    #[test]
    fn home_resource_policy_allows_only_empty_user_directories() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("child");
        for name in ["work", "notes", "media", "apps"] {
            let directory = home.join(name);
            std::fs::create_dir_all(&directory).unwrap();
            for policy in [
                AgentDeletionPolicy::PreserveProtectedResources,
                AgentDeletionPolicy::DeleteAgentData,
            ] {
                ensure_agent_home_deletion_safe(&home, root.path(), policy).unwrap();
            }
            let artifact = directory.join("deliverable");
            std::fs::write(&artifact, "user result").unwrap();
            assert!(ensure_agent_home_deletion_safe(
                &home,
                root.path(),
                AgentDeletionPolicy::PreserveProtectedResources,
            )
            .unwrap_err()
            .to_string()
            .contains("unregistered_home_artifact"));
            ensure_agent_home_deletion_safe(
                &home,
                root.path(),
                AgentDeletionPolicy::DeleteAgentData,
            )
            .unwrap();
            assert_eq!(std::fs::read_to_string(&artifact).unwrap(), "user result");
            std::fs::remove_file(artifact).unwrap();
            ensure_agent_home_deletion_safe(
                &home,
                root.path(),
                AgentDeletionPolicy::PreserveProtectedResources,
            )
            .unwrap();
        }
    }

    #[test]
    fn home_resource_policy_treats_tmp_directory_as_disposable_scratch() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("child");
        let tmp = home.join("tmp");
        std::fs::create_dir_all(tmp.join("nested")).unwrap();
        std::fs::write(tmp.join("scratch.txt"), "stale").unwrap();
        std::fs::write(tmp.join("nested/.hidden"), "stale").unwrap();
        for policy in [
            AgentDeletionPolicy::PreserveProtectedResources,
            AgentDeletionPolicy::DeleteAgentData,
        ] {
            ensure_agent_home_deletion_safe(&home, root.path(), policy).unwrap();
        }
        // A file named tmp is not disposable scratch space.
        std::fs::remove_dir_all(&tmp).unwrap();
        std::fs::write(&tmp, "impostor").unwrap();
        assert!(ensure_agent_home_deletion_safe(
            &home,
            root.path(),
            AgentDeletionPolicy::PreserveProtectedResources,
        )
        .unwrap_err()
        .to_string()
        .contains("unregistered_home_artifact"));
        std::fs::remove_file(&tmp).unwrap();
        #[cfg(unix)]
        {
            // A symlink named tmp must not be classified as a disposable
            // directory, and classification alone must never touch its target.
            let victim = tempfile::tempdir().unwrap();
            std::fs::write(victim.path().join("sentinel"), "keep").unwrap();
            std::os::unix::fs::symlink(victim.path(), &tmp).unwrap();
            assert!(ensure_agent_home_deletion_safe(
                &home,
                root.path(),
                AgentDeletionPolicy::PreserveProtectedResources,
            )
            .unwrap_err()
            .to_string()
            .contains("unregistered_home_artifact"));
            assert_eq!(
                std::fs::read_to_string(victim.path().join("sentinel")).unwrap(),
                "keep"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn both_home_policies_reject_symlinks_and_paths_outside_agents_root() {
        let root = tempfile::tempdir().unwrap();
        let victim = tempfile::tempdir().unwrap();
        std::fs::write(victim.path().join("sentinel"), "keep").unwrap();
        let home = root.path().join("child");
        std::os::unix::fs::symlink(victim.path(), &home).unwrap();
        for policy in [
            AgentDeletionPolicy::PreserveProtectedResources,
            AgentDeletionPolicy::DeleteAgentData,
        ] {
            assert!(ensure_agent_home_deletion_safe(&home, root.path(), policy)
                .unwrap_err()
                .to_string()
                .contains("symlink"));
            assert!(
                ensure_agent_home_deletion_safe(victim.path(), root.path(), policy)
                    .unwrap_err()
                    .to_string()
                    .contains("outside agents root")
            );
            assert!(ensure_agent_home_deletion_safe(root.path(), root.path(), policy).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(victim.path().join("sentinel")).unwrap(),
            "keep"
        );
        std::fs::remove_file(&home).unwrap();
        std::os::unix::fs::symlink(victim.path().join("missing"), &home).unwrap();
        for policy in [
            AgentDeletionPolicy::PreserveProtectedResources,
            AgentDeletionPolicy::DeleteAgentData,
        ] {
            assert!(
                ensure_agent_home_deletion_safe(&home, root.path(), policy).is_err(),
                "a dangling symlink is not a missing home"
            );
        }
    }

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
            archive_path: None,
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
