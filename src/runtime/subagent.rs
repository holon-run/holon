use super::*;
use std::ffi::OsString;

use crate::provider::{ConversationMessage, ProviderTurnRequest};
use crate::system::{
    CaptureSpec, ExecutionScopeKind, FileHost, ProcessHost, ProcessPurpose, ProcessRequest,
    ProgramInvocation, StdioSpec,
};

impl RuntimeHandle {
    pub(crate) async fn prepare_managed_worktree_for_task(
        &self,
        task_id: &str,
    ) -> Result<ManagedWorktreeSeed> {
        self.prepare_managed_worktree_for_task_inner(task_id, None)
            .await
    }

    pub(crate) async fn prepare_managed_worktree_for_original_child_spawn(
        &self,
        task_id: &str,
        source: &super::OriginalChildTaskProducer,
    ) -> Result<ManagedWorktreeSeed> {
        anyhow::ensure!(
            source.address().scope.agent_id == self.instance_agent_id()
                && source.address().scope.identity_incarnation == self.instance_incarnation()
                && source.address().admitted.key.id == task_id
                && source.original_child_spawn_registered(),
            "original worktree spawn differs from its admitted source"
        );
        self.prepare_managed_worktree_for_task_inner(task_id, Some(source))
            .await
    }

    async fn prepare_managed_worktree_for_task_inner(
        &self,
        task_id: &str,
        original_spawn: Option<&super::OriginalChildTaskProducer>,
    ) -> Result<ManagedWorktreeSeed> {
        let state = self.agent_state().await?;
        if !state.execution_profile.supports_managed_worktrees {
            return Err(anyhow!(
                "managed worktrees are disabled by the current execution profile"
            ));
        }
        let original_cwd = self.workspace_root();
        let system = self.system();
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        let original_branch_output = system
            .run(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: vec![
                            OsString::from("rev-parse"),
                            OsString::from("--abbrev-ref"),
                            OsString::from("HEAD"),
                        ],
                    },
                    cwd: Some(original_cwd.clone()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::InternalGit,
                },
            )
            .await
            .context("failed to determine current git branch")?;
        if !original_branch_output.exit_status.success() {
            let stderr = String::from_utf8_lossy(&original_branch_output.stderr)
                .trim()
                .to_string();
            return Err(anyhow!(
                "failed to determine current git branch for worktree task: {stderr}"
            ));
        }
        let original_branch = String::from_utf8_lossy(&original_branch_output.stdout)
            .trim()
            .to_string();
        if original_branch == "HEAD" {
            return Err(anyhow!(
                "detached HEAD is not supported for worktree subagent tasks"
            ));
        }

        let repo_name = original_cwd
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repo");
        let managed_root = original_cwd
            .parent()
            .unwrap_or(original_cwd.as_path())
            .join(format!(".holon-worktrees-{repo_name}"));
        system
            .create_dir_all(&execution, &managed_root)
            .await
            .context("failed to create managed worktree directory")?;

        let worktree_branch = format!("task-{task_id}");
        let worktree_path = managed_root.join(&worktree_branch);
        if worktree_path.exists() {
            return Err(anyhow!(
                "managed worktree path already exists: {}",
                worktree_path.display()
            ));
        }

        if let Some(source) = original_spawn {
            source.original_spawn_worktree_planned(
                &original_cwd,
                &original_branch,
                &worktree_path,
                &worktree_branch,
            )?;
        }
        let worktree_output = system
            .run(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: vec![
                            OsString::from("worktree"),
                            OsString::from("add"),
                            OsString::from("-b"),
                            OsString::from(&worktree_branch),
                            worktree_path.as_os_str().to_os_string(),
                        ],
                    },
                    cwd: Some(original_cwd.clone()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::WorktreeSetup,
                },
            )
            .await
            .context("failed to create git worktree")?;
        if !worktree_output.exit_status.success() {
            let stderr = String::from_utf8_lossy(&worktree_output.stderr)
                .trim()
                .to_string();
            return Err(anyhow!("git worktree add failed: {stderr}"));
        }

        // The actual Git ACK is saved before the original fallible audit.
        if let Some(source) = original_spawn {
            source.original_spawn_worktree_created(&worktree_path, &worktree_branch)?;
        }
        self.inner.storage.append_event(&AuditEvent::legacy(
            "worktree_created_for_task",
            serde_json::json!({
                "task_id": task_id,
                "worktree_path": worktree_path,
                "worktree_branch": worktree_branch,
                "original_cwd": original_cwd,
                "original_branch": original_branch,
            }),
        ))?;

        let seed = ManagedWorktreeSeed {
            original_cwd,
            original_branch,
            worktree_path,
            worktree_branch,
        };
        self.record_task_owned_worktree_metadata(task_id, &seed)
            .await?;
        Ok(seed)
    }

    pub(super) async fn run_subagent_prompt(
        &self,
        agent_id: &str,
        prompt: &str,
        authority_class: &AuthorityClass,
    ) -> Result<String> {
        let execution = self
            .effective_execution(ExecutionScopeKind::SubagentTask)
            .await?;
        self.run_subagent_prompt_for_workspace(
            agent_id,
            prompt,
            authority_class,
            &execution,
            serde_json::json!({
                "prompt": prompt,
                "authority_class": authority_class,
                "execution_root": execution.workspace.execution_root(),
            }),
            None,
        )
        .await
    }

    pub(super) async fn run_subagent_prompt_in_dedicated_worktree(
        &self,
        agent_id: &str,
        prompt: &str,
        authority_class: &AuthorityClass,
        task_id: &str,
    ) -> Result<WorktreeSubagentResult> {
        let seed = self.prepare_managed_worktree_for_task(task_id).await?;
        self.emit_worktree_child_task_running_status(agent_id, task_id, authority_class)
            .await?;

        let worktree_execution = self
            .effective_execution_for_workspace(
                ExecutionScopeKind::WorktreeSubagentTask,
                self.workspace_view_for_root(
                    seed.worktree_path.clone(),
                    seed.worktree_path.clone(),
                    Some(seed.worktree_path.clone()),
                )?,
            )
            .await?;

        let prompt_result = self
            .run_subagent_prompt_for_workspace(
                agent_id,
                prompt,
                authority_class,
                &worktree_execution,
                serde_json::json!({
                    "prompt": prompt,
                    "authority_class": authority_class,
                    "task_id": task_id,
                    "workspace_root": seed.worktree_path,
                }),
                Some(serde_json::json!({
                    "task_id": task_id,
                    "workspace_root": seed.worktree_path,
                })),
            )
            .await;

        let changed_files = self
            .detect_changed_files(&seed.worktree_path)
            .await
            .unwrap_or_default();

        let (text, failed) = match prompt_result {
            Ok(text) => (text, false),
            Err(err) => (format!("worktree subagent failed: {err:#}"), true),
        };

        Ok(WorktreeSubagentResult {
            text,
            worktree_path: seed.worktree_path,
            worktree_branch: seed.worktree_branch,
            changed_files,
            failed,
        })
    }

    async fn emit_worktree_child_task_running_status(
        &self,
        agent_id: &str,
        task_id: &str,
        authority_class: &AuthorityClass,
    ) -> Result<()> {
        let mut mutation = self.begin_mutation().await?;
        let scope = mutation.input_evidence_scope();
        let limits = self.inner.input_evidence_budget.limits();
        let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
            query: Default::default(),
            capture: super::resident_task::TaskCaptureRequest::current(),
            record_limit: usize::try_from(limits.record_limit)?,
            byte_limit: usize::try_from(
                limits
                    .history_per_agent_bytes
                    .checked_add(limits.pending_per_agent_bytes)
                    .context("child Running limits overflow")?,
            )?,
        };
        let _selector = crate::runtime_db::command_task_terminal::charge_material(
            &scope,
            task_id
                .len()
                .checked_add(256)
                .and_then(|n| n.checked_mul(8))
                .context("child Running selector overflow")?,
            8,
            &request,
            &self.inner.input_evidence_budget,
        )?;
        let mut capture = super::resident_task::TaskCaptureRequest::current();
        capture
            .required_keys
            .insert(super::resident_task::TaskKey::new(
                super::resident_task::TaskFamily::Task,
                task_id,
            ));
        mutation.prepare_owner_task_request(&capture).await?;
        let selected = mutation.capture_tasks(&capture)?;
        let fact = selected
            .lookup(&super::resident_task::TaskKey::new(
                super::resident_task::TaskFamily::Task,
                task_id,
            ))?
            .context("child Running producer Task missing")?;
        let super::resident_task::CanonicalTaskFact::Task(task) = &fact.fact else {
            anyhow::bail!("child Running producer selected another family")
        };
        let _message_material = crate::runtime_db::command_task_terminal::charge_material(
            &scope,
            usize::try_from(super::input_evidence_budget::serialized_bytes(task)?)?
                .checked_add(1024)
                .and_then(|n| n.checked_mul(8))
                .context("child Running Message material overflow")?,
            super::child_task_terminal::completion_value_nodes(task.detail.as_ref())?
                .checked_add(8)
                .and_then(|n| n.checked_mul(8))
                .context("child Running Message nodes overflow")?,
            &request,
            &self.inner.input_evidence_budget,
        )?;
        let task_summary = task.summary.clone().unwrap_or_default();
        let running_message = MessageEnvelope {
            work_item_id: task.work_item_id.clone(),
            metadata: Some(serde_json::json!({
                "task_id": task.id.clone(),
                "task_created_at": task.created_at,
                "work_item_id": task.work_item_id.clone(),
                "task_kind": task.kind.clone(),
                "task_status": "running",
                "task_summary": task.summary.clone(),
                "task_detail": task.detail.clone(),
                "task_recovery": task.recovery.clone(),
            })),
            ..MessageEnvelope::new(
                agent_id.to_string(),
                MessageKind::TaskStatus,
                MessageOrigin::Task {
                    task_id: task_id.to_string(),
                },
                authority_class.clone(),
                Priority::Background,
                MessageBody::Text {
                    text: format!("worktree child agent task started: {task_summary}"),
                },
            )
            .with_admission(
                MessageDeliverySurface::TaskRejoin,
                AdmissionContext::RuntimeOwned,
            )
        };
        drop(mutation);
        self.enqueue(running_message).await.map(|_| ())
    }

    pub(super) async fn run_subagent_prompt_for_workspace(
        &self,
        agent_id: &str,
        prompt: &str,
        authority_class: &AuthorityClass,
        execution: &crate::system::EffectiveExecution,
        prompt_metadata: serde_json::Value,
        assistant_metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        let effective_prompt = self
            .build_subagent_prompt_for_workspace(agent_id, prompt, authority_class, execution)
            .await?;
        self.persist_transcript_evidence(&TranscriptEntry::new(
            agent_id.to_string(),
            TranscriptEntryKind::SubagentPrompt,
            None,
            None,
            prompt_metadata,
        ))
        .await?;
        let response = self
            .current_provider()
            .await
            .complete_turn(ProviderTurnRequest::plain(
                effective_prompt.rendered_system_prompt,
                vec![ConversationMessage::UserText(
                    effective_prompt.rendered_context_attachment,
                )],
                vec![],
            ))
            .await?;

        {
            let mut guard = self.begin_mutation().await?;
            guard.state.total_input_tokens += response.input_tokens;
            guard.state.total_output_tokens += response.output_tokens;
            guard.state.total_model_rounds += 1;
            guard.state.last_turn_token_usage = Some(crate::types::TokenUsage::new(
                response.input_tokens,
                response.output_tokens,
            ));
            guard.persist_state(&self.inner.storage).await?;
        }

        self.persist_transcript_evidence(&TranscriptEntry {
            stop_reason: response.stop_reason.clone(),
            input_tokens: Some(response.input_tokens),
            output_tokens: Some(response.output_tokens),
            ..TranscriptEntry::new(
                agent_id.to_string(),
                TranscriptEntryKind::SubagentAssistantRound,
                Some(1),
                None,
                serde_json::json!({
                    "blocks": response.blocks,
                    "metadata": assistant_metadata,
                }),
            )
        })
        .await?;

        let text = sanitize_subagent_result(
            &response
                .blocks
                .into_iter()
                .filter_map(|block| match block {
                    ModelBlock::Text { text } => Some(text),
                    ModelBlock::ToolUse { .. } => None,
                    ModelBlock::Thinking { .. }
                    | ModelBlock::ReasoningText { .. }
                    | ModelBlock::RedactedThinking { .. }
                    | ModelBlock::Citations { .. }
                    | ModelBlock::ProviderToolUse { .. }
                    | ModelBlock::ProviderToolResult { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
        if text.trim().is_empty() {
            return Ok("subagent completed without textual output".into());
        }
        Ok(text)
    }

    pub(crate) async fn detect_changed_files(&self, worktree_path: &Path) -> Result<Vec<String>> {
        let execution = self
            .effective_execution_for_workspace(
                ExecutionScopeKind::WorktreeSubagentTask,
                self.workspace_view_for_root(
                    worktree_path.to_path_buf(),
                    worktree_path.to_path_buf(),
                    Some(worktree_path.to_path_buf()),
                )?,
            )
            .await?;
        let status_output = self
            .system()
            .run(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Argv {
                        program: "git".into(),
                        args: vec![OsString::from("status"), OsString::from("--porcelain")],
                    },
                    cwd: Some(worktree_path.to_path_buf()),
                    env: vec![],
                    stdin: StdioSpec::Null,
                    tty: false,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::InternalGit,
                },
            )
            .await?;

        if status_output.exit_status.success() {
            let output = String::from_utf8_lossy(&status_output.stdout);
            let mut changed_files = output
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| {
                    let parts = line.trim().splitn(2, ' ').collect::<Vec<_>>();
                    if parts.len() > 1 {
                        parts[1].to_string()
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>();

            changed_files.sort();

            Ok(changed_files)
        } else {
            Ok(Vec::new())
        }
    }
}

pub(super) fn sanitize_subagent_result(text: &str) -> String {
    let mut cleaned = strip_tagged_blocks(text, "think");
    cleaned = strip_xml_like_tool_calls(&cleaned);
    cleaned
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("**[SYSTEM]")
                && !trimmed.starts_with("[SYSTEM]")
                && !trimmed.starts_with("(Executing Step")
                && !trimmed.starts_with("**(Executing Step")
        })
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn strip_tagged_blocks(input: &str, tag: &str) -> String {
    let mut remaining = input.to_string();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");

    while let Some(start) = remaining.find(&open) {
        if let Some(end_rel) = remaining[start..].find(&close) {
            let end = start + end_rel + close.len();
            remaining.replace_range(start..end, "");
        } else {
            remaining.replace_range(start..remaining.len(), "");
            break;
        }
    }

    remaining
}

fn strip_xml_like_tool_calls(input: &str) -> String {
    let mut result = Vec::new();
    let mut skipping = false;

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('<')
            && trimmed.ends_with('>')
            && trimmed.contains("</")
            && !trimmed.starts_with("</")
        {
            continue;
        }
        if trimmed.starts_with('<')
            && trimmed.ends_with('>')
            && !trimmed.starts_with("</")
            && trimmed.chars().filter(|ch| *ch == '<').count() == 1
        {
            skipping = true;
            continue;
        }
        if skipping {
            if trimmed.starts_with("</") && trimmed.ends_with('>') {
                skipping = false;
            }
            continue;
        }
        result.push(line);
    }

    result.join("\n")
}

#[cfg(test)]
mod running_generation_tests {
    use super::*;
    use crate::types::{AgentLifecycleDisposition, ChildAgentWorkspaceMode};
    #[tokio::test]
    async fn actual_worktree_running_source_carries_qualified_birth_and_raw_work() -> Result<()> {
        let harness = crate::runtime::tests::support::LifecycleHarness::new();
        let runtime = harness.runtime();
        let work = runtime
            .create_work_item("original worktree owner".into(), None, None, vec![])
            .await?;
        let at = runtime.now();
        let task = TaskRecord {
            id: crate::ids::task_id(),
            agent_id: "default".into(),
            kind: TaskKind::WorktreeSubagentTask,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: Some(work.id.clone()),
            summary: Some("original worktree Running".into()),
            detail: Some(serde_json::json!({"workspace_mode":"worktree"})),
            recovery: Some(TaskRecoverySpec::ChildAgentTask {
                summary: "original worktree Running".into(),
                prompt: "accepted".into(),
                authority_class: AuthorityClass::OperatorInstruction,
                workspace_mode: ChildAgentWorkspaceMode::Worktree,
                lifecycle_disposition: AgentLifecycleDisposition::Retain,
            }),
        };
        runtime
            .persist_task_transition(&task, "actual_worktree_running_admission")
            .await?;
        runtime
            .emit_worktree_child_task_running_status(
                &task.agent_id,
                &task.id,
                &AuthorityClass::OperatorInstruction,
            )
            .await?;
        let raw:String=runtime.runtime_db().connection()?.query_row("SELECT payload_json FROM messages WHERE kind='task_status' AND json_extract(payload_json,'$.metadata.task_id')=?1 ORDER BY rowid DESC LIMIT 1",[&task.id],|r|r.get(0))?;
        let message: MessageEnvelope = serde_json::from_str(&raw)?;
        assert!(message.message_seq.is_some_and(|seq| seq > 0));
        assert_eq!(message.work_item_id, task.work_item_id);
        let metadata = message.metadata.as_ref().unwrap();
        assert_eq!(
            metadata["task_created_at"],
            serde_json::to_value(task.created_at)?
        );
        assert_eq!(
            metadata["work_item_id"],
            serde_json::to_value(&task.work_item_id)?
        );
        assert_eq!(metadata["task_detail"], serde_json::to_value(&task.detail)?);
        assert_eq!(
            metadata["task_recovery"],
            serde_json::to_value(&task.recovery)?
        );
        assert_eq!(
            runtime
                .runtime_db()
                .tasks()
                .latest(&task.id)?
                .unwrap()
                .created_at,
            task.created_at
        );
        Ok(())
    }
}
