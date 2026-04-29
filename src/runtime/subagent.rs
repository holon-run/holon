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

        self.inner.storage.append_event(&AuditEvent::new(
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
        trust: &TrustLevel,
    ) -> Result<String> {
        let execution = self
            .effective_execution(ExecutionScopeKind::SubagentTask)
            .await?;
        self.run_subagent_prompt_for_workspace(
            agent_id,
            prompt,
            trust,
            &execution,
            serde_json::json!({
                "prompt": prompt,
                "trust": trust,
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
        trust: &TrustLevel,
        task_id: &str,
    ) -> Result<WorktreeSubagentResult> {
        let seed = self.prepare_managed_worktree_for_task(task_id).await?;
        self.emit_worktree_child_task_running_status(agent_id, task_id, trust)
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
                trust,
                &worktree_execution,
                serde_json::json!({
                    "prompt": prompt,
                    "trust": trust,
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
        trust: &TrustLevel,
    ) -> Result<()> {
        let Some(task) = self.inner.storage.latest_task_record(task_id)? else {
            return Ok(());
        };
        let task_summary = task.summary.clone().unwrap_or_default();
        let running_message = MessageEnvelope {
            metadata: Some(serde_json::json!({
                "task_id": task.id.clone(),
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
                trust.clone(),
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
        self.enqueue(running_message).await.map(|_| ())
    }

    pub(super) async fn run_subagent_prompt_for_workspace(
        &self,
        agent_id: &str,
        prompt: &str,
        trust: &TrustLevel,
        execution: &crate::system::EffectiveExecution,
        prompt_metadata: serde_json::Value,
        assistant_metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        let effective_prompt = self
            .build_subagent_prompt_for_workspace(agent_id, prompt, trust, execution)
            .await?;
        self.inner
            .storage
            .append_transcript_entry(&TranscriptEntry::new(
                agent_id.to_string(),
                TranscriptEntryKind::SubagentPrompt,
                None,
                None,
                prompt_metadata,
            ))?;
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
            let mut guard = self.inner.agent.lock().await;
            guard.state.total_input_tokens += response.input_tokens;
            guard.state.total_output_tokens += response.output_tokens;
            guard.state.total_model_rounds += 1;
            guard.state.last_turn_token_usage = Some(crate::types::TokenUsage::new(
                response.input_tokens,
                response.output_tokens,
            ));
            self.inner.storage.write_agent(&guard.state)?;
        }

        self.inner
            .storage
            .append_transcript_entry(&TranscriptEntry {
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
            })?;

        let text = sanitize_subagent_result(
            &response
                .blocks
                .into_iter()
                .filter_map(|block| match block {
                    ModelBlock::Text { text } => Some(text),
                    ModelBlock::ToolUse { .. } => None,
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
