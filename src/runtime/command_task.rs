use std::{path::PathBuf, time::Duration};

use anyhow::{anyhow, Context, Result};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    runtime::task_state_reducer::has_blocking_active_tasks,
    system::{
        workspace::WorkspacePathError, CaptureSpec, ExecutionScopeKind, ExecutionSnapshot,
        ProcessHost, ProcessPurpose, ProcessRequest, ProgramInvocation, RunningProcess,
        RunningProcessExitStatus, StdioSpec, StopSignal,
    },
    tool::helpers::{
        command_cost_diagnostics, command_preview, effective_tool_output_tokens,
        output_char_budget, truncate_output_to_char_budget, truncate_output_with_flag,
        truncate_text,
    },
    tool::ToolError,
    types::{
        AgentStatus, CommandCostDiagnostics, CommandTaskSpec, ExecCommandOutcome,
        ExecCommandResult, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
        TaskHandle, TaskKind, TaskRecord, TaskRecoverySpec, TaskStatus, ToolArtifactRef,
        TrustLevel,
    },
};

use super::RuntimeHandle;

const OUTPUT_CHANNEL_CAPACITY: usize = 64;
const INPUT_CHANNEL_CAPACITY: usize = 16;
const STREAM_TAIL_CHAR_LIMIT: usize = 128_000;
const COMBINED_TAIL_CHAR_LIMIT: usize = 256_000;
const PROCESS_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub(super) enum ManagedTaskHandle {
    Async(JoinHandle<()>),
    Command(CommandTaskHandle),
}

pub(super) struct CommandTaskHandle {
    pub(super) cancel_tx: Option<oneshot::Sender<()>>,
    pub(super) force_stop_tx: Option<oneshot::Sender<()>>,
    pub(super) input_tx: mpsc::Sender<CommandTaskInputRequest>,
}

pub(super) struct CommandTaskInputRequest {
    pub(super) text: String,
    pub(super) response_tx: oneshot::Sender<Result<u64, String>>,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedCommandTask {
    spec: CommandTaskSpec,
    workdir: PathBuf,
    output_path: PathBuf,
    execution: ExecutionSnapshot,
}

pub(super) struct RunningCommand {
    process: Box<dyn RunningProcess>,
    output_rx: mpsc::Receiver<OutputChunk>,
    reader_handles: Vec<JoinHandle<()>>,
}

struct CommandTaskRunOutcome {
    cancelled: bool,
    cancel_requested: bool,
    force_stop_requested: bool,
    exit_status: RunningProcessExitStatus,
}

#[derive(Debug, Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

struct OutputChunk {
    stream: OutputStream,
    text: String,
}

#[derive(Debug, Default, Clone)]
struct CapturedOutput {
    stdout: String,
    stderr: String,
    combined: String,
}

impl CapturedOutput {
    fn push(&mut self, chunk: OutputChunk) {
        push_tail(&mut self.combined, &chunk.text, COMBINED_TAIL_CHAR_LIMIT);
        match chunk.stream {
            OutputStream::Stdout => {
                push_tail(&mut self.stdout, &chunk.text, STREAM_TAIL_CHAR_LIMIT)
            }
            OutputStream::Stderr => {
                push_tail(&mut self.stderr, &chunk.text, STREAM_TAIL_CHAR_LIMIT)
            }
        }
    }

    fn initial_output(&self, max_output_tokens: Option<u64>) -> Option<String> {
        self.initial_output_with_flag(max_output_tokens).0
    }

    fn initial_output_with_flag(&self, max_output_tokens: Option<u64>) -> (Option<String>, bool) {
        if self.combined.trim().is_empty() {
            (None, false)
        } else {
            let (output, truncated) = truncate_output_with_flag(
                &self.combined,
                max_output_tokens.map(|value| value as usize),
            );
            (Some(output), truncated)
        }
    }

    fn summary(&self, max_output_tokens: Option<u64>) -> Option<String> {
        self.summary_with_flag(max_output_tokens).0
    }

    fn summary_with_flag(&self, max_output_tokens: Option<u64>) -> (Option<String>, bool) {
        let stdout = self.stdout.trim();
        let stderr = self.stderr.trim();
        if stdout.is_empty() && stderr.is_empty() {
            return (None, false);
        }
        let content = match (stdout.is_empty(), stderr.is_empty()) {
            (false, true) => stdout.to_string(),
            (true, false) => format!("stderr:\n{stderr}"),
            (false, false) => format!("stdout:\n{stdout}\n\nstderr:\n{stderr}"),
            (true, true) => String::new(),
        };
        let (summary, truncated) =
            truncate_output_with_flag(&content, max_output_tokens.map(|value| value as usize));
        (Some(summary), truncated)
    }
}

impl RuntimeHandle {
    async fn ensure_process_execution_exposed(&self, surface: &str) -> Result<()> {
        let state = self.agent_state().await?;
        crate::system::ensure_process_execution_allowed(
            &crate::system::HostLocalBoundary::from_parts(
                &state.execution_profile,
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.projection_kind),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.access_mode),
                state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.execution_root_id.clone()),
            ),
            surface,
        )
    }

    pub async fn schedule_command_task(
        &self,
        summary: String,
        spec: CommandTaskSpec,
        trust: TrustLevel,
    ) -> Result<TaskRecord> {
        self.ensure_background_tasks_allowed("command_task").await?;
        self.ensure_process_execution_exposed("command_task")
            .await?;

        let resolved = self.resolve_command_task(&spec).await?;
        let running = self.start_command_process(&resolved).await?;
        self.register_command_task(
            summary,
            resolved,
            running,
            trust,
            false,
            CapturedOutput::default(),
        )
        .await
    }

    pub(crate) async fn execute_exec_command(
        &self,
        mut spec: CommandTaskSpec,
        trust: &TrustLevel,
    ) -> Result<ExecCommandResult> {
        self.ensure_process_execution_exposed("ExecCommand").await?;
        self.apply_command_output_policy(&mut spec);
        let diagnostics = self.command_cost_diagnostics_for(&spec);
        let resolved = self.resolve_command_task(&spec).await?;
        self.append_audit_event(
            "process_execution_requested",
            serde_json::json!({
                "surface": "ExecCommand",
                "trust": trust,
                "cmd_preview": diagnostics.cmd_preview.clone(),
                "command_cost": diagnostics.clone(),
                "execution": resolved.execution.clone(),
                "boundary": crate::system::HostLocalBoundary::from_snapshot(&resolved.execution).audit_metadata(),
                "workdir": resolved.workdir.clone(),
            }),
        )?;
        let mut running = self.start_command_process(&resolved).await?;
        let mut captured = CapturedOutput::default();
        let sleep = tokio::time::sleep(Duration::from_millis(spec.yield_time_ms));
        let mut status_tick = tokio::time::interval(PROCESS_STATUS_POLL_INTERVAL);
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(sleep);

        loop {
            tokio::select! {
                chunk = running.output_rx.recv() => {
                    if let Some(chunk) = chunk {
                        captured.push(chunk);
                    }
                }
                _ = status_tick.tick() => {
                    if let Some(status) = running
                        .process
                        .try_status()
                        .await
                        .context("failed to query command status")?
                    {
                        collect_remaining_output(&mut running, &mut captured).await;
                        return self
                            .complete_exec_command_result(
                                &captured,
                                &status,
                                spec.max_output_tokens,
                                Some(diagnostics.clone()),
                            )
                            .await;
                    }
                }
                _ = &mut sleep => {
                    if let Ok(wait_result) = tokio::time::timeout(
                        PROCESS_STATUS_POLL_INTERVAL,
                        running.process.wait(),
                    )
                    .await
                    {
                        let status = wait_result
                            .context("failed to wait for command status during promotion boundary")?;
                        collect_remaining_output(&mut running, &mut captured).await;
                        return self
                            .complete_exec_command_result(
                                &captured,
                                &status,
                                spec.max_output_tokens,
                                Some(diagnostics.clone()),
                            )
                            .await;
                    }
                    if let Some(status) = running
                        .process
                        .try_status()
                        .await
                        .context("failed to query command status")?
                    {
                        collect_remaining_output(&mut running, &mut captured).await;
                        return self
                            .complete_exec_command_result(
                                &captured,
                                &status,
                                spec.max_output_tokens,
                                Some(diagnostics.clone()),
                            )
                            .await;
                    }
                    let task = self
                        .register_command_task(
                            format!("Run command: {}", truncate_text(&spec.cmd, 80)),
                            resolved,
                            running,
                            trust.clone(),
                            true,
                            captured.clone(),
                        )
                        .await?;
                    let (initial_output_preview, initial_output_truncated) =
                        captured.initial_output_with_flag(spec.max_output_tokens);
                    return Ok(ExecCommandResult {
                        outcome: ExecCommandOutcome::PromotedToTask {
                            task_handle: TaskHandle::from_task_record(&task, None),
                            initial_output_preview,
                            initial_output_truncated,
                        },
                        summary_text: Some("command promoted to a managed task".to_string()),
                        command_diagnostics: Some(diagnostics),
                    });
                }
            }
        }
    }

    pub(crate) async fn execute_exec_command_once(
        &self,
        mut spec: CommandTaskSpec,
        trust: &TrustLevel,
    ) -> Result<ExecCommandResult> {
        self.ensure_process_execution_exposed("ExecCommandBatch")
            .await?;
        self.apply_command_output_policy(&mut spec);
        let diagnostics = self.command_cost_diagnostics_for(&spec);
        let resolved = self.resolve_command_task(&spec).await?;
        self.append_audit_event(
            "process_execution_requested",
            serde_json::json!({
                "surface": "ExecCommandBatch",
                "trust": trust,
                "cmd_preview": diagnostics.cmd_preview.clone(),
                "command_cost": diagnostics.clone(),
                "execution": resolved.execution.clone(),
                "boundary": crate::system::HostLocalBoundary::from_snapshot(&resolved.execution).audit_metadata(),
                "workdir": resolved.workdir.clone(),
            }),
        )?;
        let mut captured = CapturedOutput::default();
        let mut running = self.start_command_process(&resolved).await?;
        let sleep = tokio::time::sleep(Duration::from_millis(resolved.spec.yield_time_ms));
        let mut status_tick = tokio::time::interval(PROCESS_STATUS_POLL_INTERVAL);
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(sleep);

        loop {
            tokio::select! {
                chunk = running.output_rx.recv() => {
                    if let Some(chunk) = chunk {
                        captured.push(chunk);
                    }
                }
                _ = status_tick.tick() => {
                    if let Some(status) = running
                        .process
                        .try_status()
                        .await
                        .context("failed to query command status")?
                    {
                        collect_remaining_output(&mut running, &mut captured).await;
                        return self
                            .complete_exec_command_result(
                                &captured,
                                &status,
                                resolved.spec.max_output_tokens,
                                Some(diagnostics.clone()),
                            )
                            .await;
                    }
                }
                _ = &mut sleep => {
                    let _ = running.process.stop(StopSignal::Kill).await;
                    collect_remaining_output(&mut running, &mut captured).await;
                    return Err(ToolError::new(
                        "command_timed_out",
                        format!(
                            "command exceeded timeout of {} ms",
                            resolved.spec.yield_time_ms
                        ),
                    )
                    .with_details(json!({
                        "cmd_preview": command_preview(&resolved.spec.cmd),
                        "command_cost": self.command_cost_diagnostics_for(&resolved.spec),
                        "workdir": resolved.workdir.clone(),
                        "yield_time_ms": resolved.spec.yield_time_ms,
                    }))
                    .with_recovery_hint("increase `yield_time_ms`, narrow the command, or call ExecCommand directly when background task promotion is needed")
                    .with_retryable(false)
                    .into());
                }
            }
        }
    }

    fn apply_command_output_policy(&self, spec: &mut CommandTaskSpec) {
        let effective = effective_tool_output_tokens(
            spec.max_output_tokens,
            self.inner.default_tool_output_tokens,
            self.inner.max_tool_output_tokens,
        );
        spec.max_output_tokens = Some(effective);
    }

    fn command_cost_diagnostics_for(&self, spec: &CommandTaskSpec) -> CommandCostDiagnostics {
        command_cost_diagnostics(
            &spec.cmd,
            spec.max_output_tokens.unwrap_or_else(|| {
                effective_tool_output_tokens(
                    None,
                    self.inner.default_tool_output_tokens,
                    self.inner.max_tool_output_tokens,
                )
            }),
        )
    }

    pub(super) async fn resolve_command_task(
        &self,
        spec: &CommandTaskSpec,
    ) -> Result<ResolvedCommandTask> {
        let execution = self
            .effective_execution(ExecutionScopeKind::CommandTask)
            .await?;
        let execution_snapshot = execution.snapshot();
        let view = &execution.workspace;
        let workdir = spec
            .workdir
            .as_deref()
            .map(|value| view.resolve_path(value))
            .map(|result| {
                result.map_err(|error| {
                    if error
                        .downcast_ref::<WorkspacePathError>()
                        .is_some_and(|workspace_error| {
                            workspace_error.kind()
                                == crate::system::workspace::WorkspacePathErrorKind::ExecutionRootViolation
                        })
                    {
                        ToolError::new(
                            "execution_root_violation",
                            "requested working directory is outside the current execution root",
                        )
                        .with_details(json!({
                            "attempted_workdir": spec.workdir.clone(),
                            "execution_root": view.execution_root(),
                            "cwd": view.cwd(),
                        }))
                        .with_recovery_hint("omit `workdir` to use the current workspace cwd, or provide a relative path inside the workspace")
                        .with_retryable(false)
                        .into()
                    } else {
                        error
                    }
                })
            })
            .transpose()?
            .unwrap_or_else(|| view.cwd().to_path_buf());

        Ok(ResolvedCommandTask {
            spec: spec.clone(),
            workdir,
            output_path: PathBuf::new(),
            execution: execution_snapshot,
        })
    }

    async fn register_command_task(
        &self,
        summary: String,
        mut resolved: ResolvedCommandTask,
        running: RunningCommand,
        trust: TrustLevel,
        promoted_from_exec_command: bool,
        initial_capture: CapturedOutput,
    ) -> Result<TaskRecord> {
        let agent_id = self.agent_id().await?;
        let task_id = uuid::Uuid::new_v4().to_string();
        resolved.output_path = self.command_task_output_path(&task_id)?;
        let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        let detail = command_task_detail(
            &resolved,
            promoted_from_exec_command,
            &initial_capture,
            None,
            None,
            false,
        );
        let task = TaskRecord {
            id: task_id.clone(),
            agent_id: agent_id.clone(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Queued,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            parent_message_id: None,
            summary: Some(summary.clone()),
            detail: Some(detail),
            recovery: Some(TaskRecoverySpec::CommandTask {
                summary,
                spec: resolved.spec.clone(),
                trust: trust.clone(),
                promoted_from_exec_command,
            }),
        };
        self.append_audit_event(
            "process_execution_requested",
            serde_json::json!({
                "surface": "command_task",
                "task_id": task_id,
                "trust": trust,
                "execution": resolved.execution.clone(),
                "boundary": crate::system::HostLocalBoundary::from_snapshot(&resolved.execution).audit_metadata(),
                "workdir": resolved.workdir.clone(),
                "promoted_from_exec_command": promoted_from_exec_command,
            }),
        )?;
        self.inner.storage.append_task(&task)?;
        self.inner
            .storage
            .append_event(&crate::types::AuditEvent::new(
                "task_created",
                crate::storage::to_json_value(&task),
            ))?;
        {
            let mut guard = self.inner.agent.lock().await;
            if !guard.state.active_task_ids.contains(&task.id) {
                guard.state.active_task_ids.push(task.id.clone());
            }
            if task.is_blocking()
                && !matches!(
                    guard.state.status,
                    AgentStatus::Paused | AgentStatus::Stopped
                )
            {
                guard.state.status = AgentStatus::AwaitingTask;
            }
            self.inner.storage.write_agent(&guard.state)?;
        }

        let (cancel_tx, cancel_rx) = oneshot::channel();
        let (force_stop_tx, force_stop_rx) = oneshot::channel();
        let runtime = self.clone();
        let task_record = task.clone();
        let task_record_for_error = task.clone();
        let resolved_for_error = resolved.clone();
        tokio::spawn(async move {
            if let Err(err) = runtime
                .run_command_task(
                    agent_id,
                    task_record,
                    resolved,
                    running,
                    trust,
                    cancel_rx,
                    force_stop_rx,
                    input_rx,
                    promoted_from_exec_command,
                    initial_capture,
                )
                .await
            {
                let _ = runtime
                    .inner
                    .storage
                    .append_event(&crate::types::AuditEvent::new(
                        "command_task_runner_failed",
                        serde_json::json!({
                            "task_id": task_id,
                            "error": err.to_string(),
                        }),
                    ));
                let _ = runtime
                    .persist_command_task_terminal_state(
                        &task_record_for_error,
                        TaskStatus::Failed,
                        command_task_detail(
                            &resolved_for_error,
                            promoted_from_exec_command,
                            &CapturedOutput::default(),
                            None,
                            Some(&err.to_string()),
                            true,
                        ),
                        true,
                        "command_task_result_persisted_after_runner_failure",
                    )
                    .await;
                runtime
                    .inner
                    .task_handles
                    .lock()
                    .await
                    .remove(&task_record_for_error.id);
            }
        });
        self.inner.task_handles.lock().await.insert(
            task.id.clone(),
            ManagedTaskHandle::Command(CommandTaskHandle {
                cancel_tx: Some(cancel_tx),
                force_stop_tx: Some(force_stop_tx),
                input_tx,
            }),
        );

        Ok(task)
    }

    async fn run_command_task(
        &self,
        agent_id: String,
        task_record: TaskRecord,
        resolved: ResolvedCommandTask,
        mut running: RunningCommand,
        trust: TrustLevel,
        mut cancel_rx: oneshot::Receiver<()>,
        mut force_stop_rx: oneshot::Receiver<()>,
        mut input_rx: mpsc::Receiver<CommandTaskInputRequest>,
        promoted_from_exec_command: bool,
        initial_capture: CapturedOutput,
    ) -> Result<()> {
        let mut captured = initial_capture;
        let terminal = match self
            .run_command_task_inner(
                &agent_id,
                &task_record,
                &resolved,
                &mut running,
                &trust,
                &mut cancel_rx,
                &mut force_stop_rx,
                &mut input_rx,
                promoted_from_exec_command,
                &mut captured,
            )
            .await
        {
            Ok(outcome) => CommandTaskTerminal {
                status: if outcome.cancelled {
                    TaskStatus::Cancelled
                } else if outcome.exit_status.success() {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Failed
                },
                exit_status: outcome.exit_status.code(),
                error: None,
                cancel_requested: outcome.cancel_requested,
                force_stop_requested: outcome.force_stop_requested,
            },
            Err(err) => {
                let _ = running.process.stop(StopSignal::Kill).await;
                let _ = running.process.wait().await;
                collect_remaining_output(&mut running, &mut captured).await;
                CommandTaskTerminal {
                    status: TaskStatus::Failed,
                    exit_status: None,
                    error: Some(err.to_string()),
                    cancel_requested: false,
                    force_stop_requested: false,
                }
            }
        };
        let status_label = task_status_label(&terminal.status);
        let mut detail = command_task_detail(
            &resolved,
            promoted_from_exec_command,
            &captured,
            terminal.exit_status,
            terminal.error.as_deref(),
            true,
        );
        apply_command_task_cancel_provenance(
            &mut detail,
            &terminal.status,
            terminal.cancel_requested,
            terminal.force_stop_requested,
        );
        self.persist_command_task_terminal_state(
            &task_record,
            terminal.status.clone(),
            detail.clone(),
            !task_record.is_blocking(),
            "command_task_terminal_persisted",
        )
        .await?;
        let result_text = build_command_task_result_text(
            task_record.summary.as_deref().unwrap_or(&resolved.spec.cmd),
            &resolved.output_path,
            status_label,
            terminal.exit_status,
            captured.summary(resolved.spec.max_output_tokens),
            terminal.error.as_deref(),
        );
        let result_message = MessageEnvelope {
            metadata: Some({
                serde_json::json!({
                "task_id": task_record.id,
                "task_kind": task_record.kind,
                "task_status": status_label,
                "task_summary": task_record.summary,
                "task_detail": detail.clone(),
                "task_recovery": task_record.recovery,
                })
            }),
            ..MessageEnvelope::new(
                agent_id.clone(),
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: task_record.id.clone(),
                },
                trust.clone(),
                Priority::Next,
                MessageBody::Text { text: result_text },
            )
            .with_admission(
                crate::types::MessageDeliverySurface::TaskRejoin,
                crate::types::AdmissionContext::RuntimeOwned,
            )
        };
        let enqueue_result = self.enqueue(result_message).await;
        if let Err(err) = enqueue_result {
            self.inner
                .storage
                .append_event(&crate::types::AuditEvent::new(
                    "command_task_result_enqueue_failed",
                    serde_json::json!({
                        "task_id": task_record.id,
                        "error": err.to_string(),
                    }),
                ))?;
            self.persist_command_task_terminal_state(
                &task_record,
                terminal.status.clone(),
                detail,
                true,
                "command_task_result_persisted_after_enqueue_failure",
            )
            .await?;
        }

        self.inner.task_handles.lock().await.remove(&task_record.id);
        Ok(())
    }

    async fn run_command_task_inner(
        &self,
        agent_id: &str,
        task_record: &TaskRecord,
        resolved: &ResolvedCommandTask,
        running: &mut RunningCommand,
        trust: &TrustLevel,
        cancel_rx: &mut oneshot::Receiver<()>,
        force_stop_rx: &mut oneshot::Receiver<()>,
        input_rx: &mut mpsc::Receiver<CommandTaskInputRequest>,
        promoted_from_exec_command: bool,
        captured: &mut CapturedOutput,
    ) -> Result<CommandTaskRunOutcome> {
        let mut file = self
            .system()
            .open_output_file(&resolved.output_path)
            .await?;
        if !captured.combined.is_empty() {
            file.write_all(captured.combined.as_bytes()).await?;
        }
        file.flush().await?;
        let latest_status = self
            .inner
            .storage
            .latest_task_record(&task_record.id)?
            .map(|task| task.status);
        if !matches!(
            latest_status,
            Some(TaskStatus::Cancelling)
                | Some(TaskStatus::Completed)
                | Some(TaskStatus::Failed)
                | Some(TaskStatus::Cancelled)
                | Some(TaskStatus::Interrupted)
        ) {
            self.inner.storage.append_task(&TaskRecord {
                id: task_record.id.clone(),
                agent_id: task_record.agent_id.clone(),
                kind: task_record.kind.clone(),
                status: TaskStatus::Running,
                created_at: task_record.created_at,
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                summary: task_record.summary.clone(),
                detail: Some(command_task_detail(
                    resolved,
                    promoted_from_exec_command,
                    captured,
                    None,
                    None,
                    false,
                )),
                recovery: task_record.recovery.clone(),
            })?;
            self.inner
                .storage
                .append_event(&crate::types::AuditEvent::new(
                    "command_task_running_persisted",
                    serde_json::json!({
                        "task_id": task_record.id,
                    }),
                ))?;

            let running_message = MessageEnvelope {
                metadata: Some(serde_json::json!({
                    "task_id": task_record.id,
                    "task_kind": task_record.kind,
                    "task_status": "running",
                    "task_summary": task_record.summary,
                    "task_detail": command_task_detail(
                        resolved,
                        promoted_from_exec_command,
                        captured,
                        None,
                        None,
                        false,
                    ),
                    "task_recovery": task_record.recovery,
                })),
                ..MessageEnvelope::new(
                    agent_id.to_string(),
                    MessageKind::TaskStatus,
                    MessageOrigin::Task {
                        task_id: task_record.id.clone(),
                    },
                    trust.clone(),
                    Priority::Background,
                    MessageBody::Text {
                        text: format!(
                            "command task started: {}",
                            task_record.summary.clone().unwrap_or_default()
                        ),
                    },
                )
                .with_admission(
                    crate::types::MessageDeliverySurface::TaskRejoin,
                    crate::types::AdmissionContext::RuntimeOwned,
                )
            };
            self.enqueue(running_message).await?;
        }

        let mut cancelled = false;
        let mut cancellation_requested = false;
        let mut force_stop_requested = false;
        let mut output_closed = false;
        let mut status_tick = tokio::time::interval(PROCESS_STATUS_POLL_INTERVAL);
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let exit_status;
        loop {
            tokio::select! {
                chunk = running.output_rx.recv(), if !output_closed => {
                    match chunk {
                        Some(chunk) => {
                            file.write_all(chunk.text.as_bytes()).await?;
                            captured.push(chunk);
                        }
                        None => {
                            output_closed = true;
                        }
                    }
                }
                Some(request) = input_rx.recv() => {
                    let result = match running.process.write_stdin(request.text.as_bytes()).await {
                        Ok(()) => Ok(request.text.as_bytes().len() as u64),
                        Err(err) => Err(format!("{err:#}")),
                    };
                    let _ = request.response_tx.send(result);
                }
                _ = status_tick.tick() => {
                    if let Some(status) = running
                        .process
                        .try_status()
                        .await
                        .context("failed to query command status")?
                    {
                        exit_status = status;
                        break;
                    }
                }
                _ = &mut *cancel_rx, if !cancellation_requested => {
                    cancelled = true;
                    cancellation_requested = true;
                    let _ = running.process.stop(StopSignal::Kill).await;
                }
                _ = &mut *force_stop_rx, if !force_stop_requested => {
                    cancelled = true;
                    cancellation_requested = true;
                    force_stop_requested = true;
                    let _ = running.process.stop(StopSignal::Kill).await;
                }
            }
        }

        collect_remaining_output_into_file(running, captured, &mut file).await?;
        file.flush().await?;
        Ok(CommandTaskRunOutcome {
            cancelled,
            cancel_requested: cancellation_requested,
            force_stop_requested,
            exit_status,
        })
    }

    async fn persist_command_task_terminal_state(
        &self,
        task_record: &TaskRecord,
        status: TaskStatus,
        detail: serde_json::Value,
        clear_active_state: bool,
        event_kind: &'static str,
    ) -> Result<()> {
        let fallback = TaskRecord {
            id: task_record.id.clone(),
            agent_id: task_record.agent_id.clone(),
            kind: task_record.kind.clone(),
            status,
            created_at: task_record.created_at,
            updated_at: chrono::Utc::now(),
            parent_message_id: None,
            summary: task_record.summary.clone(),
            detail: Some(detail),
            recovery: task_record.recovery.clone(),
        };
        self.inner.storage.append_task(&fallback)?;
        if clear_active_state {
            let mut guard = self.inner.agent.lock().await;
            guard.state.active_task_ids.retain(|id| id != &fallback.id);
            if !matches!(
                guard.state.status,
                AgentStatus::Paused | AgentStatus::Stopped
            ) {
                guard.state.status = if guard.state.current_run_id.is_some() {
                    guard.state.status.clone()
                } else {
                    if has_blocking_active_tasks(&self.inner.storage, &guard.state.active_task_ids)?
                    {
                        AgentStatus::AwaitingTask
                    } else {
                        AgentStatus::AwakeIdle
                    }
                };
            }
            self.inner.storage.write_agent(&guard.state)?;
        }
        self.inner
            .storage
            .append_event(&crate::types::AuditEvent::new(
                event_kind,
                crate::storage::to_json_value(&fallback),
            ))?;
        Ok(())
    }

    fn command_task_output_dir(&self) -> Result<PathBuf> {
        Ok(self.inner.storage.data_dir().join("task-output"))
    }

    fn command_task_output_path(&self, task_id: &str) -> Result<PathBuf> {
        Ok(self
            .command_task_output_dir()?
            .join(format!("{task_id}.log")))
    }

    fn tool_artifact_dir(&self) -> PathBuf {
        self.inner.storage.data_dir().join("tool-artifacts")
    }

    async fn persist_exec_command_artifact(&self, stream: &str, content: &str) -> Result<String> {
        let dir = self.tool_artifact_dir();
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let path = dir.join(format!(
            "exec-command-{}-{stream}.log",
            Uuid::new_v4().simple()
        ));
        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("failed to persist {}", path.display()))?;
        Ok(path.display().to_string())
    }

    async fn complete_exec_command_result(
        &self,
        captured: &CapturedOutput,
        status: &RunningProcessExitStatus,
        max_output_tokens: Option<u64>,
        command_diagnostics: Option<CommandCostDiagnostics>,
    ) -> Result<ExecCommandResult> {
        let stdout_raw = captured.stdout.as_str();
        let stderr_raw = captured.stderr.as_str();
        let stdout = stdout_raw.trim();
        let stderr = stderr_raw.trim();
        let non_empty_streams = usize::from(!stdout.is_empty()) + usize::from(!stderr.is_empty());
        let stream_count = non_empty_streams.max(1);
        let char_budget = output_char_budget(max_output_tokens.map(|value| value as usize));
        let per_stream_budget = char_budget / stream_count;
        let (stdout_preview, stdout_truncated) = if stdout.is_empty() {
            (None, false)
        } else {
            let (value, truncated) = truncate_output_to_char_budget(stdout, per_stream_budget);
            (Some(value), truncated)
        };
        let (stderr_preview, stderr_truncated) = if stderr.is_empty() {
            (None, false)
        } else {
            let (value, truncated) = truncate_output_to_char_budget(stderr, per_stream_budget);
            (Some(value), truncated)
        };

        let mut artifacts = Vec::new();
        let mut stdout_artifact = None;
        let mut stderr_artifact = None;
        if stdout_truncated {
            stdout_artifact = Some(artifacts.len());
            artifacts.push(ToolArtifactRef {
                path: self
                    .persist_exec_command_artifact("stdout", stdout_raw)
                    .await?,
            });
        }
        if stderr_truncated {
            stderr_artifact = Some(artifacts.len());
            artifacts.push(ToolArtifactRef {
                path: self
                    .persist_exec_command_artifact("stderr", stderr_raw)
                    .await?,
            });
        }

        let exit_status = status.code();
        Ok(ExecCommandResult {
            outcome: ExecCommandOutcome::Completed {
                exit_status,
                stdout_preview,
                stderr_preview,
                truncated: stdout_truncated || stderr_truncated,
                artifacts,
                stdout_artifact,
                stderr_artifact,
            },
            command_diagnostics,
            summary_text: Some(match exit_status {
                Some(code) => format!("command exited with status {code}"),
                None => format!("command exited with status {status}"),
            }),
        })
    }

    pub(super) async fn start_command_process(
        &self,
        resolved: &ResolvedCommandTask,
    ) -> Result<RunningCommand> {
        let system = self.system();
        let execution = self
            .effective_execution(ExecutionScopeKind::CommandTask)
            .await?;
        let mut process = system
            .spawn(
                &execution,
                ProcessRequest {
                    program: ProgramInvocation::Shell {
                        command: resolved.spec.cmd.clone(),
                        shell: resolved.spec.shell.clone(),
                        login: resolved.spec.login,
                    },
                    cwd: Some(resolved.workdir.clone()),
                    env: vec![],
                    stdin: if resolved.spec.accepts_input {
                        StdioSpec::Piped
                    } else {
                        StdioSpec::Null
                    },
                    tty: resolved.spec.tty,
                    capture: CaptureSpec::BOTH,
                    timeout: None,
                    purpose: ProcessPurpose::CommandTask,
                },
            )
            .await
            .map_err(|error| {
                ToolError::new("command_spawn_failed", "failed to start command process")
                    .with_details(json!({
                        "cmd": resolved.spec.cmd.clone(),
                        "shell": resolved.spec.shell.clone(),
                        "workdir": resolved.workdir.clone(),
                        "error": format!("{error:#}"),
                    }))
                    .with_recovery_hint(
                        "use a valid shell binary, or omit `shell` to use the default shell",
                    )
                    .with_retryable(false)
            })?;
        let stdout = process
            .take_stdout()
            .ok_or_else(|| anyhow!("failed to capture command stdout"))?;
        let (tx, rx) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
        let stdout_handle = tokio::spawn(read_output(stdout, OutputStream::Stdout, tx.clone()));
        let mut reader_handles = vec![stdout_handle];
        if let Some(stderr) = process.take_stderr() {
            reader_handles.push(tokio::spawn(read_output(stderr, OutputStream::Stderr, tx)));
        }

        Ok(RunningCommand {
            process,
            output_rx: rx,
            reader_handles,
        })
    }
}

async fn read_output<R>(mut reader: R, stream: OutputStream, tx: mpsc::Sender<OutputChunk>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                if tx.send(OutputChunk { stream, text }).await.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

async fn collect_remaining_output(running: &mut RunningCommand, captured: &mut CapturedOutput) {
    while let Some(chunk) = running.output_rx.recv().await {
        captured.push(chunk);
    }
    for handle in running.reader_handles.drain(..) {
        let _ = handle.await;
    }
}

async fn collect_remaining_output_into_file(
    running: &mut RunningCommand,
    captured: &mut CapturedOutput,
    file: &mut tokio::fs::File,
) -> Result<()> {
    while let Some(chunk) = running.output_rx.recv().await {
        file.write_all(chunk.text.as_bytes()).await?;
        captured.push(chunk);
    }
    for handle in running.reader_handles.drain(..) {
        let _ = handle.await;
    }
    Ok(())
}

fn build_command_task_result_text(
    summary: &str,
    output_path: &PathBuf,
    status_label: &str,
    exit_status: Option<i32>,
    output_summary: Option<String>,
    error: Option<&str>,
) -> String {
    let mut lines = vec![format!("command task {status_label}: {summary}")];
    lines.push(format!("output_path: {}", output_path.display()));
    if let Some(code) = exit_status {
        lines.push(format!("exit_status: {code}"));
    }
    if let Some(summary) = output_summary {
        lines.push(format!("output_summary:\n{summary}"));
    }
    if let Some(error) = error {
        lines.push(format!("error: {error}"));
    }
    lines.join("\n")
}

fn command_task_detail(
    resolved: &ResolvedCommandTask,
    promoted_from_exec_command: bool,
    captured: &CapturedOutput,
    exit_status: Option<i32>,
    error: Option<&str>,
    terminal_snapshot_ready: bool,
) -> serde_json::Value {
    serde_json::json!({
        "cmd": resolved.spec.cmd,
        "wait_policy": if resolved.spec.continue_on_result { "blocking" } else { "background" },
        "workdir": resolved.workdir,
        "execution": resolved.execution,
        "shell": resolved.spec.shell,
        "login": resolved.spec.login,
        "tty": resolved.spec.tty,
        "yield_time_ms": resolved.spec.yield_time_ms,
        "max_output_tokens": resolved.spec.max_output_tokens,
        "continue_on_result": resolved.spec.continue_on_result,
        "promoted_from_exec_command": promoted_from_exec_command,
        "accepts_input": resolved.spec.accepts_input && !terminal_snapshot_ready,
        "input_target": if resolved.spec.accepts_input && !terminal_snapshot_ready {
            Some(if resolved.spec.tty { "tty" } else { "stdin" })
        } else {
            None::<&str>
        },
        "output_path": resolved.output_path,
        "initial_output": captured.initial_output(resolved.spec.max_output_tokens),
        "output_summary": captured.summary(resolved.spec.max_output_tokens),
        "terminal_snapshot_ready": terminal_snapshot_ready,
        "exit_status": exit_status,
        "error": error,
    })
}

#[derive(Debug)]
struct CommandTaskTerminal {
    status: TaskStatus,
    exit_status: Option<i32>,
    error: Option<String>,
    cancel_requested: bool,
    force_stop_requested: bool,
}

fn apply_command_task_cancel_provenance(
    detail: &mut serde_json::Value,
    status: &TaskStatus,
    cancel_requested: bool,
    force_stop_requested: bool,
) {
    if *status != TaskStatus::Cancelled {
        return;
    }
    let Some(detail_map) = detail.as_object_mut() else {
        return;
    };
    if cancel_requested {
        detail_map.insert("cancel_requested".into(), serde_json::json!(true));
    }
    if force_stop_requested {
        detail_map.insert("force_stop_requested".into(), serde_json::json!(true));
        detail_map.insert(
            "cancelled_reason".into(),
            serde_json::json!("force_stop_requested"),
        );
    } else if cancel_requested {
        detail_map.insert(
            "cancelled_reason".into(),
            serde_json::json!("cancel_requested"),
        );
    }
}

fn task_status_label(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    }
}

fn push_tail(buffer: &mut String, chunk: &str, max_chars: usize) {
    buffer.push_str(chunk);
    trim_to_tail(buffer, max_chars);
}

fn trim_to_tail(buffer: &mut String, max_chars: usize) {
    let char_count = buffer.chars().count();
    if char_count <= max_chars {
        return;
    }
    let trim_chars = char_count - max_chars;
    let trim_at = buffer
        .char_indices()
        .nth(trim_chars)
        .map(|(index, _)| index)
        .unwrap_or(buffer.len());
    buffer.drain(..trim_at);
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::Utc;
    use tempfile::{tempdir, TempDir};
    use tokio::sync::Mutex;

    use crate::{
        context::ContextConfig,
        provider::StubProvider,
        system::{process::ProcessOutput, RunningProcess, RunningProcessExitStatus, StopSignal},
    };

    use super::*;

    #[derive(Clone)]
    struct FakeRunningProcess {
        status: Arc<Mutex<Option<RunningProcessExitStatus>>>,
        stop_status: RunningProcessExitStatus,
        wait_status: RunningProcessExitStatus,
        try_status_error: Option<String>,
        stdin: Arc<Mutex<Vec<u8>>>,
    }

    impl FakeRunningProcess {
        fn pending() -> Self {
            Self {
                status: Arc::new(Mutex::new(None)),
                stop_status: RunningProcessExitStatus::new(Some(143), None),
                wait_status: RunningProcessExitStatus::new(Some(143), None),
                try_status_error: None,
                stdin: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn failing_status(error: impl Into<String>) -> Self {
            Self {
                try_status_error: Some(error.into()),
                ..Self::pending()
            }
        }
    }

    #[async_trait]
    impl RunningProcess for FakeRunningProcess {
        fn id(&self) -> String {
            "fake-process".into()
        }

        fn take_stdout(&mut self) -> Option<Box<dyn ProcessOutput>> {
            None
        }

        fn take_stderr(&mut self) -> Option<Box<dyn ProcessOutput>> {
            None
        }

        async fn write_stdin(&mut self, data: &[u8]) -> Result<()> {
            self.stdin.lock().await.extend_from_slice(data);
            Ok(())
        }

        async fn wait(&mut self) -> Result<RunningProcessExitStatus> {
            if let Some(status) = self.status.lock().await.clone() {
                return Ok(status);
            }
            Ok(self.wait_status.clone())
        }

        async fn try_status(&mut self) -> Result<Option<RunningProcessExitStatus>> {
            if let Some(error) = self.try_status_error.as_ref() {
                return Err(anyhow::anyhow!(error.clone()));
            }
            Ok(self.status.lock().await.clone())
        }

        async fn stop(&mut self, _signal: StopSignal) -> Result<()> {
            *self.status.lock().await = Some(self.stop_status.clone());
            Ok(())
        }
    }

    fn test_runtime() -> (TempDir, TempDir, RuntimeHandle) {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "default",
            home.path().to_path_buf(),
            workspace.path().to_path_buf(),
            "http://127.0.0.1:7878".into(),
            Arc::new(StubProvider::new("done")),
            "default".into(),
            ContextConfig::default(),
        )
        .unwrap();
        (home, workspace, runtime)
    }

    fn command_spec(accepts_input: bool, continue_on_result: bool) -> CommandTaskSpec {
        CommandTaskSpec {
            cmd: "fake command".into(),
            workdir: None,
            shell: None,
            login: true,
            tty: false,
            yield_time_ms: 10,
            max_output_tokens: None,
            accepts_input,
            continue_on_result,
        }
    }

    async fn resolved_command(
        runtime: &RuntimeHandle,
        spec: &CommandTaskSpec,
    ) -> ResolvedCommandTask {
        runtime.resolve_command_task(spec).await.unwrap()
    }

    fn running_command(process: FakeRunningProcess, stdout: &str, stderr: &str) -> RunningCommand {
        let (tx, rx) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
        if !stdout.is_empty() {
            tx.try_send(OutputChunk {
                stream: OutputStream::Stdout,
                text: stdout.into(),
            })
            .unwrap();
        }
        if !stderr.is_empty() {
            tx.try_send(OutputChunk {
                stream: OutputStream::Stderr,
                text: stderr.into(),
            })
            .unwrap();
        }
        drop(tx);

        RunningCommand {
            process: Box::new(process),
            output_rx: rx,
            reader_handles: Vec::new(),
        }
    }

    fn task_record(
        id: &str,
        status: TaskStatus,
        summary: &str,
        resolved: &ResolvedCommandTask,
        accepts_input: bool,
        continue_on_result: bool,
    ) -> TaskRecord {
        let spec = command_spec(accepts_input, continue_on_result);
        TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: status.clone(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            summary: Some(summary.into()),
            detail: Some(command_task_detail(
                resolved,
                false,
                &CapturedOutput::default(),
                None,
                None,
                matches!(
                    status,
                    TaskStatus::Completed
                        | TaskStatus::Failed
                        | TaskStatus::Cancelled
                        | TaskStatus::Interrupted
                ),
            )),
            recovery: Some(TaskRecoverySpec::CommandTask {
                summary: summary.into(),
                spec,
                trust: TrustLevel::TrustedOperator,
                promoted_from_exec_command: false,
            }),
        }
    }

    async fn wait_for_latest_task(
        runtime: &RuntimeHandle,
        task_id: &str,
        expected: TaskStatus,
    ) -> TaskRecord {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(task) = runtime.inner.storage.latest_task_record(task_id).unwrap() {
                if task.status == expected {
                    return task;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "task {task_id} did not reach {expected:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn assert_output_file_contains(path: &Path, expected: &str) {
        let content = std::fs::read_to_string(path).expect("output file should be readable");
        assert!(
            content.contains(expected),
            "output file did not contain {expected:?}: {content:?}"
        );
    }

    #[tokio::test]
    async fn cancellation_after_partial_output_persists_terminal_detail() {
        let (_home, _workspace, runtime) = test_runtime();
        let spec = command_spec(false, false);
        let resolved = resolved_command(&runtime, &spec).await;
        let task = runtime
            .register_command_task(
                "cancel with output".into(),
                resolved,
                running_command(
                    FakeRunningProcess::pending(),
                    "partial stdout\n",
                    "partial stderr\n",
                ),
                TrustLevel::TrustedOperator,
                false,
                CapturedOutput::default(),
            )
            .await
            .unwrap();

        let handle = {
            let mut handles = runtime.inner.task_handles.lock().await;
            match handles.get_mut(&task.id) {
                Some(ManagedTaskHandle::Command(handle)) => handle
                    .cancel_tx
                    .take()
                    .expect("command task should expose cancel sender"),
                _ => panic!("command task handle should exist"),
            }
        };
        handle.send(()).unwrap();

        let latest = wait_for_latest_task(&runtime, &task.id, TaskStatus::Cancelled).await;
        let detail = latest.detail.as_ref().expect("terminal detail");
        assert_eq!(detail["terminal_snapshot_ready"].as_bool(), Some(true));
        assert_eq!(detail["cancel_requested"].as_bool(), Some(true));
        assert_eq!(
            detail["cancelled_reason"].as_str(),
            Some("cancel_requested")
        );
        assert_eq!(detail["force_stop_requested"].as_bool(), None);
        assert!(detail["output_summary"]
            .as_str()
            .expect("output summary")
            .contains("partial stdout"));
        let output_path = detail["output_path"].as_str().expect("output path");
        assert_output_file_contains(Path::new(output_path), "partial stderr");
    }

    #[tokio::test]
    async fn force_stop_persists_distinct_cancel_metadata() {
        let (_home, _workspace, runtime) = test_runtime();
        let spec = command_spec(false, false);
        let resolved = resolved_command(&runtime, &spec).await;
        let task = runtime
            .register_command_task(
                "force stop with output".into(),
                resolved,
                running_command(FakeRunningProcess::pending(), "before force stop\n", ""),
                TrustLevel::TrustedOperator,
                false,
                CapturedOutput::default(),
            )
            .await
            .unwrap();

        let handle = {
            let mut handles = runtime.inner.task_handles.lock().await;
            match handles.get_mut(&task.id) {
                Some(ManagedTaskHandle::Command(handle)) => handle
                    .force_stop_tx
                    .take()
                    .expect("command task should expose force-stop sender"),
                _ => panic!("command task handle should exist"),
            }
        };
        handle.send(()).unwrap();

        let latest = wait_for_latest_task(&runtime, &task.id, TaskStatus::Cancelled).await;
        let detail = latest.detail.as_ref().expect("terminal detail");
        assert_eq!(detail["cancel_requested"].as_bool(), Some(true));
        assert_eq!(detail["force_stop_requested"].as_bool(), Some(true));
        assert_eq!(
            detail["cancelled_reason"].as_str(),
            Some("force_stop_requested")
        );
        assert!(detail["output_summary"]
            .as_str()
            .expect("output summary")
            .contains("before force stop"));
    }

    #[tokio::test]
    async fn process_poll_failure_cleans_handle_and_persists_failed_terminal_state() {
        let (_home, _workspace, runtime) = test_runtime();
        let spec = command_spec(false, false);
        let resolved = resolved_command(&runtime, &spec).await;
        let task = runtime
            .register_command_task(
                "poll failure".into(),
                resolved,
                running_command(FakeRunningProcess::failing_status("poll exploded"), "", ""),
                TrustLevel::TrustedOperator,
                false,
                CapturedOutput::default(),
            )
            .await
            .unwrap();

        let latest = wait_for_latest_task(&runtime, &task.id, TaskStatus::Failed).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !runtime
                    .inner
                    .task_handles
                    .lock()
                    .await
                    .contains_key(&task.id)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("task handle should be removed after failed terminal state is persisted");
        let detail = latest.detail.as_ref().expect("failed detail");
        assert_eq!(detail["terminal_snapshot_ready"].as_bool(), Some(true));
        assert!(detail["error"]
            .as_str()
            .expect("failure error")
            .contains("failed to query command status"));

        let events = runtime.inner.storage.read_recent_events(20).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "command_task_terminal_persisted"
                && event.data["id"].as_str() == Some(task.id.as_str())
        }));
    }

    #[tokio::test]
    async fn task_input_rejects_terminal_command_task_without_dropping_input_metadata() {
        let (_home, _workspace, runtime) = test_runtime();
        let spec = command_spec(true, false);
        let mut resolved = resolved_command(&runtime, &spec).await;
        resolved.output_path = runtime.command_task_output_path("terminal-input").unwrap();
        let task = task_record(
            "terminal-input",
            TaskStatus::Completed,
            "terminal input",
            &resolved,
            true,
            false,
        );
        runtime.inner.storage.append_task(&task).unwrap();

        let result = runtime.task_input(&task.id, "hello\n").await.unwrap();

        assert!(!result.accepted_input);
        assert_eq!(result.task.status, TaskStatus::Completed);
        assert_eq!(result.input_target, None);
        assert_eq!(result.bytes_written, None);
        assert_eq!(
            result.summary_text.as_deref(),
            Some("task is not currently accepting input")
        );
        assert_eq!(
            result
                .task
                .command
                .as_ref()
                .and_then(|command| command.accepts_input),
            Some(false)
        );
        assert_eq!(
            result
                .task
                .command
                .as_ref()
                .and_then(|command| command.output_path.as_ref())
                .map(String::as_str),
            Some(resolved.output_path.to_string_lossy().as_ref())
        );
    }
}
