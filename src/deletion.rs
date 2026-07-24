//! Reentrant agent deletion cleanup coordinator.
//!
//! Drives an [`AgentDeletionJob`] through its ordered phases from `Fence` to
//! `Finalize`. Each phase is idempotent: re-running a phase that has already
//! been completed is a safe no-op. On transient failure the job is marked
//! `RetryableFailed` with an actionable `last_error`; a subsequent retry
//! resumes from the failed phase.
//!
//! The coordinator is triggered:
//! - inline after `begin_public_agent_deletion`;
//! - on daemon startup for crash recovery;
//! - periodically for retry of failed jobs.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use tracing::{debug, info, warn};

use crate::host::RuntimeHost;
use crate::types::*;

/// Interval between periodic deletion coordinator sweeps.
const DELETION_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

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
        let host = self.clone();
        tokio::spawn(async move {
            host.run_daemon_deletion_coordinator().await;
        });
    }

    async fn run_daemon_deletion_coordinator(self) {
        let mut interval = tokio::time::interval(DELETION_SWEEP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = self.inner.daemon_deletion_token.cancelled() => {
                    debug!("deletion coordinator cancelled");
                    break;
                }
                _ = interval.tick() => {
                    if let Err(err) = self.execute_pending_deletions().await {
                        warn!(error = %err, "deletion coordinator sweep failed");
                    }
                }
            }
        }
    }

    /// Execute all actionable deletion jobs (Pending, Running, RetryableFailed).
    pub(crate) async fn execute_pending_deletions(&self) -> Result<()> {
        let jobs = self.runtime_db().agent_deletions().actionable_jobs()?;
        for job in jobs {
            if let Err(err) = self.execute_deletion_job(job).await {
                warn!(
                    agent_id = %err,
                    "deletion job execution failed"
                );
            }
        }
        Ok(())
    }

    /// Drive a single deletion job through its remaining phases.
    pub(crate) async fn execute_deletion_job(&self, mut job: AgentDeletionJob) -> Result<()> {
        let agent_id = job.agent_id.clone();
        info!(
            agent_id = %agent_id,
            deletion_id = %job.deletion_id,
            phase = ?job.phase,
            status = ?job.status,
            "executing deletion job"
        );

        // Mark as Running if currently Pending or RetryableFailed.
        if job.status != AgentDeletionStatus::Running {
            job.status = AgentDeletionStatus::Running;
            job.attempts = job.attempts.saturating_add(1);
            job.last_error = None;
            job.updated_at = Utc::now();
            self.runtime_db().agent_deletions().update(&job)?;
        }

        // Execute each phase from the current one onward.
        let phases = AgentDeletionPhase::ALL;
        let start_idx = phases.iter().position(|p| *p == job.phase).unwrap_or(0);

        for phase in &phases[start_idx..] {
            // Skip if we've already advanced past this phase.
            if job.phase > *phase {
                continue;
            }

            let result = self.execute_deletion_phase(&agent_id, *phase, &job).await;
            match result {
                Ok(()) => {
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
                    job.updated_at = Utc::now();
                    self.runtime_db().agent_deletions().update(&job)?;
                    return Err(anyhow!(
                        "deletion job for {agent_id} failed at phase {phase:?}: {err}"
                    ));
                }
            }
        }

        // All phases completed.
        job.status = AgentDeletionStatus::Completed;
        job.completed_at = Some(Utc::now());
        job.updated_at = Utc::now();
        self.runtime_db().agent_deletions().update(&job)?;

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
            AgentDeletionPhase::Finalize => self.deletion_phase_finalize(agent_id).await,
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

        let storage = match self.agent_storage(agent_id) {
            Ok(s) => s,
            Err(_) => {
                // Agent data dir already gone; nothing to quiesce.
                debug!(
                    agent_id,
                    "agent storage unavailable during quiesce; skipping"
                );
                return Ok(());
            }
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

        // Emit audit event via storage if available.
        let _ = storage.append_event(&AuditEvent::legacy(
            "deletion_quiesce",
            serde_json::json!({
                "agent_id": agent_id,
                "tasks_cancelled": tasks_count,
                "waits_cancelled": waits_count,
            }),
        ));

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

    /// Scheduler: ensure no scheduler state references this agent.
    ///
    /// Since the identity fence prevents new dispatches and the runtime is
    /// unloaded, this phase verifies there are no lingering activations.
    /// The per-agent scheduler state lives in the agent data directory and
    /// will be removed in the Home phase.
    async fn deletion_phase_scheduler(&self, agent_id: &str) -> Result<()> {
        // The scheduler protocol snapshot is per-agent and stored in the
        // agent's own data directory. Since the runtime is unloaded and the
        // identity fence prevents new activations, no shared scheduler state
        // needs cleanup here. This phase exists as an explicit checkpoint for
        // future shared scheduler extensions.
        debug!(agent_id, "scheduler phase: no shared state to terminalize");
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
        // The memory index is a shared SQLite database. Remove all rows
        // belonging to this agent.
        let storage = match self.agent_storage(agent_id) {
            Ok(s) => s,
            Err(_) => {
                debug!(
                    agent_id,
                    "agent storage unavailable during index cleanup; skipping"
                );
                return Ok(());
            }
        };
        let index_path = crate::memory::index::memory_index_path(&storage);
        if !index_path.exists() {
            return Ok(());
        }
        let connection = rusqlite::Connection::open(&index_path)
            .with_context(|| format!("opening memory index at {}", index_path.display()))?;
        let tx = connection.unchecked_transaction()?;
        // Delete FTS entries first (references memory_documents).
        let _ = tx.execute(
            "DELETE FROM memory_documents_fts
             WHERE document_key IN (
                SELECT document_key FROM memory_documents WHERE agent_id = ?1
             )",
            [agent_id],
        );
        tx.execute(
            "DELETE FROM memory_documents WHERE agent_id = ?1",
            [agent_id],
        )?;
        tx.execute(
            "DELETE FROM memory_index_source_state WHERE agent_id = ?1",
            [agent_id],
        )?;
        let _ = tx.execute(
            "DELETE FROM memory_index_pending_sources WHERE agent_id = ?1",
            [agent_id],
        );
        let _ = tx.execute(
            "DELETE FROM memory_index_checkpoints WHERE agent_id = ?1",
            [agent_id],
        );
        let _ = tx.execute(
            "DELETE FROM memory_index_meta WHERE agent_id = ?1",
            [agent_id],
        );
        let _ = tx.execute(
            "DELETE FROM memory_index_cursors WHERE agent_id = ?1",
            [agent_id],
        );
        tx.commit()?;
        debug!(agent_id, "removed agent from memory index");
        Ok(())
    }

    /// Home: rename agent home to trash then delete.
    async fn deletion_phase_home(&self, agent_id: &str) -> Result<()> {
        let data_dir = self.agent_data_dir(agent_id);
        if !data_dir.exists() {
            return Ok(());
        }
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
    async fn deletion_phase_finalize(&self, agent_id: &str) -> Result<()> {
        let mut identity = self
            .agent_identity_record(agent_id)?
            .ok_or_else(|| anyhow!("agent {agent_id} identity not found during finalize"))?;
        if identity.status != AgentRegistryStatus::Deleted {
            identity.status = AgentRegistryStatus::Deleted;
            identity.deleted_at = Some(Utc::now());
            identity.updated_at = Utc::now();
            identity.revision = identity.revision.saturating_add(1);
            self.append_agent_identity(&identity)?;
        }
        info!(agent_id, "agent identity finalized as Deleted");
        Ok(())
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
                    && id.status == AgentRegistryStatus::Active
            })
            .collect();

        for child in children {
            let child_id = &child.agent_id;
            // Create a deletion job for the child if one doesn't exist.
            let existing = self
                .runtime_db()
                .agent_deletions()
                .latest_for_agent(child_id)?;
            let child_job = match existing {
                Some(job) if job.status == AgentDeletionStatus::Completed => continue,
                Some(job) => job,
                None => {
                    let (updated_identity, job, _) = self.runtime_db().agent_deletions().begin(
                        child_id,
                        child.revision,
                        &parent_job.requested_by,
                        false, // Don't recurse further
                    )?;
                    self.append_agent_identity(&updated_identity)?;
                    job
                }
            };
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
