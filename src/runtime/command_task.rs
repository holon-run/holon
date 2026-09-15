use std::{io::SeekFrom, path::PathBuf, time::Duration};

use anyhow::{anyhow, Context, Result};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    system::{
        CaptureSpec, ExecutionScopeKind, ExecutionSnapshot, ProcessHost, ProcessPurpose,
        ProcessRequest, ProgramInvocation, RunningProcess, RunningProcessExitStatus, StdioSpec,
        StopSignal,
    },
    tool::helpers::{
        command_cost_diagnostics, command_digest, command_display, command_preview,
        effective_tool_output_tokens, output_char_budget, truncate_output_with_flag, truncate_text,
    },
    tool::ToolError,
    types::{
        AuthorityClass, CommandCostDiagnostics, CommandTaskOutputCaptureSnapshot,
        CommandTaskOutputFailureCode, CommandTaskOutputPolicy, CommandTaskSpec,
        CommandTaskStatusSnapshot, ExecCommandDuplicatePolicy, ExecCommandOutcome,
        ExecCommandResult, ExternalTriggerScope, ExternalTriggerStatus, MessageBody,
        MessageEnvelope, MessageKind, MessageOrigin, Priority, TaskHandle, TaskKind, TaskRecord,
        TaskRecoverySpec, TaskStatus, ToolArtifactRef, HOLON_CALLER_AGENT_ID_ENV,
        HOLON_CALLER_AUTHORITY_CLASS_ENV, HOLON_CALLER_SOURCE_ACTIVATION_ID_ENV,
        HOLON_CALLER_SOURCE_TASK_ID_ENV, HOLON_CALLER_SOURCE_TURN_ID_ENV,
        HOLON_CALLER_SOURCE_WORK_ITEM_ID_ENV,
    },
    utf8::IncrementalUtf8LossyDecoder,
};

use super::{task_state_reducer, RuntimeHandle};

const OUTPUT_CHANNEL_CAPACITY: usize = 64;
const INPUT_CHANNEL_CAPACITY: usize = 16;
const STREAM_TAIL_CHAR_LIMIT: usize = 128_000;
const COMBINED_TAIL_CHAR_LIMIT: usize = 256_000;
const PERSISTED_TAIL_BYTE_LIMIT: usize = 256 * 1024;
const PERSISTED_TRUNCATION_MARKER_RESERVE: u64 = 256;
const DISK_SPACE_CHECK_INTERVAL_BYTES: u64 = 256 * 1024;
const PROCESS_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(25);
// After the main process exits, only give readers a short grace period to
// deliver already-buffered output. Background children may inherit stdout/stderr
// and keep the pipes open long after the foreground command has completed.
const COLLECT_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);

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
    env: Vec<(String, String)>,
    output_policy: CommandTaskOutputPolicy,
}

pub(super) struct RunningCommand {
    process: Box<dyn RunningProcess>,
    output_rx: mpsc::Receiver<OutputChunk>,
    reader_handles: Vec<JoinHandle<()>>,
    trace: Option<RunningCommandTrace>,
}

struct RunningCommandTrace {
    child_process_context: crate::observability::TraceContext,
    output_collect_context: crate::observability::TraceContext,
    parent_span_id: String,
    started_at: chrono::DateTime<chrono::Utc>,
    tool_name: String,
}

struct CommandTaskRunOutcome {
    cancelled: bool,
    cancel_requested: bool,
    force_stop_requested: bool,
    exit_status: RunningProcessExitStatus,
    process_completed_at: chrono::DateTime<chrono::Utc>,
    output_completed_at: chrono::DateTime<chrono::Utc>,
    output_failure: Option<CommandTaskOutputFailure>,
}

#[derive(Debug, Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

struct OutputChunk {
    stream: OutputStream,
    text: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Default, Clone)]
struct CapturedOutput {
    stdout: String,
    stderr: String,
    combined: String,
    emitted_bytes: u64,
    decoded_bytes: u64,
    retained_bytes: u64,
    dropped_bytes: u64,
    retention_limit_bytes: u64,
    execution_quota_bytes: u64,
    truncated: bool,
    raw_retained: Vec<u8>,
    raw_tail: Vec<u8>,
    raw_truncated: bool,
    output_failure: Option<CommandTaskOutputFailure>,
}

#[derive(Debug, Clone)]
struct CommandTaskOutputFailure {
    code: CommandTaskOutputFailureCode,
    message: String,
    available_disk_bytes: Option<u64>,
    required_free_disk_bytes: Option<u64>,
}

struct BoundedOutputFile {
    file: tokio::fs::File,
    path: PathBuf,
    policy: CommandTaskOutputPolicy,
    tail_limit: usize,
    head_limit: u64,
    retained_bytes: u64,
    rolling_tail: Vec<u8>,
    truncated: bool,
    next_disk_check_at: u64,
}

struct CommandTaskMatch {
    id: String,
    kind: String,
    status: crate::types::TaskStatus,
    summary: Option<String>,
    command: Option<CommandTaskStatusSnapshot>,
}

impl CapturedOutput {
    fn new(policy: CommandTaskOutputPolicy) -> Self {
        Self {
            retention_limit_bytes: policy.retention_bytes,
            execution_quota_bytes: policy.execution_quota_bytes,
            ..Self::default()
        }
    }

    fn push(&mut self, chunk: &OutputChunk) {
        self.emitted_bytes = self.emitted_bytes.saturating_add(chunk.bytes.len() as u64);
        self.decoded_bytes = self.decoded_bytes.saturating_add(chunk.text.len() as u64);
        push_tail(&mut self.combined, &chunk.text, COMBINED_TAIL_CHAR_LIMIT);
        match chunk.stream {
            OutputStream::Stdout => {
                push_tail(&mut self.stdout, &chunk.text, STREAM_TAIL_CHAR_LIMIT)
            }
            OutputStream::Stderr => {
                push_tail(&mut self.stderr, &chunk.text, STREAM_TAIL_CHAR_LIMIT)
            }
        }
        let tail_limit = persisted_tail_limit(self.retention_limit_bytes);
        push_rolling_bytes(&mut self.raw_tail, &chunk.bytes, tail_limit);
        if !self.raw_truncated {
            let remaining = self
                .retention_limit_bytes
                .saturating_sub(self.raw_retained.len() as u64);
            let retain = remaining.min(chunk.bytes.len() as u64) as usize;
            self.raw_retained.extend_from_slice(&chunk.bytes[..retain]);
            if retain < chunk.bytes.len() {
                self.raw_truncated = true;
                self.raw_retained
                    .truncate(persisted_head_limit(self.retention_limit_bytes));
            }
        }
    }

    fn apply_persistence(&mut self, retained_bytes: u64, truncated: bool) {
        self.retained_bytes = retained_bytes;
        self.dropped_bytes = self.emitted_bytes.saturating_sub(retained_bytes);
        self.truncated = truncated || self.dropped_bytes > 0;
    }

    fn fail(&mut self, failure: CommandTaskOutputFailure) {
        self.output_failure = Some(failure);
    }

    fn output_capture_snapshot(&self) -> CommandTaskOutputCaptureSnapshot {
        CommandTaskOutputCaptureSnapshot {
            emitted_bytes: self.emitted_bytes,
            decoded_bytes: self.decoded_bytes,
            retained_bytes: self.retained_bytes,
            dropped_bytes: self.dropped_bytes,
            retention_limit_bytes: self.retention_limit_bytes,
            execution_quota_bytes: self.execution_quota_bytes,
            truncated: self.truncated,
            failure_code: self.output_failure.as_ref().map(|failure| failure.code),
            available_disk_bytes: self
                .output_failure
                .as_ref()
                .and_then(|failure| failure.available_disk_bytes),
            required_free_disk_bytes: self
                .output_failure
                .as_ref()
                .and_then(|failure| failure.required_free_disk_bytes),
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

impl BoundedOutputFile {
    async fn open(
        system: &crate::system::LocalSystem,
        path: PathBuf,
        policy: CommandTaskOutputPolicy,
    ) -> Result<Self> {
        let file = system.open_output_file(&path).await?;
        let tail_limit = persisted_tail_limit(policy.retention_bytes);
        let head_limit = persisted_head_limit(policy.retention_bytes) as u64;
        Ok(Self {
            file,
            path,
            policy,
            tail_limit,
            head_limit,
            retained_bytes: 0,
            rolling_tail: Vec::with_capacity(tail_limit),
            truncated: false,
            next_disk_check_at: 0,
        })
    }

    async fn write_chunk(
        &mut self,
        system: &crate::system::LocalSystem,
        bytes: &[u8],
        emitted_bytes: u64,
    ) -> std::result::Result<Option<CommandTaskOutputFailure>, std::io::Error> {
        let remaining = self
            .policy
            .retention_bytes
            .saturating_sub(self.retained_bytes);
        let write_len = if self.truncated {
            0
        } else {
            remaining.min(bytes.len() as u64)
        };
        if emitted_bytes >= self.next_disk_check_at {
            self.next_disk_check_at = emitted_bytes.saturating_add(DISK_SPACE_CHECK_INTERVAL_BYTES);
            let probe_path = self.path.parent().unwrap_or(self.path.as_path());
            match system.filesystem_space(probe_path) {
                Ok((available, total)) => {
                    let required = required_free_disk_bytes(self.policy, total);
                    if would_cross_disk_waterline(available, required, write_len) {
                        return Ok(Some(CommandTaskOutputFailure {
                            code: CommandTaskOutputFailureCode::LowDiskSpace,
                            message: format!(
                                "command output stopped because writing {write_len} bytes would cross the configured filesystem safety waterline ({available} available, {required} required after the write)"
                            ),
                            available_disk_bytes: Some(available),
                            required_free_disk_bytes: Some(required),
                        }));
                    }
                }
                Err(err) => {
                    return Ok(Some(CommandTaskOutputFailure {
                        code: CommandTaskOutputFailureCode::OutputPersistenceFailed,
                        message: format!("failed to inspect command output filesystem: {err:#}"),
                        available_disk_bytes: None,
                        required_free_disk_bytes: None,
                    }));
                }
            }
        }

        push_rolling_bytes(&mut self.rolling_tail, bytes, self.tail_limit);
        if self.truncated {
            return Ok(None);
        }

        let write_len = write_len as usize;
        if write_len > 0 {
            if let Err(err) = self.file.write_all(&bytes[..write_len]).await {
                return Err(err);
            }
            self.retained_bytes = self.retained_bytes.saturating_add(write_len as u64);
        }
        if write_len < bytes.len() {
            self.activate_truncation().await?;
        }
        Ok(None)
    }

    async fn seed_capture(
        &mut self,
        system: &crate::system::LocalSystem,
        captured: &CapturedOutput,
    ) -> std::result::Result<Option<CommandTaskOutputFailure>, std::io::Error> {
        if captured.raw_retained.is_empty() {
            return Ok(None);
        }
        if let Some(failure) = self
            .write_chunk(system, &captured.raw_retained, captured.emitted_bytes)
            .await?
        {
            return Ok(Some(failure));
        }
        if captured.raw_truncated {
            self.rolling_tail.clone_from(&captured.raw_tail);
            self.activate_truncation().await?;
        }
        Ok(None)
    }

    async fn activate_truncation(&mut self) -> std::io::Result<()> {
        self.truncated = true;
        self.file.set_len(self.head_limit).await?;
        self.retained_bytes = self.head_limit;
        self.file.seek(SeekFrom::Start(self.head_limit)).await?;
        self.file
            .write_all(b"\n\n[holon: command output truncated; final dropped_bytes are recorded in task metadata]\n\n")
            .await?;
        Ok(())
    }

    async fn finalize(
        &mut self,
        emitted_bytes: u64,
        allow_tail_rewrite: bool,
    ) -> std::io::Result<(u64, bool)> {
        if self.truncated && allow_tail_rewrite {
            let retained_bytes = self
                .head_limit
                .saturating_add(self.rolling_tail.len() as u64);
            let dropped_bytes = emitted_bytes.saturating_sub(retained_bytes);
            let marker = format!(
                "\n\n[holon: command output truncated; dropped_bytes={dropped_bytes}; retained bounded head and tail]\n\n"
            );
            self.file.set_len(self.head_limit).await?;
            self.file.seek(SeekFrom::Start(self.head_limit)).await?;
            self.file.write_all(marker.as_bytes()).await?;
            self.file.write_all(&self.rolling_tail).await?;
            self.file
                .set_len(
                    self.head_limit
                        .saturating_add(marker.len() as u64)
                        .saturating_add(self.rolling_tail.len() as u64),
                )
                .await?;
            self.file.flush().await?;
            return Ok((retained_bytes, true));
        }
        self.file.flush().await?;
        Ok((self.retained_bytes, self.truncated))
    }
}

fn persisted_tail_limit(retention_bytes: u64) -> usize {
    let quarter = usize::try_from(retention_bytes / 4).unwrap_or(usize::MAX);
    PERSISTED_TAIL_BYTE_LIMIT.min(quarter).max(1)
}

fn persisted_head_limit(retention_bytes: u64) -> usize {
    let tail_limit = persisted_tail_limit(retention_bytes) as u64;
    usize::try_from(
        retention_bytes
            .saturating_sub(tail_limit)
            .saturating_sub(PERSISTED_TRUNCATION_MARKER_RESERVE),
    )
    .unwrap_or(usize::MAX)
}

fn required_free_disk_bytes(policy: CommandTaskOutputPolicy, total_bytes: u64) -> u64 {
    let percent_bytes = total_bytes
        .saturating_mul(policy.min_free_disk_percent as u64)
        .saturating_add(99)
        / 100;
    policy.min_free_disk_bytes.max(percent_bytes)
}

fn would_cross_disk_waterline(
    available_bytes: u64,
    required_free_bytes: u64,
    planned_write_bytes: u64,
) -> bool {
    available_bytes.saturating_sub(planned_write_bytes) < required_free_bytes
}

fn push_rolling_bytes(buffer: &mut Vec<u8>, bytes: &[u8], limit: usize) {
    if bytes.len() >= limit {
        buffer.clear();
        buffer.extend_from_slice(&bytes[bytes.len() - limit..]);
        return;
    }
    let overflow = buffer
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(limit);
    if overflow > 0 {
        buffer.drain(..overflow);
    }
    buffer.extend_from_slice(bytes);
}

fn classify_output_write_error(error: &std::io::Error) -> CommandTaskOutputFailure {
    let low_disk = error.raw_os_error() == Some(libc::ENOSPC);
    CommandTaskOutputFailure {
        code: if low_disk {
            CommandTaskOutputFailureCode::LowDiskSpace
        } else {
            CommandTaskOutputFailureCode::OutputPersistenceFailed
        },
        message: if low_disk {
            format!("command output persistence failed because the filesystem is full: {error}")
        } else {
            format!("command output persistence failed: {error}")
        },
        available_disk_bytes: None,
        required_free_disk_bytes: None,
    }
}

fn classify_output_open_error(error: &anyhow::Error) -> CommandTaskOutputFailure {
    if let Some(io_error) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
    {
        return classify_output_write_error(io_error);
    }
    CommandTaskOutputFailure {
        code: CommandTaskOutputFailureCode::OutputPersistenceFailed,
        message: format!("command output persistence failed: {error:#}"),
        available_disk_bytes: None,
        required_free_disk_bytes: None,
    }
}

fn output_quota_failure(
    policy: CommandTaskOutputPolicy,
    emitted_bytes: u64,
) -> Option<CommandTaskOutputFailure> {
    (emitted_bytes > policy.execution_quota_bytes).then(|| CommandTaskOutputFailure {
        code: CommandTaskOutputFailureCode::OutputLimitExceeded,
        message: format!(
            "command emitted {emitted_bytes} bytes, exceeding the configured execution output quota of {} bytes",
            policy.execution_quota_bytes
        ),
        available_disk_bytes: None,
        required_free_disk_bytes: None,
    })
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
        authority_class: AuthorityClass,
    ) -> Result<TaskRecord> {
        self.ensure_background_tasks_allowed("command_task").await?;
        self.ensure_process_execution_exposed("command_task")
            .await?;

        let resolved = self.resolve_command_task(&spec).await?;
        let running = self
            .start_command_process(&resolved, None, crate::tool::names::EXEC_COMMAND)
            .await?;
        let captured = CapturedOutput::new(resolved.output_policy);
        self.register_command_task(summary, resolved, running, authority_class, false, captured)
            .await
    }

    pub(crate) async fn execute_exec_command(
        &self,
        mut spec: CommandTaskSpec,
        duplicate_policy: ExecCommandDuplicatePolicy,
        authority_class: &AuthorityClass,
        trace_context: Option<&crate::observability::TraceContext>,
    ) -> Result<ExecCommandResult> {
        self.ensure_process_execution_exposed(crate::tool::names::EXEC_COMMAND)
            .await?;
        self.apply_command_output_policy(&mut spec);
        let diagnostics = self.command_cost_diagnostics_for(&spec);
        let resolved = self.resolve_command_task(&spec).await?;
        if matches!(duplicate_policy, ExecCommandDuplicatePolicy::ReuseRunning) {
            if let Some(existing) = self
                .find_reusable_active_command_task(&resolved.spec, &resolved.workdir)
                .await?
            {
                return Ok(ExecCommandResult {
                    outcome: ExecCommandOutcome::AlreadyRunning {
                        task_handle: TaskHandle::new(
                            existing.id.clone(),
                            existing.kind.clone(),
                            existing.status.clone(),
                            None,
                        ),
                        command: existing.command,
                        summary: existing.summary,
                        instructions: Some(
                            "Set duplicate_policy=\"start_new\" to start another instance.".into(),
                        ),
                    },
                    summary_text: Some(format!(
                        "command already running as {} (status {:?})",
                        existing.id, existing.status
                    )),
                    command_diagnostics: Some(diagnostics),
                });
            }
        }
        let mut running = self
            .start_command_process(&resolved, trace_context, crate::tool::names::EXEC_COMMAND)
            .await?;
        let mut captured = CapturedOutput::new(resolved.output_policy);
        if spec.yield_time_ms == 0 {
            return self
                .promote_exec_command_to_task(
                    spec,
                    resolved,
                    running,
                    authority_class.clone(),
                    captured,
                    diagnostics,
                )
                .await;
        }
        let sleep = tokio::time::sleep(Duration::from_millis(spec.yield_time_ms));
        let mut status_tick = tokio::time::interval(PROCESS_STATUS_POLL_INTERVAL);
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(sleep);

        loop {
            tokio::select! {
                chunk = running.output_rx.recv() => {
                    if let Some(chunk) = chunk {
                        captured.push(&chunk);
                    }
                }
                _ = status_tick.tick() => {
                    if let Some(status) = running
                        .process
                        .try_status()
                        .await
                        .context("failed to query command status")?
                    {
                        let process_completed_at = chrono::Utc::now();
                        collect_remaining_output(&mut running, &mut captured).await;
                        record_command_trace(
                            &mut running,
                            process_completed_at,
                            chrono::Utc::now(),
                            if status.success() {
                                crate::observability::TraceSpanStatus::Ok
                            } else {
                                crate::observability::TraceSpanStatus::Error
                            },
                            if status.success() { "completed" } else { "failed" },
                        );
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
                    if spec.yield_time_ms > 0 {
                        if let Some(status) = running
                            .process
                            .try_status()
                            .await
                            .context("failed to query command status")?
                        {
                            let process_completed_at = chrono::Utc::now();
                            collect_remaining_output(&mut running, &mut captured).await;
                            record_command_trace(
                                &mut running,
                                process_completed_at,
                                chrono::Utc::now(),
                                if status.success() {
                                    crate::observability::TraceSpanStatus::Ok
                                } else {
                                    crate::observability::TraceSpanStatus::Error
                                },
                                if status.success() { "completed" } else { "failed" },
                            );
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
                    return self
                        .promote_exec_command_to_task(
                            spec,
                            resolved,
                            running,
                            authority_class.clone(),
                            captured,
                            diagnostics,
                        )
                        .await;
                }
            }
        }
    }

    async fn promote_exec_command_to_task(
        &self,
        spec: CommandTaskSpec,
        resolved: ResolvedCommandTask,
        running: RunningCommand,
        authority_class: AuthorityClass,
        captured: CapturedOutput,
        diagnostics: CommandCostDiagnostics,
    ) -> Result<ExecCommandResult> {
        let task = self
            .register_command_task(
                format!("Run command: {}", truncate_text(&spec.cmd, 80)),
                resolved,
                running,
                authority_class,
                true,
                captured.clone(),
            )
            .await?;
        let (initial_output_preview, initial_output_truncated) =
            captured.initial_output_with_flag(spec.max_output_tokens);
        Ok(ExecCommandResult {
            outcome: ExecCommandOutcome::PromotedToTask {
                task_handle: TaskHandle::from_task_record(&task, None),
                initial_output_preview,
                initial_output_truncated,
            },
            summary_text: Some("command promoted to a managed task".to_string()),
            command_diagnostics: Some(diagnostics),
        })
    }

    pub(crate) async fn execute_exec_command_once(
        &self,
        mut spec: CommandTaskSpec,
        _authority_class: &AuthorityClass,
        trace_context: Option<&crate::observability::TraceContext>,
    ) -> Result<ExecCommandResult> {
        self.ensure_process_execution_exposed(crate::tool::names::EXEC_COMMAND_BATCH)
            .await?;
        self.apply_command_output_policy(&mut spec);
        let diagnostics = self.command_cost_diagnostics_for(&spec);
        let resolved = self.resolve_command_task(&spec).await?;
        let mut captured = CapturedOutput::new(resolved.output_policy);
        let mut running = self
            .start_command_process(
                &resolved,
                trace_context,
                crate::tool::names::EXEC_COMMAND_BATCH,
            )
            .await?;
        let sleep = tokio::time::sleep(Duration::from_millis(resolved.spec.yield_time_ms));
        let mut status_tick = tokio::time::interval(PROCESS_STATUS_POLL_INTERVAL);
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(sleep);

        loop {
            tokio::select! {
                chunk = running.output_rx.recv() => {
                    if let Some(chunk) = chunk {
                        captured.push(&chunk);
                    }
                }
                _ = status_tick.tick() => {
                    if let Some(status) = running
                        .process
                        .try_status()
                        .await
                        .context("failed to query command status")?
                    {
                        let process_completed_at = chrono::Utc::now();
                        collect_remaining_output(&mut running, &mut captured).await;
                        record_command_trace(
                            &mut running,
                            process_completed_at,
                            chrono::Utc::now(),
                            if status.success() {
                                crate::observability::TraceSpanStatus::Ok
                            } else {
                                crate::observability::TraceSpanStatus::Error
                            },
                            if status.success() { "completed" } else { "failed" },
                        );
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
                    let process_completed_at = chrono::Utc::now();
                    collect_remaining_output(&mut running, &mut captured).await;
                    record_command_trace(
                        &mut running,
                        process_completed_at,
                        chrono::Utc::now(),
                        crate::observability::TraceSpanStatus::Error,
                        "timed_out",
                    );
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
                    .with_recovery_hint("increase the item or top-level `yield_time_ms`, narrow the command, or call ExecCommand directly when background task promotion is needed; ExecCommandBatch does not promote timed-out items")
                    .with_retryable(false)
                    .into());
                }
            }
        }
    }

    fn command_entry_matches_identity(
        &self,
        command: &CommandTaskStatusSnapshot,
        spec: &CommandTaskSpec,
        workdir: &PathBuf,
    ) -> bool {
        command.cmd.as_deref() == Some(spec.cmd.as_str())
            && command.workdir.as_deref() == Some(workdir.to_string_lossy().as_ref())
            && command.shell.as_deref() == spec.shell.as_deref()
            && command.login == Some(spec.login)
            && command.tty == Some(spec.tty)
    }

    async fn find_reusable_active_command_task(
        &self,
        spec: &CommandTaskSpec,
        workdir: &PathBuf,
    ) -> Result<Option<CommandTaskMatch>> {
        let mut entries = self.managed_tasks().latest_task_list_entries().await?;
        let found = entries
            .drain(..)
            .find(|entry| {
                entry.kind == TaskKind::CommandTask.as_str()
                    && entry.command.as_ref().is_some_and(|command| {
                        self.command_entry_matches_identity(command, spec, workdir)
                    })
            })
            .map(|entry| CommandTaskMatch {
                id: entry.id,
                kind: entry.kind,
                status: entry.status,
                summary: entry.summary,
                command: entry.command,
            });
        Ok(found)
    }

    fn apply_command_output_policy(&self, spec: &mut CommandTaskSpec) {
        let snap = self.inner.config_snapshot.load();
        let effective = effective_tool_output_tokens(
            spec.max_output_tokens,
            snap.default_tool_output_tokens,
            snap.max_tool_output_tokens,
        );
        spec.max_output_tokens = Some(effective);
    }

    fn command_cost_diagnostics_for(&self, spec: &CommandTaskSpec) -> CommandCostDiagnostics {
        command_cost_diagnostics(
            &spec.cmd,
            spec.max_output_tokens.unwrap_or_else(|| {
                let snap = self.inner.config_snapshot.load();
                effective_tool_output_tokens(
                    None,
                    snap.default_tool_output_tokens,
                    snap.max_tool_output_tokens,
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
            .map(|value| view.resolve_read_path(value))
            .transpose()?
            .unwrap_or_else(|| view.cwd().to_path_buf());

        // Fail early with a clear error if the workdir does not exist.
        // Without this check, the error surfaces later as a generic
        // "command_spawn_failed" with a misleading shell-related recovery hint.
        if !workdir.exists() {
            return Err(ToolError::new(
                "workdir_not_found",
                format!("workdir does not exist: {}", workdir.display()),
            )
            .with_details(json!({ "workdir": workdir.display().to_string() }))
            .with_recovery_hint(
                "use an existing directory for `workdir`, or omit it to use the workspace cwd",
            )
            .with_retryable(false)
            .into());
        }

        let agent_id = self.agent_id().await?;
        let mut env = vec![
            ("HOLON_RUNTIME".to_string(), "1".to_string()),
            ("HOLON_AGENT_ID".to_string(), agent_id.clone()),
            (
                "HOLON_AGENT_HOME".to_string(),
                self.agent_home().to_string_lossy().into_owned(),
            ),
        ];
        let state = self.agent_state().await?;
        if let Some(binding) = state.current_execution_binding.as_ref() {
            env.push((HOLON_CALLER_AGENT_ID_ENV.to_string(), agent_id.clone()));
            env.push((
                HOLON_CALLER_SOURCE_TURN_ID_ENV.to_string(),
                binding.turn_id.clone(),
            ));
            if let Some(work_item_id) = binding.work_item_id.as_ref() {
                env.push((
                    HOLON_CALLER_SOURCE_WORK_ITEM_ID_ENV.to_string(),
                    work_item_id.clone(),
                ));
            }
            if let Some(activation_id) = binding.activation_id.as_ref() {
                env.push((
                    HOLON_CALLER_SOURCE_ACTIVATION_ID_ENV.to_string(),
                    activation_id.clone(),
                ));
            }
            if let Some(message) = self
                .storage()
                .read_message_by_id(&binding.source_message_id)?
            {
                if let Some(task_id) = message.task_id {
                    env.push((HOLON_CALLER_SOURCE_TASK_ID_ENV.to_string(), task_id));
                }
                env.push((
                    HOLON_CALLER_AUTHORITY_CLASS_ENV.to_string(),
                    serde_json::to_string(&message.authority_class)?
                        .trim_matches('"')
                        .to_string(),
                ));
            }
        }
        if let Some(trigger_url) = self.command_external_trigger_url(&agent_id).await? {
            env.push(("HOLON_EXTERNAL_TRIGGER_URL".to_string(), trigger_url));
        }

        Ok(ResolvedCommandTask {
            spec: spec.clone(),
            workdir,
            output_path: PathBuf::new(),
            execution: execution_snapshot,
            env,
            output_policy: self.inner.config_snapshot.load().command_task_output_policy,
        })
    }

    async fn command_external_trigger_url(&self, agent_id: &str) -> Result<Option<String>> {
        Ok(self
            .latest_external_triggers()
            .await?
            .into_iter()
            .find_map(|trigger| {
                (trigger.target_agent_id == agent_id
                    && trigger.scope == ExternalTriggerScope::Agent
                    && trigger.status == ExternalTriggerStatus::Active)
                    .then_some(())
                    .and_then(|_| {
                        trigger.token.as_ref().map(|token| {
                            crate::callbacks::build_callback_url(
                                &self.inner.callback_base_url,
                                &trigger.delivery_mode,
                                token,
                            )
                        })
                    })
            }))
    }

    async fn register_command_task(
        &self,
        summary: String,
        mut resolved: ResolvedCommandTask,
        running: RunningCommand,
        authority_class: AuthorityClass,
        promoted_from_exec_command: bool,
        mut initial_capture: CapturedOutput,
    ) -> Result<TaskRecord> {
        let agent_id = self.agent_id().await?;
        let task_id = crate::ids::task_id();
        resolved.output_path = self.command_task_output_path(&task_id)?;
        if initial_capture.retention_limit_bytes == 0 {
            initial_capture.retention_limit_bytes = resolved.output_policy.retention_bytes;
            initial_capture.execution_quota_bytes = resolved.output_policy.execution_quota_bytes;
        }
        let (input_tx, input_rx) = mpsc::channel(INPUT_CHANNEL_CAPACITY);
        let detail = command_task_detail(
            &resolved,
            promoted_from_exec_command,
            &initial_capture,
            None,
            None,
            false,
        );
        let detail = self.task_creation_detail(&task_id, detail).await?;
        let diagnostics = self.command_cost_diagnostics_for(&resolved.spec);
        let work_item_id = self.task_work_item_binding().await;
        let task = TaskRecord {
            id: task_id.clone(),
            agent_id: agent_id.clone(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Queued,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            parent_message_id: None,
            work_item_id,
            summary: Some(summary.clone()),
            detail: Some(detail),
            recovery: Some(TaskRecoverySpec::CommandTask {
                summary,
                spec: resolved.spec.clone(),
                authority_class: authority_class.clone(),
                promoted_from_exec_command,
            }),
        };
        self.append_audit_event(
            "process_execution_requested",
            serde_json::json!({
                "surface": "command_task",
                "task_id": task_id,
                "authority_class": authority_class,
                "cmd_preview": diagnostics.cmd_preview.clone(),
                "cmd_display": command_display(&resolved.spec.cmd),
                "command_cost": diagnostics,
                "execution": resolved.execution.clone(),
                "boundary": crate::system::HostLocalBoundary::from_snapshot(&resolved.execution).audit_metadata(),
                "workdir": resolved.workdir.clone(),
                "promoted_from_exec_command": promoted_from_exec_command,
            }),
        )?;
        self.apply_task_transition(task_state_reducer::TaskTransition::new(
            &task,
            "task_created",
        ))
        .await?;

        let (cancel_tx, cancel_rx) = oneshot::channel();
        let (force_stop_tx, force_stop_rx) = oneshot::channel();
        self.inner.task_handles.lock().await.insert(
            task.id.clone(),
            ManagedTaskHandle::Command(CommandTaskHandle {
                cancel_tx: Some(cancel_tx),
                force_stop_tx: Some(force_stop_tx),
                input_tx,
            }),
        );
        let runtime = self.clone();
        let task_record = task.clone();
        let task_record_for_error = task.clone();
        let resolved_for_error = resolved.clone();
        tokio::spawn(async move {
            if let Err(err) = runtime
                .run_command_task(
                    task_record,
                    resolved,
                    running,
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
                    .append_event(&crate::types::AuditEvent::legacy(
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
                        None,
                        None,
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

        Ok(task)
    }

    async fn run_command_task(
        &self,
        task_record: TaskRecord,
        resolved: ResolvedCommandTask,
        mut running: RunningCommand,
        mut cancel_rx: oneshot::Receiver<()>,
        mut force_stop_rx: oneshot::Receiver<()>,
        mut input_rx: mpsc::Receiver<CommandTaskInputRequest>,
        promoted_from_exec_command: bool,
        initial_capture: CapturedOutput,
    ) -> Result<()> {
        let mut captured = initial_capture;
        let terminal = match self
            .run_command_task_inner(
                &task_record,
                &resolved,
                &mut running,
                &mut cancel_rx,
                &mut force_stop_rx,
                &mut input_rx,
                promoted_from_exec_command,
                &mut captured,
            )
            .await
        {
            Ok(outcome) => {
                let status = if outcome.cancelled {
                    TaskStatus::Cancelled
                } else if outcome.output_failure.is_some() {
                    TaskStatus::Failed
                } else if outcome.exit_status.success() {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Failed
                };
                record_command_trace(
                    &mut running,
                    outcome.process_completed_at,
                    outcome.output_completed_at,
                    if status == TaskStatus::Completed {
                        crate::observability::TraceSpanStatus::Ok
                    } else {
                        crate::observability::TraceSpanStatus::Error
                    },
                    task_status_label(&status),
                );
                CommandTaskTerminal {
                    status,
                    exit_status: outcome.exit_status.code(),
                    error: outcome
                        .output_failure
                        .as_ref()
                        .map(|failure| failure.message.clone()),
                    cancel_requested: outcome.cancel_requested,
                    force_stop_requested: outcome.force_stop_requested,
                }
            }
            Err(err) => {
                let _ = running.process.stop(StopSignal::Kill).await;
                let _ = running.process.wait().await;
                let process_completed_at = chrono::Utc::now();
                collect_remaining_output(&mut running, &mut captured).await;
                record_command_trace(
                    &mut running,
                    process_completed_at,
                    chrono::Utc::now(),
                    crate::observability::TraceSpanStatus::Error,
                    "failed",
                );
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
        let result_turn_id = crate::ids::turn_id();
        if let Some(detail) = detail.as_object_mut() {
            detail.insert(
                "parent_turn_id".to_string(),
                serde_json::json!(result_turn_id.clone()),
            );
        }
        let result_text = build_command_task_result_text(
            task_record.summary.as_deref().unwrap_or(&resolved.spec.cmd),
            &resolved.output_path,
            status_label,
            terminal.exit_status,
            captured.summary(resolved.spec.max_output_tokens),
            terminal.error.as_deref(),
        );
        let result_message = MessageEnvelope {
            turn_id: Some(result_turn_id),
            metadata: Some({
                serde_json::json!({
                    "task_id": task_record.id,
                    "task_kind": task_record.kind,
                    "task_status": status_label,
                    "task_summary": task_record.summary,
                    "task_detail": detail.clone(),
                    "task_recovery": task_record.recovery,
                    "work_item_id": task_record.work_item_id.clone(),
                })
            }),
            ..MessageEnvelope::new(
                task_record.agent_id.clone(),
                MessageKind::TaskResult,
                MessageOrigin::Task {
                    task_id: task_record.id.clone(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Next,
                MessageBody::Text { text: result_text },
            )
            .with_admission(
                crate::types::MessageDeliverySurface::TaskRejoin,
                crate::types::AdmissionContext::RuntimeOwned,
            )
        };
        self.persist_command_task_terminal_state(
            &task_record,
            terminal.status.clone(),
            detail.clone(),
            Some(&result_message.id),
            Some(&result_message),
        )
        .await?;

        self.inner.task_handles.lock().await.remove(&task_record.id);
        Ok(())
    }

    async fn run_command_task_inner(
        &self,
        task_record: &TaskRecord,
        resolved: &ResolvedCommandTask,
        running: &mut RunningCommand,
        cancel_rx: &mut oneshot::Receiver<()>,
        force_stop_rx: &mut oneshot::Receiver<()>,
        input_rx: &mut mpsc::Receiver<CommandTaskInputRequest>,
        promoted_from_exec_command: bool,
        captured: &mut CapturedOutput,
    ) -> Result<CommandTaskRunOutcome> {
        let system = self.system();
        let mut output_failure = None;
        let mut output = match BoundedOutputFile::open(
            system.as_ref(),
            resolved.output_path.clone(),
            resolved.output_policy,
        )
        .await
        {
            Ok(output) => Some(output),
            Err(err) => {
                output_failure = Some(classify_output_open_error(&err));
                None
            }
        };
        if output_failure.is_none() {
            if let Some(output) = output.as_mut() {
                output_failure = match output.seed_capture(system.as_ref(), captured).await {
                    Ok(failure) => failure,
                    Err(err) => Some(classify_output_write_error(&err)),
                };
            }
        }
        if output_failure.is_none() {
            output_failure = output_quota_failure(resolved.output_policy, captured.emitted_bytes);
        }
        if let Some(failure) = output_failure.clone() {
            captured.fail(failure);
            let _ = running.process.stop(StopSignal::Kill).await;
        }
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
            let running_task = TaskRecord {
                id: task_record.id.clone(),
                agent_id: task_record.agent_id.clone(),
                kind: task_record.kind.clone(),
                status: TaskStatus::Running,
                created_at: task_record.created_at,
                updated_at: chrono::Utc::now(),
                parent_message_id: None,
                work_item_id: task_record.work_item_id.clone(),
                summary: task_record.summary.clone(),
                detail: Some(self.task_detail_preserving_rejoin_contract(
                    task_record,
                    command_task_detail(
                        resolved,
                        promoted_from_exec_command,
                        captured,
                        None,
                        None,
                        false,
                    ),
                )),
                recovery: task_record.recovery.clone(),
            };
            self.apply_task_transition(task_state_reducer::TaskTransition::new(
                &running_task,
                "task_status_updated",
            ))
            .await?;
        }

        let mut cancelled = false;
        let mut cancellation_requested = false;
        let mut force_stop_requested = false;
        let mut output_stop_requested = output_failure.is_some();
        let mut output_closed = false;
        let mut status_tick = tokio::time::interval(PROCESS_STATUS_POLL_INTERVAL);
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let exit_status;
        let process_completed_at;
        loop {
            tokio::select! {
                chunk = running.output_rx.recv(), if !output_closed => {
                    match chunk {
                        Some(chunk) => {
                            captured.push(&chunk);
                            if output_failure.is_none() {
                                if let Some(output) = output.as_mut() {
                                    output_failure = match output
                                        .write_chunk(
                                            system.as_ref(),
                                            &chunk.bytes,
                                            captured.emitted_bytes,
                                        )
                                        .await
                                    {
                                        Ok(failure) => failure,
                                        Err(err) => Some(classify_output_write_error(&err)),
                                    };
                                }
                            }
                            if output_failure.is_none() {
                                output_failure = output_quota_failure(
                                    resolved.output_policy,
                                    captured.emitted_bytes,
                                );
                            }
                            if let Some(failure) = output_failure.clone() {
                                captured.fail(failure);
                                if !output_stop_requested {
                                    output_stop_requested = true;
                                    let _ = running.process.stop(StopSignal::Kill).await;
                                }
                            }
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
                        process_completed_at = chrono::Utc::now();
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

        collect_remaining_output_into_file(
            system.as_ref(),
            resolved.output_policy,
            running,
            captured,
            &mut output,
            &mut output_failure,
        )
        .await;
        let output_completed_at = chrono::Utc::now();
        let allow_tail_rewrite = output_failure.as_ref().is_none_or(|failure| {
            failure.code == CommandTaskOutputFailureCode::OutputLimitExceeded
        });
        let (retained_bytes, truncated) = if let Some(output) = output.as_mut() {
            match output
                .finalize(captured.emitted_bytes, allow_tail_rewrite)
                .await
            {
                Ok(result) => result,
                Err(err) => {
                    if output_failure.is_none() {
                        output_failure = Some(classify_output_write_error(&err));
                    }
                    (output.retained_bytes, output.truncated)
                }
            }
        } else {
            (0, captured.emitted_bytes > 0)
        };
        captured.apply_persistence(retained_bytes, truncated);
        if let Some(failure) = output_failure.clone() {
            captured.fail(failure);
        }
        Ok(CommandTaskRunOutcome {
            cancelled,
            cancel_requested: cancellation_requested,
            force_stop_requested,
            exit_status,
            process_completed_at,
            output_completed_at,
            output_failure,
        })
    }

    async fn persist_command_task_terminal_state(
        &self,
        task_record: &TaskRecord,
        status: TaskStatus,
        detail: serde_json::Value,
        parent_message_id: Option<&str>,
        message_evidence: Option<&MessageEnvelope>,
    ) -> Result<()> {
        let fallback = TaskRecord {
            id: task_record.id.clone(),
            agent_id: task_record.agent_id.clone(),
            kind: task_record.kind.clone(),
            status,
            created_at: task_record.created_at,
            updated_at: chrono::Utc::now(),
            parent_message_id: parent_message_id.map(ToString::to_string),
            work_item_id: task_record.work_item_id.clone(),
            summary: task_record.summary.clone(),
            detail: Some(self.task_detail_preserving_rejoin_contract(task_record, detail)),
            recovery: task_record.recovery.clone(),
        };
        if let Some(message_evidence) = message_evidence {
            self.commit_terminal_task_result(
                &fallback,
                "command_task_terminal_persisted",
                message_evidence,
            )
            .await?;
        } else {
            self.apply_task_transition_silent(task_state_reducer::TaskTransition::new(
                &fallback,
                "command_task_terminal_persisted",
            ))
            .await?;
        }
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

    pub(crate) async fn persist_tool_text_artifact(
        &self,
        label: &str,
        content: &str,
    ) -> Result<String> {
        let dir = self.tool_artifact_dir();
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let path = dir.join(format!("{label}-{}.log", Uuid::new_v4().simple()));
        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("failed to persist {}", path.display()))?;
        Ok(path.display().to_string())
    }

    async fn persist_exec_command_artifact(&self, stream: &str, content: &str) -> Result<String> {
        self.persist_tool_text_artifact(&format!("exec-command-{stream}"), content)
            .await
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
        let stdout = stdout_raw;
        let stderr = stderr_raw;
        let non_empty_streams = usize::from(!stdout.is_empty()) + usize::from(!stderr.is_empty());
        let stream_count = non_empty_streams.max(1);
        let char_budget = output_char_budget(max_output_tokens.map(|value| value as usize));
        let per_stream_budget = char_budget / stream_count;
        let (stdout_preview, stdout_truncated) = if stdout.is_empty() {
            (None, false)
        } else {
            let value = stdout.chars().take(per_stream_budget).collect::<String>();
            let truncated = value.len() < stdout.len();
            (Some(value), truncated)
        };
        let (stderr_preview, stderr_truncated) = if stderr.is_empty() {
            (None, false)
        } else {
            let value = stderr.chars().take(per_stream_budget).collect::<String>();
            let truncated = value.len() < stderr.len();
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
        trace_context: Option<&crate::observability::TraceContext>,
        tool_name: &str,
    ) -> Result<RunningCommand> {
        let started_at = chrono::Utc::now();
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
                    env: resolved.env.clone(),
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
                        "ensure the `shell` binary exists and the `workdir` directory is valid, or omit `shell`/`workdir` to use defaults",
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
            trace: trace_context.map(|parent| RunningCommandTrace {
                child_process_context: parent.child(),
                output_collect_context: parent.child(),
                parent_span_id: parent.span_id.clone(),
                started_at,
                tool_name: tool_name.to_string(),
            }),
        })
    }
}

async fn read_output<R>(mut reader: R, stream: OutputStream, tx: mpsc::Sender<OutputChunk>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut buffer = [0u8; 4096];
    let mut decoder = IncrementalUtf8LossyDecoder::new();
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let text = decoder.push(&buffer[..read]);
                if tx
                    .send(OutputChunk {
                        stream,
                        text,
                        bytes: buffer[..read].to_vec(),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = decoder.finish();
    if !text.is_empty() {
        let _ = tx
            .send(OutputChunk {
                stream,
                text,
                bytes: Vec::new(),
            })
            .await;
    }
}

async fn collect_remaining_output(running: &mut RunningCommand, captured: &mut CapturedOutput) {
    let drain = async {
        while let Some(chunk) = running.output_rx.recv().await {
            captured.push(&chunk);
        }
    };
    // After the main process exits, background children may hold pipe write-ends
    // open indefinitely.  Cap the drain so the agent cannot block forever.
    if tokio::time::timeout(COLLECT_OUTPUT_DRAIN_TIMEOUT, drain)
        .await
        .is_ok()
    {
        for handle in running.reader_handles.drain(..) {
            let _ = handle.await;
        }
    } else {
        for handle in running.reader_handles.drain(..) {
            handle.abort();
        }
    }
}

fn record_command_trace(
    running: &mut RunningCommand,
    process_completed_at: chrono::DateTime<chrono::Utc>,
    output_completed_at: chrono::DateTime<chrono::Utc>,
    status: crate::observability::TraceSpanStatus,
    outcome: &str,
) {
    let Some(trace) = running.trace.take() else {
        return;
    };
    let attributes = crate::observability::TraceAttributes {
        tool_name: Some(trace.tool_name),
        outcome: Some(outcome.to_string()),
        ..Default::default()
    };
    crate::observability::record_span(
        &trace.child_process_context,
        crate::observability::completed_span_at(
            "holon.tool.child_process",
            &trace.child_process_context,
            Some(trace.parent_span_id.clone()),
            trace.started_at,
            process_completed_at,
            status,
            attributes.clone(),
        ),
    );
    crate::observability::record_span(
        &trace.output_collect_context,
        crate::observability::completed_span_at(
            "holon.tool.output_collect",
            &trace.output_collect_context,
            Some(trace.parent_span_id),
            trace.started_at,
            output_completed_at,
            status,
            attributes,
        ),
    );
}

async fn collect_remaining_output_into_file(
    system: &crate::system::LocalSystem,
    policy: CommandTaskOutputPolicy,
    running: &mut RunningCommand,
    captured: &mut CapturedOutput,
    output: &mut Option<BoundedOutputFile>,
    output_failure: &mut Option<CommandTaskOutputFailure>,
) {
    let drain = async {
        while let Some(chunk) = running.output_rx.recv().await {
            captured.push(&chunk);
            if output_failure.is_none() {
                if let Some(output) = output.as_mut() {
                    *output_failure = match output
                        .write_chunk(system, &chunk.bytes, captured.emitted_bytes)
                        .await
                    {
                        Ok(failure) => failure,
                        Err(err) => Some(classify_output_write_error(&err)),
                    };
                }
            }
            if output_failure.is_none() {
                *output_failure = output_quota_failure(policy, captured.emitted_bytes);
            }
            if let Some(failure) = output_failure.clone() {
                captured.fail(failure);
            }
        }
    };
    // After the main process exits, background children may hold pipe write-ends
    // open indefinitely.  Cap the drain so the agent cannot block forever.
    if tokio::time::timeout(COLLECT_OUTPUT_DRAIN_TIMEOUT, drain)
        .await
        .is_ok()
    {
        for handle in running.reader_handles.drain(..) {
            let _ = handle.await;
        }
    } else {
        for handle in running.reader_handles.drain(..) {
            handle.abort();
        }
    }
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
        "cmd_digest": command_digest(&resolved.spec.cmd),
        "wait_policy": "background",
        "workdir": resolved.workdir,
        "execution": resolved.execution,
        "shell": resolved.spec.shell,
        "login": resolved.spec.login,
        "tty": resolved.spec.tty,
        "yield_time_ms": resolved.spec.yield_time_ms,
        "max_output_tokens": resolved.spec.max_output_tokens,
        "output_policy": resolved.output_policy,
        "output_capture": captured.output_capture_snapshot(),
        "terminal_reentry": resolved.spec.terminal_reentry,
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
    use std::{collections::BTreeMap, io::Cursor, path::Path, sync::Arc};

    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::Utc;
    use tempfile::{tempdir, TempDir};
    use tokio::sync::Mutex;

    use crate::{
        context::ContextConfig,
        provider::StubProvider,
        system::{process::ProcessOutput, RunningProcess, RunningProcessExitStatus, StopSignal},
        types::{
            CallbackDeliveryMode, ExternalTriggerRecord, ExternalTriggerScope,
            ExternalTriggerStatus,
        },
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

        fn completed(code: i32) -> Self {
            Self {
                status: Arc::new(Mutex::new(Some(RunningProcessExitStatus::new(
                    Some(code),
                    None,
                )))),
                ..Self::pending()
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

    fn command_spec(accepts_input: bool, terminal_reentry: bool) -> CommandTaskSpec {
        CommandTaskSpec {
            cmd: "fake command".into(),
            workdir: None,
            shell: None,
            login: true,
            tty: false,
            yield_time_ms: 10,
            max_output_tokens: None,
            accepts_input,
            terminal_reentry,
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
                bytes: stdout.as_bytes().to_vec(),
            })
            .unwrap();
        }
        if !stderr.is_empty() {
            tx.try_send(OutputChunk {
                stream: OutputStream::Stderr,
                text: stderr.into(),
                bytes: stderr.as_bytes().to_vec(),
            })
            .unwrap();
        }
        drop(tx);

        RunningCommand {
            process: Box::new(process),
            output_rx: rx,
            reader_handles: Vec::new(),
            trace: None,
        }
    }

    fn traced_running_command(
        process: FakeRunningProcess,
        stdout: &str,
        stderr: &str,
        parent: &crate::observability::TraceContext,
        tool_name: &str,
    ) -> RunningCommand {
        let mut running = running_command(process, stdout, stderr);
        running.trace = Some(RunningCommandTrace {
            child_process_context: parent.child(),
            output_collect_context: parent.child(),
            parent_span_id: parent.span_id.clone(),
            started_at: Utc::now(),
            tool_name: tool_name.to_string(),
        });
        running
    }

    fn task_record(
        id: &str,
        status: TaskStatus,
        summary: &str,
        resolved: &ResolvedCommandTask,
        accepts_input: bool,
        terminal_reentry: bool,
    ) -> TaskRecord {
        let spec = command_spec(accepts_input, terminal_reentry);
        TaskRecord {
            id: id.into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: status.clone(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            work_item_id: None,
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
                authority_class: AuthorityClass::OperatorInstruction,
                promoted_from_exec_command: false,
            }),
        }
    }

    async fn wait_for_latest_task(
        runtime: &RuntimeHandle,
        task_id: &str,
        expected: TaskStatus,
    ) -> TaskRecord {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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

    fn resolved_env(resolved: &ResolvedCommandTask) -> BTreeMap<String, String> {
        resolved.env.iter().cloned().collect()
    }

    async fn read_output_text(bytes: Vec<u8>, stream: OutputStream) -> (String, u64) {
        let (tx, mut rx) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
        read_output(Cursor::new(bytes), stream, tx).await;
        let mut output = String::new();
        let mut emitted_bytes = 0;
        while let Some(chunk) = rx.recv().await {
            assert!(matches!(
                (chunk.stream, stream),
                (OutputStream::Stdout, OutputStream::Stdout)
                    | (OutputStream::Stderr, OutputStream::Stderr)
            ));
            output.push_str(&chunk.text);
            emitted_bytes += chunk.bytes.len() as u64;
        }
        (output, emitted_bytes)
    }

    #[test]
    fn command_trace_records_distinct_process_and_output_completion() {
        let parent = crate::observability::TraceContext::new_root(true);
        let mut running = traced_running_command(
            FakeRunningProcess::completed(0),
            "",
            "",
            &parent,
            crate::tool::names::EXEC_COMMAND,
        );
        let started_at = Utc::now();
        running.trace.as_mut().expect("trace").started_at = started_at;

        record_command_trace(
            &mut running,
            started_at + chrono::Duration::milliseconds(5),
            started_at + chrono::Duration::milliseconds(8),
            crate::observability::TraceSpanStatus::Ok,
            "completed",
        );

        let trace = crate::observability::recent_trace(&parent.trace_id).expect("trace");
        let child_process = trace
            .spans
            .iter()
            .find(|span| span.name == "holon.tool.child_process")
            .expect("child process span");
        let output_collect = trace
            .spans
            .iter()
            .find(|span| span.name == "holon.tool.output_collect")
            .expect("output collect span");
        assert_eq!(
            child_process.parent_span_id.as_deref(),
            Some(parent.span_id.as_str())
        );
        assert_eq!(
            output_collect.parent_span_id.as_deref(),
            Some(parent.span_id.as_str())
        );
        assert_ne!(child_process.span_id, output_collect.span_id);
        assert_eq!(child_process.duration_us, 5_000);
        assert_eq!(output_collect.duration_us, 8_000);
        assert_eq!(
            child_process.attributes.tool_name.as_deref(),
            Some(crate::tool::names::EXEC_COMMAND)
        );
        assert_eq!(
            output_collect.attributes.outcome.as_deref(),
            Some("completed")
        );
    }

    #[tokio::test]
    async fn command_output_preserves_utf8_across_the_4096_byte_read_boundary() {
        for stream in [OutputStream::Stdout, OutputStream::Stderr] {
            let mut bytes = vec![b'a'; 4095];
            bytes.extend_from_slice("中".as_bytes());
            let expected_emitted_bytes = bytes.len() as u64;
            let (output, emitted_bytes) = read_output_text(bytes, stream).await;

            assert_eq!(output, format!("{}中", "a".repeat(4095)));
            assert_eq!(emitted_bytes, expected_emitted_bytes);
            assert!(!output.contains('\u{FFFD}'));
        }
    }

    #[tokio::test]
    async fn bounded_output_file_retains_head_and_tail_within_combined_cap() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bounded-output.log");
        let policy = CommandTaskOutputPolicy {
            retention_bytes: 4 * 1024,
            execution_quota_bytes: 64 * 1024,
            min_free_disk_bytes: 0,
            min_free_disk_percent: 0,
        };
        let system = crate::system::LocalSystem::new();
        let stdout = "A".repeat(3_000);
        let stderr = "Z".repeat(3_000);
        let mut output = BoundedOutputFile::open(&system, path.clone(), policy)
            .await
            .unwrap();

        assert!(output
            .write_chunk(&system, stdout.as_bytes(), stdout.len() as u64)
            .await
            .unwrap()
            .is_none());
        assert!(output
            .write_chunk(
                &system,
                stderr.as_bytes(),
                (stdout.len() + stderr.len()) as u64,
            )
            .await
            .unwrap()
            .is_none());
        let decoded_bytes = (stdout.len() + stderr.len()) as u64;
        let (retained_bytes, truncated) = output.finalize(decoded_bytes, true).await.unwrap();
        let artifact = tokio::fs::read(&path).await.unwrap();
        let artifact_text = String::from_utf8(artifact.clone()).unwrap();

        assert!(truncated);
        assert!(artifact.len() as u64 <= policy.retention_bytes);
        assert!(artifact_text.starts_with(&"A".repeat(output.head_limit as usize)));
        assert!(artifact_text.ends_with(&"Z".repeat(output.tail_limit)));
        assert!(artifact_text.contains("dropped_bytes=2160"));
        assert_eq!(retained_bytes, output.head_limit + output.tail_limit as u64);
    }

    #[tokio::test]
    async fn command_task_retention_truncation_does_not_fail_execution() {
        let (_home, _workspace, runtime) = test_runtime();
        let spec = command_spec(false, false);
        let mut resolved = resolved_command(&runtime, &spec).await;
        let policy = CommandTaskOutputPolicy {
            retention_bytes: 4 * 1024,
            execution_quota_bytes: 64 * 1024,
            min_free_disk_bytes: 0,
            min_free_disk_percent: 0,
        };
        resolved.output_policy = policy;
        let stdout = "A".repeat(3_000);
        let stderr = "Z".repeat(3_000);
        let task = runtime
            .register_command_task(
                "bounded output".into(),
                resolved,
                running_command(FakeRunningProcess::completed(0), &stdout, &stderr),
                AuthorityClass::OperatorInstruction,
                false,
                CapturedOutput::new(policy),
            )
            .await
            .unwrap();

        let latest = wait_for_latest_task(&runtime, &task.id, TaskStatus::Completed).await;
        let detail = latest.detail.as_ref().expect("terminal detail");
        let capture = detail["output_capture"]
            .as_object()
            .expect("output capture");
        assert_eq!(capture["emitted_bytes"].as_u64(), Some(6_000));
        assert_eq!(capture["decoded_bytes"].as_u64(), Some(6_000));
        assert_eq!(capture["retained_bytes"].as_u64(), Some(3_840));
        assert_eq!(capture["dropped_bytes"].as_u64(), Some(2_160));
        assert_eq!(capture["truncated"].as_bool(), Some(true));
        assert!(capture.get("failure_code").is_none());

        let output_path = detail["output_path"].as_str().expect("output path");
        let artifact = tokio::fs::read(output_path).await.unwrap();
        assert!(artifact.len() as u64 <= policy.retention_bytes);
        assert!(artifact.starts_with(&"A".repeat(2_816).into_bytes()));
        assert!(artifact.ends_with(&"Z".repeat(1_024).into_bytes()));
    }

    #[tokio::test]
    async fn promoted_command_task_seeds_bounded_raw_head_and_tail() {
        let (_home, _workspace, runtime) = test_runtime();
        let spec = command_spec(false, false);
        let mut resolved = resolved_command(&runtime, &spec).await;
        let policy = CommandTaskOutputPolicy {
            retention_bytes: 4 * 1024,
            execution_quota_bytes: 64 * 1024,
            min_free_disk_bytes: 0,
            min_free_disk_percent: 0,
        };
        resolved.output_policy = policy;
        let mut captured = CapturedOutput::new(policy);
        captured.push(&OutputChunk {
            stream: OutputStream::Stdout,
            text: "A".repeat(3_000),
            bytes: vec![b'A'; 3_000],
        });
        captured.push(&OutputChunk {
            stream: OutputStream::Stderr,
            text: "Z".repeat(3_000),
            bytes: vec![b'Z'; 3_000],
        });
        let task = runtime
            .register_command_task(
                "promoted bounded output".into(),
                resolved,
                running_command(FakeRunningProcess::completed(0), "", ""),
                AuthorityClass::OperatorInstruction,
                true,
                captured,
            )
            .await
            .unwrap();

        let latest = wait_for_latest_task(&runtime, &task.id, TaskStatus::Completed).await;
        let detail = latest.detail.as_ref().expect("terminal detail");
        assert_eq!(
            detail["output_capture"]["emitted_bytes"].as_u64(),
            Some(6_000)
        );
        assert_eq!(
            detail["output_capture"]["dropped_bytes"].as_u64(),
            Some(2_160)
        );
        let output_path = detail["output_path"].as_str().expect("output path");
        let artifact = tokio::fs::read(output_path).await.unwrap();
        assert!(artifact.len() as u64 <= policy.retention_bytes);
        assert!(artifact.starts_with(&"A".repeat(2_816).into_bytes()));
        assert!(artifact.ends_with(&"Z".repeat(1_024).into_bytes()));
    }

    #[test]
    fn dropped_bytes_use_raw_emitted_bytes_not_lossy_utf8_size() {
        let policy = CommandTaskOutputPolicy {
            retention_bytes: 4 * 1024,
            execution_quota_bytes: 64 * 1024,
            min_free_disk_bytes: 0,
            min_free_disk_percent: 0,
        };
        let bytes = vec![0xff; 3];
        let mut captured = CapturedOutput::new(policy);
        captured.push(&OutputChunk {
            stream: OutputStream::Stdout,
            text: String::from_utf8_lossy(&bytes).into_owned(),
            bytes,
        });
        captured.apply_persistence(1, true);

        assert_eq!(captured.emitted_bytes, 3);
        assert_eq!(captured.decoded_bytes, 9);
        assert_eq!(captured.dropped_bytes, 2);
    }

    #[test]
    fn enospc_is_classified_as_low_disk_space() {
        let failure = classify_output_write_error(&std::io::Error::from_raw_os_error(libc::ENOSPC));

        assert_eq!(failure.code, CommandTaskOutputFailureCode::LowDiskSpace);
        assert!(failure.message.contains("filesystem is full"));
    }

    #[test]
    fn disk_waterline_accounts_for_the_current_write() {
        assert!(!would_cross_disk_waterline(1_100, 1_000, 100));
        assert!(would_cross_disk_waterline(1_100, 1_000, 101));
        assert!(would_cross_disk_waterline(500, 1_000, 0));
    }

    #[tokio::test]
    async fn command_task_execution_output_quota_is_a_typed_failure() {
        let (_home, _workspace, runtime) = test_runtime();
        let spec = command_spec(false, false);
        let mut resolved = resolved_command(&runtime, &spec).await;
        let policy = CommandTaskOutputPolicy {
            retention_bytes: 4 * 1024,
            execution_quota_bytes: 4 * 1024,
            min_free_disk_bytes: 0,
            min_free_disk_percent: 0,
        };
        resolved.output_policy = policy;
        let stdout = "Q".repeat(8 * 1024);
        let task = runtime
            .register_command_task(
                "quota output".into(),
                resolved,
                running_command(FakeRunningProcess::completed(0), &stdout, ""),
                AuthorityClass::OperatorInstruction,
                false,
                CapturedOutput::new(policy),
            )
            .await
            .unwrap();

        let latest = wait_for_latest_task(&runtime, &task.id, TaskStatus::Failed).await;
        let detail = latest.detail.as_ref().expect("terminal detail");
        assert_eq!(
            detail["output_capture"]["failure_code"].as_str(),
            Some("output_limit_exceeded")
        );
        assert_eq!(
            detail["output_capture"]["emitted_bytes"].as_u64(),
            Some(8 * 1024)
        );
        assert!(detail["error"]
            .as_str()
            .expect("typed output failure")
            .contains("execution output quota"));
    }

    #[tokio::test]
    async fn command_task_low_disk_waterline_is_a_typed_failure() {
        let (_home, _workspace, runtime) = test_runtime();
        let spec = command_spec(false, false);
        let mut resolved = resolved_command(&runtime, &spec).await;
        let policy = CommandTaskOutputPolicy {
            retention_bytes: 4 * 1024,
            execution_quota_bytes: 64 * 1024,
            min_free_disk_bytes: u64::MAX,
            min_free_disk_percent: 100,
        };
        resolved.output_policy = policy;
        let task = runtime
            .register_command_task(
                "low disk output".into(),
                resolved,
                running_command(FakeRunningProcess::pending(), "disk guarded output", ""),
                AuthorityClass::OperatorInstruction,
                false,
                CapturedOutput::new(policy),
            )
            .await
            .unwrap();

        let latest = wait_for_latest_task(&runtime, &task.id, TaskStatus::Failed).await;
        let detail = latest.detail.as_ref().expect("terminal detail");
        assert_eq!(
            detail["output_capture"]["failure_code"].as_str(),
            Some("low_disk_space")
        );
        assert_eq!(
            detail["output_capture"]["required_free_disk_bytes"].as_u64(),
            Some(u64::MAX)
        );
        assert!(detail["output_capture"]["available_disk_bytes"]
            .as_u64()
            .is_some());
    }

    #[tokio::test]
    async fn command_task_output_open_failure_still_drains_and_serves_fallback() {
        let (home, _workspace, runtime) = test_runtime();
        tokio::fs::write(home.path().join("task-output"), b"not a directory")
            .await
            .unwrap();
        let spec = command_spec(false, false);
        let resolved = resolved_command(&runtime, &spec).await;
        let task = runtime
            .register_command_task(
                "failed output open".into(),
                resolved,
                running_command(
                    FakeRunningProcess::pending(),
                    "stdout survived persistence failure",
                    "",
                ),
                AuthorityClass::OperatorInstruction,
                false,
                CapturedOutput::default(),
            )
            .await
            .unwrap();

        let latest = wait_for_latest_task(&runtime, &task.id, TaskStatus::Failed).await;
        assert_eq!(
            latest.detail.as_ref().unwrap()["output_capture"]["failure_code"].as_str(),
            Some("output_persistence_failed")
        );
        assert_eq!(
            latest.detail.as_ref().unwrap()["output_capture"]["emitted_bytes"].as_u64(),
            Some(35)
        );
        let output = runtime.task_output(&task.id, false, 0).await.unwrap();
        assert!(output
            .task
            .output_preview
            .contains("stdout survived persistence failure"));
        assert_eq!(
            output
                .task
                .output_capture
                .as_ref()
                .and_then(|capture| capture.failure_code),
            Some(CommandTaskOutputFailureCode::OutputPersistenceFailed)
        );
    }

    #[tokio::test]
    async fn command_resolution_exposes_holon_agent_environment() {
        let (home, _workspace, runtime) = test_runtime();
        let spec = command_spec(false, false);
        let resolved = resolved_command(&runtime, &spec).await;
        let env = resolved_env(&resolved);
        let expected_home = home.path().to_string_lossy().into_owned();

        assert_eq!(env.get("HOLON_RUNTIME").map(String::as_str), Some("1"));
        assert_eq!(
            env.get("HOLON_AGENT_ID").map(String::as_str),
            Some("default")
        );
        assert_eq!(env.get("HOLON_AGENT_HOME"), Some(&expected_home));
        assert!(!env.contains_key("HOLON_EXTERNAL_TRIGGER_URL"));
        assert!(!env.contains_key("HOLON_EXTERNAL_TRIGGER_SCOPE"));
        assert!(!env.contains_key("HOLON_EXTERNAL_TRIGGER_DELIVERY_MODE"));
    }

    #[tokio::test]
    async fn command_resolution_exposes_external_trigger_url_when_available() {
        let (_home, _workspace, runtime) = test_runtime();
        let capability = runtime
            .default_external_trigger(CallbackDeliveryMode::EnqueueMessage)
            .await
            .unwrap();

        let spec = command_spec(false, false);
        let resolved = resolved_command(&runtime, &spec).await;
        let env = resolved_env(&resolved);

        assert_eq!(
            env.get("HOLON_EXTERNAL_TRIGGER_URL"),
            Some(&capability.trigger_url)
        );
        assert!(!env.contains_key("HOLON_EXTERNAL_TRIGGER_SCOPE"));
        assert!(!env.contains_key("HOLON_EXTERNAL_TRIGGER_DELIVERY_MODE"));
    }

    #[tokio::test]
    async fn command_resolution_includes_agent_external_trigger_url() {
        let (_home, _workspace, runtime) = test_runtime();
        let work = runtime
            .create_work_item("wait for scoped callback".into(), None, None, Vec::new())
            .await
            .unwrap();
        runtime.pick_work_item(work.id.clone()).await.unwrap();
        runtime
            .inner
            .runtime_db
            .external_triggers()
            .upsert(&ExternalTriggerRecord {
                external_trigger_id: "legacy-work-item-trigger".into(),
                target_agent_id: "default".into(),
                scope: ExternalTriggerScope::Agent,
                delivery_mode: CallbackDeliveryMode::WakeHint,
                token: Some("token".into()),
                token_hash: "token-hash".into(),
                status: ExternalTriggerStatus::Active,
                created_at: Utc::now(),
                revoked_at: None,
                last_delivered_at: None,
                delivery_count: 0,
            })
            .unwrap();

        let spec = command_spec(false, false);
        let resolved = resolved_command(&runtime, &spec).await;
        let env = resolved_env(&resolved);

        assert_eq!(
            env.get("HOLON_EXTERNAL_TRIGGER_URL").map(String::as_str),
            Some("http://127.0.0.1:7878/api/callbacks/wake/token")
        );
    }

    #[tokio::test]
    async fn cancellation_after_partial_output_persists_terminal_detail() {
        let (_home, _workspace, runtime) = test_runtime();
        let spec = command_spec(false, false);
        let resolved = resolved_command(&runtime, &spec).await;
        let tool_context = crate::observability::TraceContext::new_root(true);
        let task = runtime
            .register_command_task(
                "cancel with output".into(),
                resolved,
                traced_running_command(
                    FakeRunningProcess::pending(),
                    "partial stdout\n",
                    "partial stderr\n",
                    &tool_context,
                    crate::tool::names::EXEC_COMMAND,
                ),
                AuthorityClass::OperatorInstruction,
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

        let trace =
            crate::observability::recent_trace(&tool_context.trace_id).expect("command trace");
        for name in ["holon.tool.child_process", "holon.tool.output_collect"] {
            let span = trace
                .spans
                .iter()
                .find(|span| span.name == name)
                .unwrap_or_else(|| panic!("missing {name} span"));
            assert_eq!(
                span.parent_span_id.as_deref(),
                Some(tool_context.span_id.as_str())
            );
            assert_eq!(span.status, crate::observability::TraceSpanStatus::Error);
            assert_eq!(span.attributes.outcome.as_deref(), Some("cancelled"));
        }
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
                AuthorityClass::OperatorInstruction,
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
                AuthorityClass::OperatorInstruction,
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
                && event.data["task_id"].as_str() == Some(task.id.as_str())
                && event.data["status"].as_str() == Some("failed")
        }));
        assert!(!events
            .iter()
            .any(|event| event.kind == "command_task_running_persisted"));
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.kind == "task_status_updated"
                        && event.data["task_id"].as_str() == Some(task.id.as_str())
                        && event.data["status"].as_str() == Some("running")
                })
                .count(),
            1
        );
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
        runtime.inner.runtime_db.tasks().upsert(&task).unwrap();

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

    #[tokio::test]
    async fn resolve_command_task_rejects_nonexistent_workdir() {
        let (_home, workspace, runtime) = test_runtime();
        let nonexistent = workspace.path().join("does").join("not").join("exist");

        let mut spec = command_spec(false, false);
        spec.workdir = Some(nonexistent.to_string_lossy().into_owned());

        let result = runtime.resolve_command_task(&spec).await;
        assert!(
            result.is_err(),
            "resolving a command task with a nonexistent workdir should fail"
        );
        let err = result.unwrap_err();
        let tool_error = crate::tool::ToolError::from_anyhow(&err);
        assert_eq!(tool_error.kind, "workdir_not_found");
        assert!(
            tool_error
                .recovery_hint
                .as_deref()
                .is_some_and(|hint| hint.contains("existing directory")),
            "recovery hint should mention using an existing directory: {tool_error:?}"
        );
    }

    #[tokio::test]
    async fn resolve_command_task_accepts_workdir_outside_execution_root() {
        let (_home, workspace, runtime) = test_runtime();
        // Create a directory outside the workspace root
        let outside_dir = workspace.path().parent().unwrap().join("outside_cwd");
        std::fs::create_dir_all(&outside_dir).unwrap();

        let mut spec = command_spec(false, false);
        spec.workdir = Some(outside_dir.to_string_lossy().into_owned());

        let resolved = resolved_command(&runtime, &spec).await;
        // The resolved workdir should point to the external directory, not the workspace root
        let resolved_canonical = std::fs::canonicalize(&resolved.workdir).unwrap();
        let outside_canonical = std::fs::canonicalize(&outside_dir).unwrap();
        assert_eq!(
            resolved_canonical, outside_canonical,
            "workdir outside execution_root should be accepted and resolve to the requested directory"
        );
        assert_ne!(
            resolved.workdir,
            workspace.path(),
            "workdir should not fall back to workspace root"
        );
    }
}
