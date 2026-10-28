//! One cleanup policy, with explicit runtime and frozen Host execution adapters.
use super::*;
use crate::runtime::{
    input_evidence_budget::{
        serialized_bytes, BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory,
        OwnedBytesEstimate,
    },
    resident_input_evidence::InputEvidenceScope,
};
use crate::runtime_db::task_snapshot::TaskDomainLoadRequest;
use crate::system::{EffectiveExecution, LocalSystem};

pub(crate) struct FrozenWorktreeCleanup {
    system: LocalSystem,
    execution: EffectiveExecution,
    repo_root: PathBuf,
    scope: InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    byte_limit: usize,
    record_limit: usize,
    bytes: usize,
    records: usize,
    audits: Vec<AuditEvent>,
    guards: Vec<Arc<BudgetViewGuard>>,
    completion: Option<Arc<crate::runtime::input_evidence_budget::OwnedCompletionBudget>>,
}
pub(in crate::runtime) struct ObservedOriginalControlCleanup {
    pub(in crate::runtime) cleanup: TaskOwnedWorktreeCleanup,
    pub(in crate::runtime) audits: Vec<AuditEvent>,
}
pub(crate) struct ObservedWorktreeCleanup {
    pub(crate) status: TaskOwnedWorktreeCleanupStatus,
    pub(crate) audits: Vec<AuditEvent>,
    pub(crate) guards: Vec<Arc<BudgetViewGuard>>,
}

pub(super) fn original_task_deletion_fenced(
    runtime: &RuntimeHandle,
    task_id: &str,
    request: &TaskDomainLoadRequest,
) -> Result<bool> {
    use rusqlite::OptionalExtension;
    let scope = InputEvidenceScope {
        agent_id: runtime.inner.mutation_owner.agent_id().to_owned(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    runtime.inner.runtime_db.transaction(|tx| {
        let bytes: usize = tx.query_row(
            "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM tasks WHERE task_id=?1 AND owner_agent_id=?2),0)",
            rusqlite::params![task_id, scope.agent_id],
            |row| row.get(0),
        )?;
        let task_bytes = bytes
            .checked_add(task_id.len())
            .and_then(|n| n.checked_add(scope.agent_id.len()))
            .and_then(|n| n.checked_add(256))
            .and_then(|n| n.checked_mul(8))
            .context("worktree deletion Task material overflow")?;
        let _task_material = crate::runtime_db::command_task_terminal::charge_material(
            &scope,
            task_bytes,
            8,
            request,
            &runtime.inner.input_evidence_budget,
        )?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM tasks WHERE task_id=?1 AND owner_agent_id=?2",
                rusqlite::params![task_id, scope.agent_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(false);
        };
        let task = crate::runtime_db::repositories::decode_task_payload(&raw)?;
        anyhow::ensure!(
            task.id == task_id && task.agent_id == scope.agent_id,
            "worktree deletion Task projection mismatch"
        );
        if task.detail.as_ref().is_some_and(|detail| {
            detail["worktree"]["cleanup_reason"].as_str() == Some("agent_deleted")
        }) {
            return Ok(true);
        }
        let Some(target) = task.target() else {
            return Ok(false);
        };
        let bytes: usize = tx.query_row(
            "SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",
            [&target.agent_id],
            |row| row.get(0),
        )?;
        let target_bytes = bytes
            .checked_add(target.agent_id.len())
            .and_then(|n| n.checked_add(256))
            .and_then(|n| n.checked_mul(8))
            .context("worktree deletion identity material overflow")?;
        let bytes = task_bytes
            .checked_add(target_bytes)
            .context("worktree deletion material overflow")?;
        let (records, limit) = TaskDomainLoadRequest::bounded_material_limits(
            request.record_limit,
            request.byte_limit,
        );
        anyhow::ensure!(
            bytes <= limit && records >= 16,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "worktree deletion evidence"
            )
        );
        let _target_material = crate::runtime_db::command_task_terminal::charge_material(
            &scope,
            target_bytes,
            8,
            request,
            &runtime.inner.input_evidence_budget,
        )?;
        crate::runtime_db::repositories::task_target_is_deleted(tx, &target)
    })
}

pub(super) fn retain_deleted_target_metadata(
    task_id: &str,
    worktree: &mut serde_json::Value,
) -> Result<(TaskOwnedWorktreeCleanup, AuditEvent)> {
    anyhow::ensure!(worktree.is_object(), "invalid task-owned worktree metadata");
    worktree["cleanup_status"] =
        serde_json::json!(TaskOwnedWorktreeCleanupStatus::Retained.label());
    worktree["cleanup_reason"] = serde_json::json!("agent_deleted");
    worktree["retained_for_review"] = serde_json::json!(true);
    worktree["cleanup_owner_task_id"] = serde_json::json!(task_id);
    worktree["auto_cleaned_up"] = serde_json::json!(false);
    Ok((
        TaskOwnedWorktreeCleanup {
            changed_files: changed_files_from_metadata(worktree),
            status: TaskOwnedWorktreeCleanupStatus::Retained,
        },
        AuditEvent::legacy(
            "task_worktree_cleanup_retained",
            serde_json::json!({
                "task_id": task_id,
                "worktree": worktree,
                "reason": "agent_deleted",
            }),
        ),
    ))
}

impl FrozenWorktreeCleanup {
    pub(crate) fn new(
        execution: EffectiveExecution,
        repo_root: PathBuf,
        scope: InputEvidenceScope,
        ledger: InputEvidenceBudgetLedger,
        request: &TaskDomainLoadRequest,
    ) -> Result<Self> {
        let mut value = Self {
            system: LocalSystem::new(),
            execution,
            repo_root,
            scope,
            ledger,
            byte_limit: request.byte_limit.min(64 * 1024 * 1024),
            record_limit: request.record_limit.min(65536),
            bytes: 0,
            records: 0,
            audits: Vec::new(),
            guards: Vec::new(),
            completion: None,
        };
        // Paths and the frozen execution are owned across the Git awaits.
        let bytes = value
            .repo_root
            .as_os_str()
            .len()
            .checked_add(serialized_bytes(&value.execution.snapshot())? as usize)
            .context("worktree execution material overflow")?;
        value.retain_material(bytes, 1)?;
        Ok(value)
    }
    fn retain_material(&mut self, bytes: usize, records: usize) -> Result<()> {
        if let Some(completion) = &self.completion {
            completion.observe_material(
                (bytes as u64).saturating_mul(8),
                (records as u64).saturating_mul(128).saturating_mul(8),
            );
            return Ok(());
        }
        let bytes = bytes.checked_mul(8).context("worktree material overflow")?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("worktree material overflow")?;
        self.records = self
            .records
            .checked_add(records.checked_mul(8).context("worktree record overflow")?)
            .context("worktree record overflow")?;
        anyhow::ensure!(
            self.bytes <= self.byte_limit && self.records <= self.record_limit,
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "worktree cleanup material"
            )
        );
        self.guards.push(self.ledger.register_owned_bytes(
            &self.scope,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: u64::try_from(bytes)?,
                metadata_estimate_bytes: 0,
            },
        )?);
        Ok(())
    }
    async fn git(
        &mut self,
        cwd: &Path,
        args: &[&str],
        context: &str,
        purpose: ProcessPurpose,
    ) -> Result<String> {
        let output = self
            .system
            .run(
                &self.execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: args.iter().map(OsString::from).collect(),
                    },
                    cwd: Some(cwd.to_path_buf()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose,
                },
            )
            .await
            .with_context(|| context.to_owned())?;
        self.retain_material(
            output
                .stdout
                .len()
                .checked_add(output.stderr.len())
                .context("worktree Git output overflow")?,
            1,
        )?;
        if !output.exit_status.success() {
            anyhow::bail!(
                "{context}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}
pub(crate) async fn cleanup_frozen_worktree(
    mut frozen: FrozenWorktreeCleanup,
    task_id: &str,
    metadata: &mut serde_json::Value,
    reason: &str,
) -> Result<ObservedWorktreeCleanup> {
    let result = cleanup_metadata(
        &mut CleanupAdapter::Frozen(&mut frozen),
        task_id,
        metadata,
        reason,
    )
    .await?;
    Ok(ObservedWorktreeCleanup {
        status: result.status,
        audits: frozen.audits,
        guards: frozen.guards,
    })
}
/// The original Control owns this pre-registered output reservation. Reuse the
/// same cleanup policy/Git adapter; keep its historical audit behavior intact.
pub(super) async fn cleanup_original_control_metadata(
    runtime: &RuntimeHandle,
    task_id: &str,
    metadata: &mut serde_json::Value,
    reason: &str,
    completion: Arc<crate::runtime::input_evidence_budget::OwnedCompletionBudget>,
    request: &TaskDomainLoadRequest,
) -> Result<ObservedOriginalControlCleanup> {
    let execution = runtime
        .effective_execution(ExecutionScopeKind::AgentTurn)
        .await?;
    let scope = InputEvidenceScope {
        agent_id: runtime.inner.mutation_owner.agent_id().to_owned(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    // Construction (including every fallible registration) precedes Git cleanup.
    let mut frozen = FrozenWorktreeCleanup::new(
        execution,
        runtime.workspace_root(),
        scope,
        runtime.inner.input_evidence_budget.clone(),
        request,
    )?;
    frozen.completion = Some(completion);
    let cleanup = cleanup_metadata(
        &mut CleanupAdapter::Completion(&mut frozen),
        task_id,
        metadata,
        reason,
    )
    .await?;
    Ok(ObservedOriginalControlCleanup {
        cleanup,
        audits: frozen.audits,
    })
}
#[cfg(test)]
pub(crate) async fn cleanup_completion_worktree_for_test(
    mut frozen: FrozenWorktreeCleanup,
    completion: Arc<crate::runtime::input_evidence_budget::OwnedCompletionBudget>,
    task_id: &str,
    metadata: &mut serde_json::Value,
) -> Result<(TaskOwnedWorktreeCleanupStatus, Vec<AuditEvent>)> {
    frozen.completion = Some(completion);
    let result = cleanup_metadata(
        &mut CleanupAdapter::Completion(&mut frozen),
        task_id,
        metadata,
        "agent_lifecycle_stop",
    )
    .await?;
    Ok((result.status, frozen.audits))
}

enum CleanupAdapter<'a> {
    Completion(&'a mut FrozenWorktreeCleanup),
    Frozen(&'a mut FrozenWorktreeCleanup),
}
impl CleanupAdapter<'_> {
    fn audit(&mut self, event: &AuditEvent) -> Result<()> {
        match self {
            Self::Completion(frozen) => {
                // This reservation predates FS. Audit persistence belongs to the
                // saved completion capsule, so a later SQL failure cannot redo Git.
                let bytes = serialized_bytes(event).unwrap_or(u64::MAX);
                frozen
                    .completion
                    .as_ref()
                    .expect("completion adapter reservation")
                    .observe_material(bytes.saturating_mul(8), 1024);
                frozen.audits.push(event.clone());
                Ok(())
            }
            Self::Frozen(frozen) => {
                frozen.retain_material(usize::try_from(serialized_bytes(event)?)?, 1)?;
                frozen.audits.push(event.clone());
                Ok(())
            }
        }
    }
    async fn current_branch(&mut self, path: &Path) -> Result<String> {
        match self {
            Self::Frozen(frozen) | Self::Completion(frozen) => {
                frozen
                    .git(
                        path,
                        &["rev-parse", "--abbrev-ref", "HEAD"],
                        "failed to inspect worktree branch",
                        ProcessPurpose::InternalGit,
                    )
                    .await
            }
        }
    }
    async fn changed_files(&mut self, path: &Path) -> Result<Vec<String>> {
        match self {
            Self::Frozen(frozen) | Self::Completion(frozen) => {
                let status = frozen
                    .git(
                        path,
                        &["status", "--porcelain"],
                        "failed to inspect worktree status",
                        ProcessPurpose::InternalGit,
                    )
                    .await?;
                let count = status
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count();
                frozen.retain_material(
                    status
                        .len()
                        .checked_add(
                            count
                                .checked_mul(std::mem::size_of::<String>())
                                .context("worktree paths slots overflow")?,
                        )
                        .context("worktree path material overflow")?,
                    count,
                )?;
                let mut paths = status
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| {
                        let trimmed = line.trim();
                        trimmed
                            .get(3..)
                            .filter(|path| !path.trim().is_empty())
                            .unwrap_or(trimmed)
                            .to_owned()
                    })
                    .collect::<Vec<_>>();
                paths.sort();
                Ok(paths)
            }
        }
    }
    async fn remove(
        &mut self,
        artifact: &TaskOwnedWorktreeArtifact,
    ) -> Result<TaskOwnedWorktreeRemoveOutcome> {
        match self {
            Self::Frozen(frozen) | Self::Completion(frozen) => {
                let repo = frozen.repo_root.clone();
                let path = artifact
                    .worktree_path
                    .to_str()
                    .context("worktree path is not UTF-8")?;
                frozen
                    .git(
                        &repo,
                        &["worktree", "remove", path],
                        "git worktree remove failed",
                        ProcessPurpose::WorktreeSetup,
                    )
                    .await?;
                let branch_ref = format!("refs/heads/{}", artifact.worktree_branch);
                // show-ref absence is the original AlreadyRemoved branch outcome.
                let output = frozen
                    .system
                    .run(
                        &frozen.execution,
                        ProcessRequest {
                            program: ProgramInvocation::Argv {
                                program: "git".into(),
                                args: ["show-ref", "--verify", "--quiet", &branch_ref]
                                    .into_iter()
                                    .map(OsString::from)
                                    .collect(),
                            },
                            cwd: Some(repo.clone()),
                            env: vec![],
                            stdin: StdioSpec::Null,
                            tty: false,
                            capture: CaptureSpec::BOTH,
                            timeout: None,
                            purpose: ProcessPurpose::InternalGit,
                        },
                    )
                    .await
                    .context("failed to inspect worktree branch ref")?;
                frozen.retain_material(
                    output
                        .stdout
                        .len()
                        .checked_add(output.stderr.len())
                        .context("worktree ref material overflow")?,
                    1,
                )?;
                if !output.exit_status.success() {
                    return Ok(TaskOwnedWorktreeRemoveOutcome {
                        branch_deleted: false,
                        branch_delete_error: None,
                    });
                }
                match frozen
                    .git(
                        &repo,
                        &["branch", "-D", &artifact.worktree_branch],
                        "git branch -D failed",
                        ProcessPurpose::InternalGit,
                    )
                    .await
                {
                    Ok(_) => Ok(TaskOwnedWorktreeRemoveOutcome {
                        branch_deleted: true,
                        branch_delete_error: None,
                    }),
                    Err(error) => Ok(TaskOwnedWorktreeRemoveOutcome {
                        branch_deleted: false,
                        branch_delete_error: Some(error.to_string()),
                    }),
                }
            }
        }
    }
}

async fn cleanup_metadata(
    adapter: &mut CleanupAdapter<'_>,
    task_id: &str,
    worktree: &mut serde_json::Value,
    reason: &str,
) -> Result<TaskOwnedWorktreeCleanup> {
    // Only deletion evidence overrides the normal clean/dirty cleanup policy.
    if reason == "agent_deleted" || worktree["cleanup_reason"].as_str() == Some("agent_deleted") {
        let (cleanup, audit) = retain_deleted_target_metadata(task_id, worktree)?;
        adapter.audit(&audit)?;
        return Ok(cleanup);
    }
    let artifact = match parse_task_owned_worktree_artifact(worktree) {
        Ok(artifact) => artifact,
        Err(err) => {
            let error = err.to_string();
            worktree["cleanup_status"] =
                serde_json::json!(TaskOwnedWorktreeCleanupStatus::Failed.label());
            worktree["cleanup_error"] = serde_json::json!(error);
            adapter.audit(&AuditEvent::legacy(
                "task_worktree_cleanup_failed",
                serde_json::json!({
                    "task_id": task_id,
                    "reason": reason,
                    "error": worktree["cleanup_error"],
                }),
            ))?;
            return Ok(TaskOwnedWorktreeCleanup {
                changed_files: Vec::new(),
                status: TaskOwnedWorktreeCleanupStatus::Failed,
            });
        }
    };

    if !artifact.worktree_path.exists() {
        worktree["cleanup_status"] =
            serde_json::json!(TaskOwnedWorktreeCleanupStatus::AlreadyRemoved.label());
        worktree["cleanup_reason"] = serde_json::json!("worktree_path_missing");
        worktree["auto_cleaned_up"] = serde_json::json!(true);
        adapter.audit(&AuditEvent::legacy(
            "task_worktree_cleanup_already_removed",
            serde_json::json!({
                "task_id": task_id,
                "worktree_path": artifact.worktree_path,
                "worktree_branch": artifact.worktree_branch,
                "reason": reason,
            }),
        ))?;
        return Ok(TaskOwnedWorktreeCleanup {
            changed_files: Vec::new(),
            status: TaskOwnedWorktreeCleanupStatus::AlreadyRemoved,
        });
    }

    let actual_branch = adapter.current_branch(&artifact.worktree_path).await;
    match actual_branch {
        Ok(actual_branch) if actual_branch != artifact.worktree_branch => {
            worktree["cleanup_status"] =
                serde_json::json!(TaskOwnedWorktreeCleanupStatus::Retained.label());
            worktree["cleanup_reason"] = serde_json::json!("branch_mismatch");
            worktree["actual_branch"] = serde_json::json!(actual_branch);
            adapter.audit(&AuditEvent::legacy(
                "task_worktree_cleanup_retained",
                serde_json::json!({
                    "task_id": task_id,
                    "worktree_path": artifact.worktree_path,
                    "worktree_branch": artifact.worktree_branch,
                    "actual_branch": actual_branch,
                    "reason": "branch_mismatch",
                    "cleanup_trigger": reason,
                }),
            ))?;
            return Ok(TaskOwnedWorktreeCleanup {
                changed_files: changed_files_from_metadata(worktree),
                status: TaskOwnedWorktreeCleanupStatus::Retained,
            });
        }
        Err(err) => {
            let error = err.to_string();
            worktree["cleanup_status"] =
                serde_json::json!(TaskOwnedWorktreeCleanupStatus::Retained.label());
            worktree["cleanup_reason"] = serde_json::json!("branch_inspection_failed");
            worktree["cleanup_error"] = serde_json::json!(error);
            adapter.audit(&AuditEvent::legacy(
                "task_worktree_cleanup_retained",
                serde_json::json!({
                    "task_id": task_id,
                    "worktree_path": artifact.worktree_path,
                    "worktree_branch": artifact.worktree_branch,
                    "reason": "branch_inspection_failed",
                    "cleanup_trigger": reason,
                    "error": worktree["cleanup_error"],
                }),
            ))?;
            return Ok(TaskOwnedWorktreeCleanup {
                changed_files: changed_files_from_metadata(worktree),
                status: TaskOwnedWorktreeCleanupStatus::Retained,
            });
        }
        _ => {}
    }

    let changed_files = match adapter.changed_files(&artifact.worktree_path).await {
        Ok(changed_files) => changed_files,
        Err(err) => {
            let error = err.to_string();
            worktree["cleanup_status"] =
                serde_json::json!(TaskOwnedWorktreeCleanupStatus::Retained.label());
            worktree["cleanup_reason"] = serde_json::json!("change_detection_failed");
            worktree["cleanup_error"] = serde_json::json!(error);
            adapter.audit(&AuditEvent::legacy(
                "task_worktree_cleanup_retained",
                serde_json::json!({
                    "task_id": task_id,
                    "worktree_path": artifact.worktree_path,
                    "worktree_branch": artifact.worktree_branch,
                    "reason": "change_detection_failed",
                    "cleanup_trigger": reason,
                    "error": worktree["cleanup_error"],
                }),
            ))?;
            return Ok(TaskOwnedWorktreeCleanup {
                changed_files: changed_files_from_metadata(worktree),
                status: TaskOwnedWorktreeCleanupStatus::Retained,
            });
        }
    };
    worktree["changed_files"] = serde_json::json!(changed_files);

    if !changed_files.is_empty() {
        worktree["cleanup_status"] =
            serde_json::json!(TaskOwnedWorktreeCleanupStatus::Retained.label());
        worktree["cleanup_reason"] = serde_json::json!("changed_files");
        worktree["retained_for_review"] = serde_json::json!(true);
        adapter.audit(&AuditEvent::legacy(
            "worktree_retained_for_review",
            serde_json::json!({
                "task_id": task_id,
                "worktree_path": artifact.worktree_path,
                "worktree_branch": artifact.worktree_branch,
                "changed_files": changed_files,
                "reason": "changes detected in worktree",
                "cleanup_trigger": reason,
            }),
        ))?;
        return Ok(TaskOwnedWorktreeCleanup {
            changed_files,
            status: TaskOwnedWorktreeCleanupStatus::Retained,
        });
    }

    match adapter.remove(&artifact).await {
        Ok(remove_outcome) => {
            worktree["cleanup_status"] =
                serde_json::json!(TaskOwnedWorktreeCleanupStatus::Cleaned.label());
            worktree["cleanup_reason"] = serde_json::json!("clean_worktree");
            worktree["auto_cleaned_up"] = serde_json::json!(true);
            worktree["branch_cleanup_status"] =
                serde_json::json!(if remove_outcome.branch_deleted {
                    "deleted"
                } else if remove_outcome.branch_delete_error.is_some() {
                    "retained"
                } else {
                    "already_removed"
                });
            if let Some(error) = remove_outcome.branch_delete_error.as_ref() {
                worktree["branch_cleanup_error"] = serde_json::json!(error);
                adapter.audit(&AuditEvent::legacy(
                    "task_worktree_branch_cleanup_retained",
                    serde_json::json!({
                        "task_id": task_id,
                        "worktree_path": artifact.worktree_path,
                        "worktree_branch": artifact.worktree_branch,
                        "reason": reason,
                        "error": error,
                    }),
                ))?;
            }
            adapter.audit(&AuditEvent::legacy(
                "worktree_auto_cleaned_up",
                serde_json::json!({
                    "task_id": task_id,
                    "worktree_path": artifact.worktree_path,
                    "worktree_branch": artifact.worktree_branch,
                    "reason": reason,
                    "branch_cleanup_status": worktree["branch_cleanup_status"],
                }),
            ))?;
            Ok(TaskOwnedWorktreeCleanup {
                changed_files,
                status: TaskOwnedWorktreeCleanupStatus::Cleaned,
            })
        }
        Err(err) if !artifact.worktree_path.exists() => {
            worktree["cleanup_status"] =
                serde_json::json!(TaskOwnedWorktreeCleanupStatus::AlreadyRemoved.label());
            worktree["cleanup_reason"] = serde_json::json!("worktree_path_missing_after_remove");
            worktree["auto_cleaned_up"] = serde_json::json!(true);
            adapter.audit(&AuditEvent::legacy(
                "task_worktree_cleanup_already_removed",
                serde_json::json!({
                    "task_id": task_id,
                    "worktree_path": artifact.worktree_path,
                    "worktree_branch": artifact.worktree_branch,
                    "reason": reason,
                    "remove_error": err.to_string(),
                }),
            ))?;
            Ok(TaskOwnedWorktreeCleanup {
                changed_files,
                status: TaskOwnedWorktreeCleanupStatus::AlreadyRemoved,
            })
        }
        Err(err) => {
            let error = err.to_string();
            worktree["cleanup_status"] =
                serde_json::json!(TaskOwnedWorktreeCleanupStatus::Failed.label());
            worktree["cleanup_error"] = serde_json::json!(error);
            adapter.audit(&AuditEvent::legacy(
                "worktree_auto_cleanup_failed",
                serde_json::json!({
                    "task_id": task_id,
                    "worktree_path": artifact.worktree_path,
                    "worktree_branch": artifact.worktree_branch,
                    "reason": reason,
                    "error": worktree["cleanup_error"],
                }),
            ))?;
            Ok(TaskOwnedWorktreeCleanup {
                changed_files,
                status: TaskOwnedWorktreeCleanupStatus::Failed,
            })
        }
    }
}

#[cfg(test)]
#[path = "worktree_cleanup_tests.rs"]
mod tests;
